//! Overlap minimization ("separation"), adapted from sparrow by Jeroen Gardeyn (MIT,
//! https://github.com/JeroenGar/sparrow, algorithms from https://doi.org/10.48550/arXiv.2509.13329).
//!
//! Differences from sparrow:
//! - Works on a plain jagua `Layout`, so the container can be any simple polygon with holes
//!   (remnants, defects, and the temporary "wall" used for compression).
//! - Collisions with the container exterior are quantified with pole penetration depth against
//!   the actual outline instead of the bounding box.
//! - Collisions with holes are quantified like item-item collisions.
//! - Mirrored items keep their reflection while they move.
//! - Single-threaded; the engine already runs one search per core.
//!
//! sparrow's license:
//!
//! MIT License
//!
//! Copyright (c) 2025 Jeroen Gardeyn, KU Leuven
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use jagua_rs::collision_detection::hazards::collector::{BasicHazardCollector, HazardCollector};
use jagua_rs::collision_detection::hazards::filter::NoFilter;
use jagua_rs::collision_detection::hazards::{BasicHazardEntity, HazKey};
use jagua_rs::entities::{Item, Layout, LayoutSnapshot, PItemKey};
use jagua_rs::geometry::DTransformation;
use jagua_rs::geometry::Transformation;
use jagua_rs::geometry::geo_enums::RotationRange;
use jagua_rs::geometry::geo_traits::TransformableFrom;
use jagua_rs::geometry::primitives::{Point, Rect, SPolygon};
use rand::prelude::*;
use slotmap::SecondaryMap;
use std::cmp::Ordering;
use std::f32::consts::PI;
use std::time::Instant;

const GLS_WEIGHT_MAX_INC_RATIO: f32 = 2.0;
const GLS_WEIGHT_MIN_INC_RATIO: f32 = 1.2;
const GLS_WEIGHT_DECAY: f32 = 0.95;
const OVERLAP_PROXY_EPSILON_DIAM_RATIO: f32 = 0.01;
const CD_STEP_SUCCESS: f32 = 1.1;
const CD_STEP_FAIL: f32 = 0.5;
const PRE_REFINE_CD_TL_RATIOS: (f32, f32) = (0.25, 0.02);
const PRE_REFINE_CD_R_STEPS: (f32, f32) = (5.0 * PI / 180.0, 1.0 * PI / 180.0);
const SND_REFINE_CD_TL_RATIOS: (f32, f32) = (0.01, 0.001);
const SND_REFINE_CD_R_STEPS: (f32, f32) = (0.5 * PI / 180.0, 0.05 * PI / 180.0);
const UNIQUE_SAMPLE_THRESHOLD: f32 = 0.05;
const ROT_N_SAMPLES: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct SepConfig {
    pub iter_no_imprv_limit: usize,
    pub strike_limit: usize,
    pub n_container_samples: usize,
    pub n_focussed_samples: usize,
    pub n_coord_descents: usize,
}

pub const SEP_CONFIG: SepConfig = SepConfig {
    iter_no_imprv_limit: 100,
    strike_limit: 4,
    n_container_samples: 50,
    n_focussed_samples: 25,
    n_coord_descents: 3,
};

// ---------------------------------------------------------------------------------------------
// Collision quantification

fn overlap_area_proxy(s1: &SPolygon, s2: &SPolygon, epsilon: f32) -> f32 {
    let mut total = 0.0;
    for p1 in &s1.surrogate().poles {
        for p2 in &s2.surrogate().poles {
            let d =
                ((p1.center.0 - p2.center.0).powi(2) + (p1.center.1 - p2.center.1).powi(2)).sqrt();
            let pd = (p1.radius + p2.radius) - d;
            let pd_decay = if pd >= epsilon {
                pd
            } else {
                epsilon * epsilon / (-pd + 2.0 * epsilon)
            };
            total += pd_decay * p1.radius.min(p2.radius);
        }
    }
    total * PI
}

fn shape_penalty(s1: &SPolygon, s2: &SPolygon) -> f32 {
    let p1 = s1.surrogate().convex_hull_area.sqrt();
    let p2 = s2.surrogate().convex_hull_area.sqrt();
    (p1 * p2).sqrt()
}

fn quantify_pp(s1: &SPolygon, s2: &SPolygon) -> f32 {
    let eps = s1.diameter().max(s2.diameter()) * OVERLAP_PROXY_EPSILON_DIAM_RATIO;
    let proxy = overlap_area_proxy(s1, s2, eps) + eps * eps;
    proxy.sqrt() * shape_penalty(s1, s2)
}

/// Signed distance from `p` to the boundary of `poly`: positive inside, negative outside.
fn signed_dist(p: Point, poly: &SPolygon) -> f32 {
    let v = poly.vertices();
    let n = v.len();
    let mut inside = false;
    let mut best = f32::INFINITY;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (v[j], v[i]);
        if (b.1 > p.1) != (a.1 > p.1) && p.0 < (a.0 - b.0) * (p.1 - b.1) / (a.1 - b.1) + b.0 {
            inside = !inside;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (cx, cy) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
        best = best.min(cx * cx + cy * cy);
        j = i;
    }
    let d = best.sqrt();
    if inside { d } else { -d }
}

/// How far `s` pokes out of the container outline, from the penetration depth of its poles.
fn quantify_exterior(s: &SPolygon, outer: &SPolygon) -> f32 {
    let eps = s.diameter() * OVERLAP_PROXY_EPSILON_DIAM_RATIO;
    let mut total = 0.0;
    for pole in &s.surrogate().poles {
        let pd = pole.radius - signed_dist(pole.center, outer);
        let pd_decay = if pd >= eps {
            pd
        } else {
            eps * eps / (-pd + 2.0 * eps)
        };
        total += pd_decay * pole.radius;
    }
    let overlap = total * PI + eps * eps;
    2.0 * overlap.sqrt() * shape_penalty(s, s)
}

fn hole_shape(l: &Layout, idx: usize) -> &SPolygon {
    l.container().quality_zones()[0]
        .as_ref()
        .expect("hole hazard without zone")
        .shapes_cd()[idx]
        .as_ref()
}

// ---------------------------------------------------------------------------------------------
// Collision tracker (pairwise losses and GLS weights)

#[derive(Clone, Copy, Debug)]
struct Entry {
    loss: f32,
    weight: f32,
}

impl Entry {
    fn wl(&self) -> f32 {
        self.loss * self.weight
    }
}

const FRESH: Entry = Entry {
    loss: 0.0,
    weight: 1.0,
};

#[derive(Clone, Debug)]
struct Tracker {
    size: usize,
    idx: SecondaryMap<PItemKey, usize>,
    pairs: Vec<Entry>,
    statics: Vec<Entry>,
}

fn tri(r: usize, c: usize, n: usize) -> usize {
    if r <= c {
        r * n + c - r * (r + 1) / 2
    } else {
        c * n + r - c * (c + 1) / 2
    }
}

impl Tracker {
    fn new(l: &Layout) -> Tracker {
        let size = l.placed_items().len();
        let mut t = Tracker {
            size,
            idx: l
                .placed_items()
                .keys()
                .enumerate()
                .map(|(i, k)| (k, i))
                .collect(),
            pairs: vec![FRESH; size * (size + 1) / 2],
            statics: vec![FRESH; size],
        };
        for k in l.placed_items().keys() {
            t.recompute(k, l);
        }
        t
    }

    fn recompute(&mut self, pk: PItemKey, l: &Layout) {
        let i = self.idx[pk];
        for j in 0..self.size {
            self.pairs[tri(i, j, self.size)].loss = 0.0;
        }
        self.statics[i].loss = 0.0;
        let pi = &l.placed_items()[pk];
        let shape = pi.shape();
        let mut col = BasicHazardCollector::with_capacity(l.placed_items().len() + 1);
        l.cde().collect_poly_collisions(shape, &NoFilter, &mut col);
        col.remove_by_entity(&BasicHazardEntity::from((pk, pi)));
        for (_, haz) in col.iter() {
            match haz {
                BasicHazardEntity::PlacedItem { pk: other, .. } => {
                    let j = self.idx[*other];
                    self.pairs[tri(i, j, self.size)].loss =
                        quantify_pp(shape, l.placed_items()[*other].shape());
                }
                BasicHazardEntity::Exterior => {
                    self.statics[i].loss += quantify_exterior(shape, l.container().outer_cd());
                }
                BasicHazardEntity::Hole { idx } => {
                    self.statics[i].loss += quantify_pp(shape, hole_shape(l, *idx));
                }
                BasicHazardEntity::InferiorQualityZone { .. } => {}
            }
        }
    }

    fn moved(&mut self, l: &Layout, old: PItemKey, new: PItemKey) {
        let i = self.idx.remove(old).unwrap();
        self.idx.insert(new, i);
        self.recompute(new, l);
    }

    fn restore_keep_weights(&mut self, s: &Tracker) {
        self.idx = s.idx.clone();
        for (a, b) in self.pairs.iter_mut().zip(&s.pairs) {
            a.loss = b.loss;
        }
        for (a, b) in self.statics.iter_mut().zip(&s.statics) {
            a.loss = b.loss;
        }
    }

    fn update_weights(&mut self) {
        let max = self
            .pairs
            .iter()
            .chain(&self.statics)
            .map(|e| e.loss)
            .fold(0.0, f32::max);
        for e in self.pairs.iter_mut().chain(self.statics.iter_mut()) {
            let m = if e.loss == 0.0 {
                GLS_WEIGHT_DECAY
            } else {
                GLS_WEIGHT_MIN_INC_RATIO
                    + (GLS_WEIGHT_MAX_INC_RATIO - GLS_WEIGHT_MIN_INC_RATIO) * (e.loss / max)
            };
            e.weight = (e.weight * m).max(1.0);
        }
    }

    fn pair_weight(&self, a: PItemKey, b: PItemKey) -> f32 {
        self.pairs[tri(self.idx[a], self.idx[b], self.size)].weight
    }

    fn static_weight(&self, a: PItemKey) -> f32 {
        self.statics[self.idx[a]].weight
    }

    fn loss(&self, pk: PItemKey) -> f32 {
        let i = self.idx[pk];
        self.statics[i].loss
            + (0..self.size)
                .map(|j| self.pairs[tri(i, j, self.size)].loss)
                .sum::<f32>()
    }

    fn total_loss(&self) -> f32 {
        self.statics.iter().chain(&self.pairs).map(|e| e.loss).sum()
    }

    fn total_wloss(&self) -> f32 {
        self.statics.iter().chain(&self.pairs).map(Entry::wl).sum()
    }
}

// ---------------------------------------------------------------------------------------------
// Sample evaluation

#[derive(Clone, Copy, Debug, PartialEq)]
enum Eval {
    Clear,
    Collision(f32),
    Invalid,
}

impl Eval {
    fn rank(&self) -> (u8, f32) {
        match self {
            Eval::Clear => (0, 0.0),
            Eval::Collision(l) => (1, *l),
            Eval::Invalid => (2, 0.0),
        }
    }
    fn cmp(&self, o: &Eval) -> Ordering {
        let (a, b) = (self.rank(), o.rank());
        a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
    }
    fn lt(&self, o: &Eval) -> bool {
        self.cmp(o) == Ordering::Less
    }
}

struct Evaluator<'a> {
    l: &'a Layout,
    item: &'a Item,
    ct: &'a Tracker,
    pk: PItemKey,
    own: (HazKey, BasicHazardEntity),
    col: BasicHazardCollector,
    buf: SPolygon,
    n: usize,
}

impl<'a> Evaluator<'a> {
    fn new(l: &'a Layout, item: &'a Item, pk: PItemKey, ct: &'a Tracker) -> Self {
        let ent = BasicHazardEntity::from((pk, &l.placed_items()[pk]));
        let key = l.cde().haz_key(&ent).expect("placed item registered");
        Evaluator {
            l,
            item,
            ct,
            pk,
            own: (key, ent),
            col: BasicHazardCollector::with_capacity(l.placed_items().len() + 1),
            buf: item.shape_cd().as_ref().clone(),
            n: 0,
        }
    }

    fn eval(&mut self, dt: DTransformation, bound: Eval) -> Eval {
        self.n += 1;
        let loss_bound = match bound {
            Eval::Collision(l) => l,
            Eval::Clear => 0.0,
            Eval::Invalid => f32::INFINITY,
        };
        let shape = self
            .buf
            .transform_from(self.item.shape_cd().as_ref(), &dt.compose());
        self.col.clear();
        self.col.insert(self.own.0, self.own.1);
        let n_ignored = self.col.len();
        let mut loss = 0.0f32;
        let (l, ct, pk) = (self.l, self.ct, self.pk);
        let weighted = |haz: BasicHazardEntity, shape: &SPolygon| -> f32 {
            match haz {
                BasicHazardEntity::PlacedItem { pk: other, .. } => {
                    quantify_pp(shape, l.placed_items()[other].shape()) * ct.pair_weight(pk, other)
                }
                BasicHazardEntity::Exterior => {
                    quantify_exterior(shape, l.container().outer_cd()) * ct.static_weight(pk)
                }
                BasicHazardEntity::Hole { idx } => {
                    quantify_pp(shape, hole_shape(l, idx)) * ct.static_weight(pk)
                }
                BasicHazardEntity::InferiorQualityZone { .. } => 0.0,
            }
        };
        let shape_ref: &SPolygon = shape;
        let mut stop = |haz: BasicHazardEntity| {
            loss += weighted(haz, shape_ref);
            loss > loss_bound
        };
        let cde = self.l.cde();
        if cde.collect_surrogate_collisions_until(shape_ref, &NoFilter, &mut self.col, &mut stop) {
            return Eval::Invalid;
        }
        if cde.collect_poly_collisions_until(shape_ref, &NoFilter, &mut self.col, &mut stop) {
            return Eval::Invalid;
        }
        if self.col.len() == n_ignored {
            Eval::Clear
        } else {
            Eval::Collision(loss)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sampling

fn rotations_for(item: &Item, reflected: bool) -> Vec<f32> {
    let ao = item.allowed_orientations();
    let r = if reflected {
        ao.rotations_after_reflection().unwrap_or(ao.rotations())
    } else {
        ao.rotations()
    };
    match r {
        RotationRange::None => vec![0.0],
        RotationRange::Discrete(v) => v.clone(),
        RotationRange::Continuous => (0..ROT_N_SAMPLES)
            .map(|i| i as f32 * 2.0 * PI / ROT_N_SAMPLES as f32)
            .collect(),
    }
}

fn wiggle(item: &Item, reflected: bool) -> bool {
    let ao = item.allowed_orientations();
    let r = if reflected {
        ao.rotations_after_reflection().unwrap_or(ao.rotations())
    } else {
        ao.rotations()
    };
    matches!(r, RotationRange::Continuous)
}

/// A rotation and the x and y ranges where the rotated item stays inside the container box.
type RotEntry = (f32, (f32, f32), (f32, f32));

struct BBoxSampler {
    entries: Vec<RotEntry>,
    reflected: bool,
}

impl BBoxSampler {
    fn new(sample: Rect, item: &Item, cont: Rect, reflected: bool) -> Option<BBoxSampler> {
        let mut buf = item.shape_cd().as_ref().clone();
        let entries: Vec<_> = rotations_for(item, reflected)
            .into_iter()
            .filter_map(|r| {
                let t = DTransformation::new(r, (0.0, 0.0))
                    .with_reflection(reflected)
                    .compose();
                let b = buf.transform_from(item.shape_cd().as_ref(), &t).bbox();
                let x = (
                    (cont.x_min - b.x_min).max(sample.x_min),
                    (cont.x_max - b.x_max).min(sample.x_max),
                );
                let y = (
                    (cont.y_min - b.y_min).max(sample.y_min),
                    (cont.y_max - b.y_max).min(sample.y_max),
                );
                (x.0 <= x.1 && y.0 <= y.1).then_some((r, x, y))
            })
            .collect();
        (!entries.is_empty()).then_some(BBoxSampler { entries, reflected })
    }

    fn sample(&self, rng: &mut impl Rng) -> DTransformation {
        let (r, x, y) = *self.entries.choose(rng).unwrap();
        let sx = if x.0 == x.1 {
            x.0
        } else {
            rng.random_range(x.0..x.1)
        };
        let sy = if y.0 == y.1 {
            y.0
        } else {
            rng.random_range(y.0..y.1)
        };
        DTransformation::new(r, (sx, sy)).with_reflection(self.reflected)
    }
}

fn similar(a: DTransformation, b: DTransformation, th: f32) -> bool {
    let (ax, ay) = a.translation();
    let (bx, by) = b.translation();
    (ax - bx).abs() < th
        && (ay - by).abs() < th
        && a.reflected == b.reflected
        && ((a.rotation() % (2.0 * PI)) - (b.rotation() % (2.0 * PI))).abs() < 1f32.to_radians()
}

struct Best {
    size: usize,
    v: Vec<(DTransformation, Eval)>,
    th: f32,
}

impl Best {
    fn upper(&self) -> Eval {
        self.v.get(self.size - 1).map_or(Eval::Invalid, |s| s.1)
    }
    fn report(&mut self, dt: DTransformation, e: Eval) {
        if !e.lt(&self.upper()) {
            return;
        }
        let th = self.th;
        let sims: Vec<Eval> = self
            .v
            .iter()
            .filter(|(d, _)| similar(*d, dt, th))
            .map(|s| s.1)
            .collect();
        if sims.is_empty() {
            if self.v.len() == self.size {
                self.v.pop();
            }
        } else if sims.iter().all(|s| e.lt(s)) {
            self.v.retain(|(d, _)| !similar(*d, dt, th));
        } else {
            return;
        }
        self.v.push((dt, e));
        self.v.sort_by(|a, b| a.1.cmp(&b.1));
    }
}

struct Cd {
    t_init: f32,
    t_limit: f32,
    r_init: f32,
    r_limit: f32,
    wiggle: bool,
}

fn coord_descent(
    start: (DTransformation, Eval),
    ev: &mut Evaluator,
    cfg: Cd,
    rng: &mut impl Rng,
) -> (DTransformation, Eval) {
    let (mut pos, mut eval) = start;
    let (mut sx, mut sy, mut sr) = (cfg.t_init, cfg.t_init, cfg.r_init);
    let axes = if cfg.wiggle { 6 } else { 4 };
    let mut axis = rng.random_range(0..axes);
    let mut guard = 0;
    while (sx >= cfg.t_limit || sy >= cfg.t_limit || (cfg.wiggle && sr >= cfg.r_limit))
        && guard < 1000
    {
        guard += 1;
        let (tx, ty) = pos.translation();
        let r = pos.rotation();
        let c = match axis {
            0 => [(tx + sx, ty, r), (tx - sx, ty, r)],
            1 => [(tx, ty + sy, r), (tx, ty - sy, r)],
            2 => [(tx + sx, ty + sy, r), (tx - sx, ty - sy, r)],
            3 => [(tx - sx, ty + sy, r), (tx + sx, ty - sy, r)],
            _ => [(tx, ty, r + sr), (tx, ty, r - sr)],
        }
        .map(|(x, y, r)| DTransformation::new(r, (x, y)).with_reflection(pos.reflected));
        let e = c.map(|d| ev.eval(d, eval));
        let (bd, be) = if e[0].cmp(&e[1]) != Ordering::Greater {
            (c[0], e[0])
        } else {
            (c[1], e[1])
        };
        let ord = be.cmp(&eval);
        if ord != Ordering::Greater {
            pos = bd;
            eval = be;
        }
        let better = ord == Ordering::Less;
        let m = if better {
            CD_STEP_SUCCESS
        } else {
            CD_STEP_FAIL
        };
        match axis {
            0 => sx *= m,
            1 => sy *= m,
            2 | 3 => {
                sx *= m.sqrt();
                sy *= m.sqrt();
            }
            _ => sr *= m,
        }
        if !better {
            axis = rng.random_range(0..axes);
        }
    }
    (pos, eval)
}

fn search_placement(
    l: &Layout,
    item: &Item,
    pk: PItemKey,
    ev: &mut Evaluator,
    cfg: &SepConfig,
    rng: &mut impl Rng,
) -> (DTransformation, Eval) {
    let min_dim = item
        .shape_cd()
        .bbox()
        .width()
        .min(item.shape_cd().bbox().height());
    let mut best = Best {
        size: cfg.n_coord_descents,
        v: vec![],
        th: min_dim * UNIQUE_SAMPLE_THRESHOLD,
    };
    let cur = l.placed_items()[pk].d_transf();
    let reflected = cur.reflected;
    let cur_eval = ev.eval(cur, best.upper());
    best.report(cur, cur_eval);
    let cont = l.container().outer_cd().bbox();
    if let Some(s) = BBoxSampler::new(l.placed_items()[pk].shape().bbox(), item, cont, reflected) {
        for _ in 0..cfg.n_focussed_samples {
            let dt = s.sample(rng);
            let e = ev.eval(dt, best.upper());
            best.report(dt, e);
        }
    }
    if let Some(s) = BBoxSampler::new(cont, item, cont, reflected) {
        for _ in 0..cfg.n_container_samples {
            let dt = s.sample(rng);
            let e = ev.eval(dt, best.upper());
            best.report(dt, e);
        }
    }
    let w = wiggle(item, reflected);
    for start in best.v.clone() {
        let cfg1 = Cd {
            t_init: min_dim * PRE_REFINE_CD_TL_RATIOS.0,
            t_limit: min_dim * PRE_REFINE_CD_TL_RATIOS.1,
            r_init: PRE_REFINE_CD_R_STEPS.0,
            r_limit: PRE_REFINE_CD_R_STEPS.1,
            wiggle: w,
        };
        let d = coord_descent(start, ev, cfg1, rng);
        best.report(d.0, d.1);
    }
    let start = best.v.first().copied().unwrap_or((cur, cur_eval));
    let cfg2 = Cd {
        t_init: min_dim * SND_REFINE_CD_TL_RATIOS.0,
        t_limit: min_dim * SND_REFINE_CD_TL_RATIOS.1,
        r_init: SND_REFINE_CD_R_STEPS.0,
        r_limit: SND_REFINE_CD_R_STEPS.1,
        wiggle: w,
    };
    coord_descent(start, ev, cfg2, rng)
}

// ---------------------------------------------------------------------------------------------
// Separator

pub struct Separator {
    pub evals: usize,
    pub layout: Layout,
    ct: Tracker,
    cfg: SepConfig,
}

impl Separator {
    pub fn new(layout: Layout, cfg: SepConfig) -> Separator {
        let ct = Tracker::new(&layout);
        Separator {
            evals: 0,
            layout,
            ct,
            cfg,
        }
    }

    fn move_item(&mut self, pk: PItemKey, dt: DTransformation) {
        let pi = self.layout.remove_item(pk);
        let item = pi.item().clone();
        let new = self.layout.place_item(&item, dt);
        self.ct.moved(&self.layout, pk, new);
    }

    fn move_items(&mut self, rng: &mut impl Rng) {
        let mut cands: Vec<PItemKey> = self
            .layout
            .placed_items()
            .keys()
            .filter(|k| self.ct.loss(*k) > 0.0)
            .collect();
        cands.shuffle(rng);
        for pk in cands {
            if self.ct.loss(pk) <= 0.0 {
                continue;
            }
            let item = self.layout.placed_items()[pk].item().clone();
            let (dt, _) = {
                let mut ev = Evaluator::new(&self.layout, &item, pk, &self.ct);
                let r = search_placement(&self.layout, &item, pk, &mut ev, &self.cfg, rng);
                self.evals += ev.n;
                r
            };
            self.move_item(pk, dt);
        }
    }

    fn snapshot(&self) -> (LayoutSnapshot, Tracker) {
        (self.layout.save(), self.ct.clone())
    }

    fn rollback(&mut self, s: &(LayoutSnapshot, Tracker)) {
        self.layout.restore(&s.0).expect("same container");
        self.ct.restore_keep_weights(&s.1);
    }

    /// Minimizes overlap until the layout is feasible or the search gives up. Leaves the layout in
    /// its least-overlapping state. Returns true when feasible.
    pub fn separate(
        &mut self,
        deadline: Instant,
        cancel: &dyn Fn() -> bool,
        rng: &mut impl Rng,
    ) -> bool {
        let t0 = Instant::now();
        let mut best = self.snapshot();
        let mut min_loss = self.ct.total_loss();
        if min_loss == 0.0 {
            return true;
        }
        let mut strikes = 0;
        'outer: while strikes < self.cfg.strike_limit {
            let mut no_imp = 0;
            let init = self.ct.total_loss();
            while no_imp < self.cfg.iter_no_imprv_limit {
                if Instant::now() >= deadline || cancel() {
                    break 'outer;
                }
                let before = self.ct.total_wloss();
                self.move_items(rng);
                debug_assert!(self.ct.total_wloss() <= before * 1.001 + 1e-3);
                let loss = self.ct.total_loss();
                if loss == 0.0 {
                    best = self.snapshot();
                    min_loss = 0.0;
                    break 'outer;
                } else if loss < min_loss {
                    if loss < min_loss * 0.98 {
                        no_imp = 0;
                    }
                    best = self.snapshot();
                    min_loss = loss;
                } else {
                    no_imp += 1;
                }
                self.ct.update_weights();
            }
            if init * 0.98 <= min_loss {
                strikes += 1;
            } else {
                strikes = 0;
            }
            self.rollback(&best);
        }
        self.rollback(&best);
        log::trace!(
            "sep: {} items, {} evals, {:.0} ms, feasible {}",
            self.layout.placed_items().len(),
            self.evals,
            t0.elapsed().as_secs_f64() * 1000.0,
            min_loss == 0.0
        );
        min_loss == 0.0
    }
}

/// A random transformation that keeps `item` inside `cont`'s box, for inserting a new item.
pub fn random_dt(item: &Item, cont: Rect, rng: &mut impl Rng) -> Option<DTransformation> {
    let reflected = item
        .allowed_orientations()
        .rotations_after_reflection()
        .is_some()
        && rng.random_bool(0.5);
    BBoxSampler::new(cont, item, cont, reflected).map(|s| s.sample(rng))
}

/// Translates `dt` by (dx, dy) in container coordinates.
pub fn shifted(dt: DTransformation, dx: f32, dy: f32) -> DTransformation {
    let t: Transformation = dt.compose().translate((dx, dy));
    t.decompose()
}
