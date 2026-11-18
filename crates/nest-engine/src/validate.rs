//! Independent check of a `NestResult` against the job, using the true part geometry (flattened
//! finely) and none of jagua's machinery. Used by the tests; the app can run it before export.

use kurbo::Point;
use nest_core::geom::{flatten, flatten_ring, point_in_ring};
use nest_core::{NestJob, NestResult, Part, SheetLayout};

/// Allowed shortfall on distances, in mm: fine flattening error plus f32 rounding in the engine.
const SLACK: f64 = 0.06;
const FLAT: f64 = 0.02;

struct Ring {
    pts: Vec<Point>,
    bbox: (f64, f64, f64, f64),
}

impl Ring {
    fn new(pts: Vec<Point>) -> Ring {
        let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in &pts {
            b = (b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y));
        }
        Ring { pts, bbox: b }
    }

    fn segs(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        let n = self.pts.len();
        (0..n).map(move |i| (self.pts[i], self.pts[(i + 1) % n]))
    }

    fn near(&self, other: &Ring, d: f64) -> bool {
        let (a, b) = (self.bbox, other.bbox);
        a.0 <= b.2 + d && b.0 <= a.2 + d && a.1 <= b.3 + d && b.1 <= a.3 + d
    }
}

fn seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let len2 = ab.hypot2();
    let t = if len2 > 0.0 {
        ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).hypot()
}

fn cross(o: Point, a: Point, b: Point) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn segs_cross(a: Point, b: Point, c: Point, d: Point) -> bool {
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

/// Minimum distance between ring boundaries (0 if they cross). Only segments near the other
/// ring's box are compared.
fn ring_dist(a: &Ring, b: &Ring, cap: f64) -> f64 {
    let in_box = |p: Point, q: Point, r: &Ring| {
        let (x0, y0, x1, y1) = r.bbox;
        p.x.max(q.x) >= x0 - cap
            && p.x.min(q.x) <= x1 + cap
            && p.y.max(q.y) >= y0 - cap
            && p.y.min(q.y) <= y1 + cap
    };
    let sa: Vec<_> = a.segs().filter(|(p, q)| in_box(*p, *q, b)).collect();
    let sb: Vec<_> = b.segs().filter(|(p, q)| in_box(*p, *q, a)).collect();
    let mut best = f64::MAX;
    for &(p, q) in &sa {
        for &(r, s) in &sb {
            if segs_cross(p, q, r, s) {
                return 0.0;
            }
            best = best
                .min(seg_dist(p, r, s))
                .min(seg_dist(q, r, s))
                .min(seg_dist(r, p, q))
                .min(seg_dist(s, p, q));
        }
    }
    best
}

fn part_rings(part: &Part, a: kurbo::Affine) -> (Ring, Vec<Ring>) {
    let outer = Ring::new(
        flatten_ring(&part.geometry.outer, FLAT)
            .into_iter()
            .map(|p| a * p)
            .collect(),
    );
    let holes = part
        .geometry
        .holes
        .iter()
        .flat_map(|h| flatten(h, FLAT))
        .filter(|(r, c)| *c && r.len() >= 3)
        .map(|(r, _)| Ring::new(r.into_iter().map(|p| a * p).collect()))
        .collect();
    (outer, holes)
}

/// Returns a description of every problem found; empty means the result is valid.
pub fn check(job: &NestJob, res: &NestResult) -> Vec<String> {
    let mut errs = Vec::new();

    // Counts: never place more copies than asked for.
    for part in &job.parts {
        let placed = res
            .sheets
            .iter()
            .flat_map(|sh| &sh.placements)
            .filter(|p| p.part_id == part.id)
            .count() as u32;
        let missing: u32 = res
            .unplaced
            .iter()
            .filter(|(id, _)| *id == part.id)
            .map(|(_, n)| n)
            .sum();
        if placed + missing != part.quantity {
            errs.push(format!(
                "part {}: placed {placed} + missing {missing} != quantity {}",
                part.id, part.quantity
            ));
        }
    }
    let mut stock_use = std::collections::HashMap::new();
    for sh in &res.sheets {
        *stock_use.entry(sh.stock_id).or_insert(0u32) += 1;
    }
    for (id, n) in stock_use {
        match job.stock.iter().find(|st| st.id == id) {
            Some(st) if n > st.quantity => {
                errs.push(format!("stock {id}: used {n} > quantity {}", st.quantity))
            }
            None => errs.push(format!("stock {id} doesn't exist")),
            _ => {}
        }
    }

    for (si, sh) in res.sheets.iter().enumerate() {
        for (_, msg) in sheet_problems(job, sh) {
            errs.push(format!("sheet {si}: {msg}"));
        }
    }
    errs
}

/// Placements on one sheet that break a rule, with what's wrong. Each entry names the
/// placement indices involved (two for a pair that's too close).
pub fn sheet_problems(job: &NestJob, sh: &SheetLayout) -> Vec<(Vec<usize>, String)> {
    let s = &job.settings;
    let mut out = Vec::new();
    let Some(stock) = job.stock.iter().find(|st| st.id == sh.stock_id) else {
        return out;
    };
    let outline = Ring::new(flatten_ring(&stock.outline, FLAT));
    let defects: Vec<Ring> = stock
        .defects
        .iter()
        .flat_map(|d| flatten(d, FLAT))
        .filter(|(r, c)| *c && r.len() >= 3)
        .map(|(r, _)| Ring::new(r))
        .collect();

    let parts: Vec<(usize, Ring, Vec<Ring>)> = sh
        .placements
        .iter()
        .enumerate()
        .filter_map(|(i, pl)| {
            let part = job.parts.iter().find(|p| p.id == pl.part_id)?;
            let (o, h) = part_rings(part, pl.affine());
            Some((i, o, h))
        })
        .collect();

    for (i, ring, _) in &parts {
        let tag = format!("placement {i}");
        if !ring.pts.iter().all(|p| point_in_ring(*p, &outline.pts)) {
            out.push((vec![*i], format!("{tag}: outside the stock")));
            continue;
        }
        let d = ring_dist(ring, &outline, s.edge_margin + 1.0);
        if d < s.edge_margin - SLACK {
            out.push((vec![*i], format!("{tag}: {d:.3} mm from the edge, margin is {}", s.edge_margin)));
        }
        for (di, def) in defects.iter().enumerate() {
            if !ring.near(def, s.spacing + 1.0) {
                continue;
            }
            let inside = def.pts.iter().any(|p| point_in_ring(*p, &ring.pts))
                || ring.pts.iter().any(|p| point_in_ring(*p, &def.pts));
            let d = ring_dist(ring, def, s.spacing + 1.0);
            if inside || d < s.spacing - SLACK {
                out.push((vec![*i], format!("{tag}: {d:.3} mm from defect {di}")));
            }
        }
    }

    for a in 0..parts.len() {
        for b in (a + 1)..parts.len() {
            let (ia, ra, ha) = &parts[a];
            let (ib, rb, hb) = &parts[b];
            if !ra.near(rb, s.spacing + 1.0) {
                continue;
            }
            // Part-in-part: one part entirely inside a hole of the other.
            let in_hole = |inner: &Ring, holes: &[Ring]| {
                holes.iter().any(|h| {
                    inner.pts.iter().all(|p| point_in_ring(*p, &h.pts))
                        && ring_dist(inner, h, s.spacing + 1.0) >= s.spacing - SLACK
                })
            };
            if in_hole(rb, ha) || in_hole(ra, hb) {
                continue;
            }
            let overlap = ra.pts.iter().any(|p| point_in_ring(*p, &rb.pts))
                || rb.pts.iter().any(|p| point_in_ring(*p, &ra.pts));
            let d = ring_dist(ra, rb, s.spacing + 1.0);
            if overlap || d < s.spacing - SLACK {
                out.push((vec![*ia, *ib], format!("placements {ia} and {ib} are {d:.3} mm apart (overlap: {overlap}), spacing is {}", s.spacing)));
            }
        }
    }
    out
}

/// Indices of the placements on each sheet that overlap, sit too close, or leave the stock.
pub fn conflicts(job: &NestJob, res: &NestResult) -> Vec<Vec<usize>> {
    res.sheets
        .iter()
        .map(|sh| {
            let mut v: Vec<usize> = sheet_problems(job, sh).into_iter().flat_map(|(i, _)| i).collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect()
}
