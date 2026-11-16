//! Solution state, objective, construction and ruin-and-recreate improvement.

use crate::place::{Budget, search};
use crate::prep::Prepared;
use crate::prep::container_from_ring;
use crate::sep::{SEP_CONFIG, Separator, random_dt, shifted};
use jagua_rs::entities::{Layout, PItemKey};
use kurbo::Point;
use rand::prelude::*;
use std::cmp::Ordering;
use std::time::Instant;

#[derive(Clone)]
pub struct Sheet {
    pub bin: usize,
    pub layout: Layout,
}

#[derive(Clone)]
pub struct Sol {
    pub sheets: Vec<Sheet>,
    /// Copies still to place, per item type.
    pub missing: Vec<usize>,
    /// Copies of each bin type in use.
    pub used: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Score {
    pub placed: f64,
    pub cost: f64,
    pub compact: f64,
}

impl Score {
    /// Lexicographic: more placed area, then cheaper stock, then less fresh material consumed
    /// (remnants marked free count only lightly, to keep them tidy).
    pub fn cmp(&self, other: &Score, total_area: f64) -> Ordering {
        let eps_a = total_area * 1e-7;
        if (self.placed - other.placed).abs() > eps_a {
            return other.placed.total_cmp(&self.placed);
        }
        if (self.cost - other.cost).abs() > 1e-6 * self.cost.max(other.cost).max(1.0) {
            return self.cost.total_cmp(&other.cost);
        }
        self.compact.total_cmp(&other.compact)
    }
}

impl Sol {
    pub fn empty(p: &Prepared) -> Sol {
        Sol {
            sheets: Vec::new(),
            missing: p.items.iter().map(|i| i.demand).collect(),
            used: vec![0; p.bins.len()],
        }
    }

    pub fn score(&self, p: &Prepared) -> Score {
        let mut placed = 0.0;
        let mut cost = 0.0;
        let mut compact = 0.0;
        // Normalized so one fully used fresh sheet of the largest size counts about 1.
        let max_area = p.bins.iter().map(|b| b.area).fold(1.0, f64::max);
        for sh in &self.sheets {
            cost += p.bins[sh.bin].cost;
            let cb = sh.layout.container().outer_cd().bbox();
            let (w, h) = (cb.width().max(1e-3), cb.height().max(1e-3));
            let mut x_max = cb.x_min;
            let mut y_max = cb.y_min;
            for pi in sh.layout.placed_items().values() {
                placed += p.items[pi.item().idx()].area;
                let b = pi.shape().bbox();
                x_max = x_max.max(b.x_max);
                y_max = y_max.max(b.y_max);
            }
            let frac = ((x_max - cb.x_min) / w + 0.1 * (y_max - cb.y_min) / h) as f64;
            let bin = &p.bins[sh.bin];
            compact += frac * (bin.area / max_area) * if bin.free { 1e-3 } else { 1.0 };
        }
        Score {
            placed,
            cost,
            compact,
        }
    }

    fn remove(&mut self, sheet: usize, key: PItemKey) {
        let pi = self.sheets[sheet].layout.remove_item(key);
        self.missing[pi.item().idx()] += 1;
    }

    fn drop_empty_sheets(&mut self) {
        let used = &mut self.used;
        self.sheets.retain(|s| {
            let keep = !s.layout.placed_items().is_empty();
            if !keep {
                used[s.bin] -= 1;
            }
            keep
        });
    }

    /// Places `missing` copies, biggest first with some noise, into open sheets (first fit) and
    /// then into new bins in `bin_order`.
    pub fn recreate(
        &mut self,
        p: &Prepared,
        bin_order: &[usize],
        budget: Budget,
        rng: &mut impl Rng,
    ) {
        let mut queue: Vec<usize> = Vec::new();
        for (i, &n) in self.missing.iter().enumerate() {
            queue.extend(std::iter::repeat_n(i, n));
        }
        queue.sort_by(|&a, &b| p.items[b].area.total_cmp(&p.items[a].area));
        for i in 1..queue.len() {
            if rng.random_bool(0.12) {
                queue.swap(i - 1, i);
            }
        }
        let mut failed = vec![false; p.items.len()];
        for it in queue {
            if failed[it] || self.missing[it] == 0 {
                continue;
            }
            let item = &p.items[it].item;
            let mut done = false;
            for sh in self.sheets.iter_mut() {
                if let Some((dt, _)) = search(sh.layout.cde(), item, budget, rng) {
                    sh.layout.place_item(item, dt);
                    done = true;
                    break;
                }
            }
            if !done {
                for &b in bin_order {
                    if self.used[b] >= p.bins[b].qty {
                        continue;
                    }
                    let cde = p.bins[b].container.base_cde();
                    if let Some((dt, _)) = search(cde, item, budget, rng) {
                        let mut layout = Layout::new(p.bins[b].container.clone());
                        layout.place_item(item, dt);
                        self.sheets.push(Sheet { bin: b, layout });
                        self.used[b] += 1;
                        done = true;
                        break;
                    }
                }
            }
            if done {
                self.missing[it] -= 1;
            } else {
                failed[it] = true;
            }
        }
    }

    /// Right edge of the parts on a sheet, in container coordinates.
    pub fn extent(&self, s: usize) -> f32 {
        let sh = &self.sheets[s];
        let x0 = sh.layout.container().outer_cd().bbox().x_min;
        sh.layout
            .placed_items()
            .values()
            .map(|pi| pi.shape().bbox().x_max)
            .fold(x0, f32::max)
    }

    /// Tries to pull every part on sheet `s` left of a wall `step` (a fraction of the used
    /// length) inside the current extent. Overlaps are resolved by separation.
    pub fn compress(
        &mut self,
        p: &Prepared,
        s: usize,
        step: f32,
        deadline: Instant,
        cancel: &dyn Fn() -> bool,
        rng: &mut impl Rng,
    ) -> bool {
        let bin = self.sheets[s].bin;
        let bt = &p.bins[bin];
        let cb = self.sheets[s].layout.container().outer_cd().bbox();
        let x_ext = self.extent(s);
        let used = x_ext - cb.x_min;
        let wall_x = x_ext - step * used;
        if used <= 0.0 || wall_x <= cb.x_min {
            return false;
        }
        let (ox0, oy0, ox1, oy1) = ring_bbox(&bt.outer_ring);
        let wall = vec![
            Point::new(wall_x as f64, oy0 - 1.0),
            Point::new(ox1.max(x_ext as f64) + 1.0, oy0 - 1.0),
            Point::new(ox1.max(x_ext as f64) + 1.0, oy1 + 1.0),
            Point::new(wall_x as f64, oy1 + 1.0),
        ];
        let _ = ox0;
        let mut holes = bt.hole_rings.clone();
        holes.push(wall);
        let Ok(walled) = container_from_ring(&p.importer, bin as u64, &bt.outer_ring, &holes)
        else {
            return false;
        };
        let mut layout = self.sheets[s].layout.clone();
        layout.swap_container(walled);
        let delta = x_ext - wall_x;
        let split = rng.random_range(cb.x_min..x_ext.max(cb.x_min + 1e-3));
        let shift: Vec<_> = layout
            .placed_items()
            .iter()
            .filter(|(_, pi)| pi.shape().centroid().0 > split)
            .map(|(k, pi)| (k, pi.d_transf()))
            .collect();
        for (k, dt) in shift {
            let pi = layout.remove_item(k);
            layout.place_item(pi.item(), shifted(dt, -delta, 0.0));
        }
        let mut sep = Separator::new(layout, SEP_CONFIG);
        if !sep.separate(deadline, cancel, rng) {
            return false;
        }
        let mut layout = sep.layout;
        layout.swap_container(bt.container.clone());
        self.sheets[s].layout = layout;
        true
    }

    /// Tries to fit one more copy of item type `it` on sheet `s` by dropping it in at random and
    /// separating.
    pub fn insert(
        &mut self,
        p: &Prepared,
        s: usize,
        it: usize,
        deadline: Instant,
        cancel: &dyn Fn() -> bool,
        rng: &mut impl Rng,
    ) -> bool {
        if self.missing[it] == 0 {
            return false;
        }
        let item = &p.items[it].item;
        let mut layout = self.sheets[s].layout.clone();
        if let Some((dt, _)) = search(layout.cde(), item, Budget::REPAIR, rng) {
            layout.place_item(item, dt);
        } else {
            let cb = layout.container().outer_cd().bbox();
            let Some(dt) = random_dt(item, cb, rng) else {
                return false;
            };
            layout.place_item(item, dt);
            let mut sep = Separator::new(layout, SEP_CONFIG);
            if !sep.separate(deadline, cancel, rng) {
                return false;
            }
            layout = sep.layout;
        }
        self.sheets[s].layout = layout;
        self.missing[it] -= 1;
        true
    }

    /// Snaps rotations within 1° of a multiple of 90° to exactly that angle when the snapped
    /// position is still collision-free. Purely cosmetic; it keeps square parts square.
    pub fn snap_rotations(&mut self) {
        use jagua_rs::collision_detection::hazards::filter::NoFilter;
        use jagua_rs::geometry::DTransformation;
        use jagua_rs::geometry::geo_enums::RotationRange;
        use jagua_rs::geometry::geo_traits::TransformableFrom;
        let quarter = std::f32::consts::FRAC_PI_2;
        for sh in &mut self.sheets {
            let keys: Vec<PItemKey> = sh.layout.placed_items().keys().collect();
            for k in keys {
                let pi = &sh.layout.placed_items()[k];
                let dt = pi.d_transf();
                let item = pi.item().clone();
                if !matches!(
                    item.allowed_orientations().rotations(),
                    RotationRange::Continuous
                ) {
                    continue;
                }
                let r = dt.rotation();
                let snapped = (r / quarter).round() * quarter;
                if (r - snapped).abs() < 1e-6 || (r - snapped).abs() > 1f32.to_radians() {
                    continue;
                }
                let cand =
                    DTransformation::new(snapped, dt.translation()).with_reflection(dt.reflected);
                let old = sh.layout.remove_item(k);
                let mut shape = item.shape_cd().as_ref().clone();
                shape.transform_from(item.shape_cd(), &cand.compose());
                let ok = !sh.layout.cde().detect_poly_collision(&shape, &NoFilter);
                sh.layout.place_item(old.item(), if ok { cand } else { dt });
            }
        }
    }

    /// Removes a handful of placed items. Returns false when there was nothing to remove.
    pub fn ruin(&mut self, p: &Prepared, rng: &mut impl Rng) -> bool {
        if self.sheets.is_empty() {
            return false;
        }
        let n_sheets = self.sheets.len();
        // Favour the last sheet: that's where the leftover is and where compaction pays off.
        let s = if n_sheets > 1 && rng.random_bool(0.5) {
            n_sheets - 1
        } else {
            rng.random_range(0..n_sheets)
        };
        let roll: f64 = rng.random();

        if roll < 0.04 && (n_sheets > 1 || p.bins.len() > 1) {
            // Empty a whole sheet: lets the search drop a sheet or switch to a cheaper bin.
            let keys: Vec<PItemKey> = self.sheets[s].layout.placed_items().keys().collect();
            for k in keys {
                self.remove(s, k);
            }
            self.drop_empty_sheets();
            return true;
        }

        let items: Vec<(PItemKey, f32, f32, f32, f32)> = self.sheets[s]
            .layout
            .placed_items()
            .iter()
            .map(|(k, pi)| {
                let b = pi.shape().bbox();
                (
                    k,
                    (b.x_min + b.x_max) * 0.5,
                    (b.y_min + b.y_max) * 0.5,
                    b.x_max,
                    b.width().max(b.height()),
                )
            })
            .collect();
        if items.is_empty() {
            return false;
        }

        let mut victims: Vec<PItemKey> = Vec::new();
        if roll < 0.55 {
            // A region around a random item.
            let seed = items[rng.random_range(0..items.len())];
            let r = seed.4 * rng.random_range(0.5..2.5);
            for it in &items {
                let d = ((it.1 - seed.1).powi(2) + (it.2 - seed.2).powi(2)).sqrt();
                if d <= r {
                    victims.push(it.0);
                }
            }
            victims.truncate(12);
        } else if roll < 0.8 {
            // The rightmost items: compaction pressure.
            let mut sorted = items.clone();
            sorted.sort_by(|a, b| b.3.total_cmp(&a.3));
            let k = rng.random_range(1..=4.min(sorted.len()));
            victims.extend(sorted.iter().take(k).map(|i| i.0));
        } else {
            let k = rng.random_range(1..=4.min(items.len()));
            for _ in 0..k {
                victims.push(items[rng.random_range(0..items.len())].0);
            }
            victims.sort();
            victims.dedup();
        }
        for k in victims {
            self.remove(s, k);
        }
        self.drop_empty_sheets();
        true
    }
}

fn ring_bbox(r: &[Point]) -> (f64, f64, f64, f64) {
    r.iter()
        .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, p| {
            (b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y))
        })
}
