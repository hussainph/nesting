//! Finds a collision-free spot for one item in one layout.
//!
//! Uniform sampling over the container followed by a local search that shrinks around the best
//! sample, then a short "slide" that pushes the item left and down. Loss is a weighted
//! left-bottom gravity, so parts pack toward the left edge and leave one contiguous offcut on the
//! right. The sampling scheme follows jagua-rs's reference LBF optimizer (MPL-2.0).

use jagua_rs::collision_detection::CDEngine;
use jagua_rs::collision_detection::hazards::filter::NoFilter;
use jagua_rs::entities::Item;
use jagua_rs::geometry::DTransformation;
use jagua_rs::geometry::geo_enums::RotationRange;
use jagua_rs::geometry::geo_traits::TransformableFrom;
use jagua_rs::geometry::primitives::{Rect, SPolygon};
use rand::prelude::*;
use rand_distr::Normal;
use std::f32::consts::PI;

const X_WEIGHT: f32 = 10.0;

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub uniform: usize,
    pub local: usize,
}

impl Budget {
    pub const CONSTRUCT: Budget = Budget {
        uniform: 1500,
        local: 600,
    };
    pub const REPAIR: Budget = Budget {
        uniform: 600,
        local: 300,
    };
}

/// Loss of a placed shape: lower is better.
pub fn loss(shape: &SPolygon, origin: (f32, f32)) -> f32 {
    let b = shape.bbox();
    (b.x_max - origin.0) * X_WEIGHT + (b.y_max - origin.1)
}

fn sample_rotation(item: &Item, reflected: bool, rng: &mut impl Rng) -> f32 {
    let ao = item.allowed_orientations();
    let range = if reflected {
        ao.rotations_after_reflection().unwrap_or(ao.rotations())
    } else {
        ao.rotations()
    };
    match range {
        RotationRange::None => 0.0,
        RotationRange::Continuous => rng.random_range(0.0..2.0 * PI),
        RotationRange::Discrete(a) => *a.choose(rng).unwrap_or(&0.0),
    }
}

fn can_reflect(item: &Item) -> bool {
    item.allowed_orientations()
        .rotations_after_reflection()
        .is_some()
}

fn is_continuous(item: &Item, reflected: bool) -> bool {
    let ao = item.allowed_orientations();
    let r = if reflected {
        ao.rotations_after_reflection().unwrap_or(ao.rotations())
    } else {
        ao.rotations()
    };
    matches!(r, RotationRange::Continuous)
}

struct Eval<'a> {
    cde: &'a CDEngine,
    item: &'a Item,
    buffer: SPolygon,
    origin: (f32, f32),
}

impl Eval<'_> {
    /// Loss when `dt` is a valid placement that beats `bound`, else `None`.
    fn try_place(&mut self, dt: &DTransformation, bound: f32) -> Option<f32> {
        let t = dt.compose();
        let surrogate = self.item.shape_cd().surrogate();
        if self
            .cde
            .detect_surrogate_collision(surrogate, &t, &NoFilter)
        {
            return None;
        }
        self.buffer.transform_from(self.item.shape_cd(), &t);
        let l = loss(&self.buffer, self.origin);
        if l >= bound {
            return None;
        }
        if self.cde.detect_poly_collision(&self.buffer, &NoFilter) {
            return None;
        }
        Some(l)
    }
}

/// Searches `cde` for the lowest-loss valid placement of `item`.
pub fn search(
    cde: &CDEngine,
    item: &Item,
    budget: Budget,
    rng: &mut impl Rng,
) -> Option<(DTransformation, f32)> {
    let bbox: Rect = cde.bbox();
    let origin = (bbox.x_min, bbox.y_min);
    let mut ev = Eval {
        cde,
        item,
        buffer: item.shape_cd().as_ref().clone(),
        origin,
    };
    let reflect = can_reflect(item);

    // The item's reference point is its centroid; keep samples where the item can still fit.
    let shape_bbox = item.shape_cd().bbox();
    let r = (shape_bbox.width().max(shape_bbox.height())) * 0.5;
    let (mut x0, mut x1, y0, y1) = (bbox.x_min, bbox.x_max, bbox.y_min, bbox.y_max);
    if x1 - x0 <= 0.0 || y1 - y0 <= 0.0 {
        return None;
    }

    let mut best: Option<(DTransformation, f32)> = None;
    for _ in 0..budget.uniform {
        let reflected = reflect && rng.random_bool(0.5);
        let rot = sample_rotation(item, reflected, rng);
        let x = rng.random_range(x0..x1);
        let y = rng.random_range(y0..y1);
        let dt = DTransformation::new(rot, (x, y)).with_reflection(reflected);
        let bound = best.map_or(f32::INFINITY, |b| b.1);
        if let Some(l) = ev.try_place(&dt, bound) {
            best = Some((dt, l));
            // Samples whose centroid is right of this bound can't beat it.
            let x_bound = origin.0 + l / X_WEIGHT + r;
            if x_bound < x1 {
                x1 = x_bound.max(x0 + 1e-3);
            }
        }
    }
    let (mut best_dt, mut best_l) = best?;
    let _ = &mut x0;

    // Local search around the best sample with shrinking normal distributions.
    let max_dim = bbox.width().max(bbox.height());
    let (sd_t0, sd_t1) = (0.01 * max_dim, 0.0005 * max_dim);
    let (sd_r0, sd_r1) = (2.0f32.to_radians(), 0.25f32.to_radians());
    for i in 0..budget.local {
        let p = i as f32 / budget.local.max(1) as f32;
        let sd_t = sd_t0 * (sd_t1 / sd_t0).powf(p);
        let sd_r = sd_r0 * (sd_r1 / sd_r0).powf(p);
        let (tx, ty) = best_dt.translation();
        let x = Normal::new(tx, sd_t).unwrap().sample(rng);
        let y = Normal::new(ty, sd_t).unwrap().sample(rng);
        let rot = if is_continuous(item, best_dt.reflected) {
            Normal::new(best_dt.rotation(), sd_r).unwrap().sample(rng)
        } else {
            best_dt.rotation()
        };
        let dt = DTransformation::new(rot, (x, y)).with_reflection(best_dt.reflected);
        if let Some(l) = ev.try_place(&dt, best_l) {
            best_dt = dt;
            best_l = l;
        }
    }

    // Slide: push left, then down, with a shrinking step, until stuck.
    let mut step = 0.02 * max_dim;
    let min_step = 0.0005 * max_dim.max(1.0);
    while step > min_step {
        let mut moved = false;
        for (dx, dy) in [(-step, 0.0), (0.0, -step), (-step, -step)] {
            let (tx, ty) = best_dt.translation();
            let dt = DTransformation::new(best_dt.rotation(), (tx + dx, ty + dy))
                .with_reflection(best_dt.reflected);
            if let Some(l) = ev.try_place(&dt, best_l) {
                best_dt = dt;
                best_l = l;
                moved = true;
            }
        }
        if !moved {
            step *= 0.5;
        }
    }

    Some((best_dt, best_l))
}
