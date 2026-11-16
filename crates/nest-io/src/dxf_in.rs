//! DXF reader. Model space entities, block inserts, unit scaling, and chaining of loose segments
//! into closed loops.

use crate::RawImport;
use anyhow::{Context, Result};
use dxf::entities::{Entity, EntityType};
use dxf::enums::Units;
use dxf::{Drawing, Point as DPoint};
use kurbo::{Affine, Arc, BezPath, ParamCurve, PathSeg, Point, Vec2};
use std::collections::{HashMap, HashSet};

/// Endpoints closer than this (mm) are joined.
const CHAIN_EPS: f64 = 0.01;
/// Second pass for chains still open: gaps up to this (mm) are closed, with a warning.
const GAP_EPS: f64 = 0.25;
const MAX_INSERT_DEPTH: usize = 8;

pub(crate) fn read(data: &[u8]) -> Result<RawImport> {
    let drawing = Drawing::load(&mut std::io::Cursor::new(data))
        .context("The DXF couldn't be parsed. Save it as an ASCII DXF and try again")?;
    let scale = unit_scale(drawing.header.default_drawing_units);
    let hidden: HashSet<String> = drawing
        .layers()
        .filter(|l| !l.is_layer_on)
        .map(|l| l.name.to_lowercase())
        .collect();
    let blocks: HashMap<String, &dxf::Block> = drawing
        .blocks()
        .map(|b| (b.name.to_lowercase(), b))
        .collect();

    let mut ctx = Ctx {
        blocks: &blocks,
        hidden: &hidden,
        closed: Vec::new(),
        segments: Vec::new(),
        skipped: HashSet::new(),
    };
    let base = Affine::scale(scale);
    for e in drawing.entities() {
        if e.common.is_in_paper_space {
            continue;
        }
        ctx.entity(e, base, 0);
    }

    let mut warnings = Vec::new();
    let (mut chains, gaps) = chain(std::mem::take(&mut ctx.segments));
    let mut paths = std::mem::take(&mut ctx.closed);
    paths.append(&mut chains);
    if gaps > 0 {
        warnings.push(format!(
            "Closed {gaps} small {} (under {GAP_EPS} mm) between lines in {{file}}.",
            if gaps == 1 { "gap" } else { "gaps" }
        ));
    }
    if !ctx.skipped.is_empty() {
        let mut names: Vec<_> = ctx.skipped.into_iter().collect();
        names.sort();
        warnings.push(format!(
            "Skipped unsupported DXF entities in {{file}}: {}.",
            names.join(", ")
        ));
    }
    Ok(RawImport {
        groups: vec![paths],
        warnings,
    })
}

fn unit_scale(units: Units) -> f64 {
    match units {
        Units::Inches => 25.4,
        Units::Feet => 304.8,
        Units::Centimeters => 10.0,
        Units::Meters => 1000.0,
        Units::Decimeters => 100.0,
        Units::Microns => 0.001,
        Units::Mils => 0.0254,
        Units::Microinches => 0.0000254,
        Units::Yards => 914.4,
        _ => 1.0,
    }
}

struct Ctx<'a> {
    blocks: &'a HashMap<String, &'a dxf::Block>,
    hidden: &'a HashSet<String>,
    /// Paths that are complete on their own (closed polylines, circles, closed splines).
    closed: Vec<BezPath>,
    /// Open pieces waiting to be chained.
    segments: Vec<Vec<PathSeg>>,
    skipped: HashSet<&'static str>,
}

fn pt(p: &DPoint) -> Point {
    Point::new(p.x, p.y)
}

/// Mirror for entities whose extrusion points down (-Z): their OCS x-axis is flipped.
fn ocs(normal_z: f64) -> Affine {
    if normal_z < 0.0 {
        Affine::new([-1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    } else {
        Affine::IDENTITY
    }
}

impl Ctx<'_> {
    fn entity(&mut self, e: &Entity, t: Affine, depth: usize) {
        if self.hidden.contains(&e.common.layer.to_lowercase()) {
            return;
        }
        match &e.specific {
            EntityType::Line(l) => {
                let seg = PathSeg::Line(kurbo::Line::new(t * pt(&l.p1), t * pt(&l.p2)));
                self.segments.push(vec![seg]);
            }
            EntityType::Arc(a) => {
                let t = t * ocs(a.normal.z);
                let mut sweep = (a.end_angle - a.start_angle).rem_euclid(360.0);
                if sweep == 0.0 {
                    sweep = 360.0;
                }
                let arc = Arc::new(
                    pt(&a.center),
                    Vec2::new(a.radius, a.radius),
                    a.start_angle.to_radians(),
                    sweep.to_radians(),
                    0.0,
                );
                let segs = arc_segments(&arc, t);
                self.segments.push(segs);
            }
            EntityType::Circle(c) => {
                let t = t * ocs(c.normal.z);
                let circle = kurbo::Circle::new(pt(&c.center), c.radius);
                self.closed.push(t * kurbo::Shape::to_path(&circle, 1e-6));
            }
            EntityType::Ellipse(el) => {
                let t = t * ocs(el.normal.z);
                let major = Vec2::new(el.major_axis.x, el.major_axis.y);
                let rx = major.hypot();
                let ry = rx * el.minor_axis_ratio;
                let mut sweep =
                    (el.end_parameter - el.start_parameter).rem_euclid(std::f64::consts::TAU);
                if sweep.abs() < 1e-9 {
                    sweep = std::f64::consts::TAU;
                }
                let arc = Arc::new(
                    pt(&el.center),
                    Vec2::new(rx, ry),
                    el.start_parameter,
                    sweep,
                    major.atan2(),
                );
                let segs = arc_segments(&arc, t);
                if (sweep - std::f64::consts::TAU).abs() < 1e-9 {
                    self.closed.push(close(segs));
                } else {
                    self.segments.push(segs);
                }
            }
            EntityType::LwPolyline(p) => {
                let t = t * ocs(p.extrusion_direction.z);
                let verts: Vec<(Point, f64)> = p
                    .vertices
                    .iter()
                    .map(|v| (Point::new(v.x, v.y), v.bulge))
                    .collect();
                self.polyline(&verts, p.is_closed(), t);
            }
            EntityType::Polyline(p) => {
                let t = t * ocs(p.normal.z);
                let verts: Vec<(Point, f64)> =
                    p.vertices().map(|v| (pt(&v.location), v.bulge)).collect();
                self.polyline(&verts, p.is_closed(), t);
            }
            EntityType::Spline(s) => {
                let pts = spline_points(s);
                if pts.len() < 2 {
                    return;
                }
                let segs: Vec<PathSeg> = pts
                    .windows(2)
                    .map(|w| PathSeg::Line(kurbo::Line::new(t * w[0], t * w[1])))
                    .collect();
                if s.is_closed() || pts[0].distance(*pts.last().unwrap()) < 1e-9 {
                    self.closed.push(close(segs));
                } else {
                    self.segments.push(segs);
                }
            }
            EntityType::Insert(ins) => {
                if depth >= MAX_INSERT_DEPTH {
                    return;
                }
                let Some(block) = self.blocks.get(&ins.name.to_lowercase()).copied() else {
                    return;
                };
                let base = Vec2::new(block.base_point.x, block.base_point.y);
                let xs = if ins.x_scale_factor == 0.0 {
                    1.0
                } else {
                    ins.x_scale_factor
                };
                let ys = if ins.y_scale_factor == 0.0 {
                    1.0
                } else {
                    ins.y_scale_factor
                };
                let cols = ins.column_count.max(1);
                let rows = ins.row_count.max(1);
                for r in 0..rows {
                    for c in 0..cols {
                        let offset =
                            Vec2::new(c as f64 * ins.column_spacing, r as f64 * ins.row_spacing);
                        let local = Affine::translate(Vec2::new(ins.location.x, ins.location.y))
                            * Affine::rotate(ins.rotation.to_radians())
                            * Affine::translate(offset)
                            * Affine::scale_non_uniform(xs, ys)
                            * Affine::translate(-base);
                        let t2 = t * ocs(ins.extrusion_direction.z) * local;
                        for e in &block.entities {
                            self.entity(e, t2, depth + 1);
                        }
                    }
                }
            }
            EntityType::Text(_) | EntityType::MText(_) => {
                self.skipped.insert("text (convert it to outlines)");
            }
            _ => {}
        }
    }

    fn polyline(&mut self, verts: &[(Point, f64)], closed: bool, t: Affine) {
        if verts.len() < 2 {
            return;
        }
        let n = verts.len();
        let count = if closed { n } else { n - 1 };
        let mut segs = Vec::new();
        for i in 0..count {
            let (a, bulge) = verts[i];
            let b = verts[(i + 1) % n].0;
            if a.distance(b) < 1e-12 {
                continue;
            }
            if bulge.abs() < 1e-12 {
                segs.push(PathSeg::Line(kurbo::Line::new(t * a, t * b)));
            } else {
                segs.extend(bulge_segments(a, b, bulge, t));
            }
        }
        if segs.is_empty() {
            return;
        }
        let first = segs[0].start();
        let last = segs.last().unwrap().end();
        if closed || first.distance(last) < 1e-9 {
            self.closed.push(close(segs));
        } else {
            self.segments.push(segs);
        }
    }
}

/// Arc from `a` to `b` with DXF bulge (tan of a quarter of the included angle, positive = CCW).
fn bulge_segments(a: Point, b: Point, bulge: f64, t: Affine) -> Vec<PathSeg> {
    let theta = 4.0 * bulge.atan();
    let chord = b - a;
    let d = chord.hypot();
    let r = d / (2.0 * (theta.abs() / 2.0).sin());
    // The center sits on the chord's perpendicular bisector, left of a→b for a CCW arc under 180°.
    // The signed offset (d/2)/tan(θ/2) covers every case, including arcs over 180° and CW arcs.
    let mid = a.midpoint(b);
    let perp = Vec2::new(-chord.y, chord.x) / d;
    let center = mid + perp * ((d / 2.0) / (theta / 2.0).tan());
    let start = (a - center).atan2();
    let arc = Arc::new(center, Vec2::new(r, r), start, theta, 0.0);
    let mut segs = arc_segments(&arc, t);
    // Snap ends to the exact vertices so chaining stays exact.
    if let Some(first) = segs.first_mut() {
        set_start(first, t * a);
    }
    if let Some(last) = segs.last_mut() {
        set_end(last, t * b);
    }
    segs
}

fn arc_segments(arc: &Arc, t: Affine) -> Vec<PathSeg> {
    let mut segs = Vec::new();
    let mut prev = arc.center
        + Vec2::new(
            arc.radii.x * arc.start_angle.cos(),
            arc.radii.y * arc.start_angle.sin(),
        )
        .rotate(arc.x_rotation);
    prev = t * prev;
    for el in arc.append_iter(1e-6) {
        if let kurbo::PathEl::CurveTo(a, b, p) = el {
            let seg = PathSeg::Cubic(kurbo::CubicBez::new(prev, t * a, t * b, t * p));
            prev = t * p;
            segs.push(seg);
        }
    }
    segs
}

trait Rotate {
    fn rotate(self, angle: f64) -> Self;
}
impl Rotate for Vec2 {
    fn rotate(self, angle: f64) -> Vec2 {
        let (s, c) = angle.sin_cos();
        Vec2::new(self.x * c - self.y * s, self.x * s + self.y * c)
    }
}

fn set_start(seg: &mut PathSeg, p: Point) {
    match seg {
        PathSeg::Line(l) => l.p0 = p,
        PathSeg::Quad(q) => q.p0 = p,
        PathSeg::Cubic(c) => c.p0 = p,
    }
}

fn set_end(seg: &mut PathSeg, p: Point) {
    match seg {
        PathSeg::Line(l) => l.p1 = p,
        PathSeg::Quad(q) => q.p2 = p,
        PathSeg::Cubic(c) => c.p3 = p,
    }
}

fn close(segs: Vec<PathSeg>) -> BezPath {
    let mut path = to_path(&segs);
    path.close_path();
    path
}

fn to_path(segs: &[PathSeg]) -> BezPath {
    let mut path = BezPath::new();
    if let Some(first) = segs.first() {
        path.move_to(first.start());
    }
    for s in segs {
        match s {
            PathSeg::Line(l) => path.line_to(l.p1),
            PathSeg::Quad(q) => path.quad_to(q.p1, q.p2),
            PathSeg::Cubic(c) => path.curve_to(c.p1, c.p2, c.p3),
        }
    }
    path
}

fn reverse(segs: Vec<PathSeg>) -> Vec<PathSeg> {
    segs.into_iter().rev().map(|s| s.reverse()).collect()
}

/// Joins open pieces end to end. Returns closed loops and still-open chains, plus how many gaps
/// wider than `CHAIN_EPS` were closed.
fn chain(pieces: Vec<Vec<PathSeg>>) -> (Vec<BezPath>, usize) {
    let mut out = Vec::new();
    let (closed, open) = join(pieces, CHAIN_EPS);
    out.extend(closed.into_iter().map(close));
    // Second pass with a wider tolerance for what's left.
    let before: usize = open.len();
    let (closed2, open2) = join(open, GAP_EPS);
    let gaps = if closed2.is_empty() {
        0
    } else {
        before - open2.len()
    };
    out.extend(closed2.into_iter().map(close));
    out.extend(open2.iter().map(|c| to_path(c)));
    (out, gaps)
}

fn join(pieces: Vec<Vec<PathSeg>>, eps: f64) -> (Vec<Vec<PathSeg>>, Vec<Vec<PathSeg>>) {
    let cell = eps * 4.0;
    let key = |p: Point| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
    // Grid of endpoints: (piece index, is_end).
    let mut grid: HashMap<(i64, i64), Vec<(usize, bool)>> = HashMap::new();
    for (i, piece) in pieces.iter().enumerate() {
        grid.entry(key(piece[0].start()))
            .or_default()
            .push((i, false));
        grid.entry(key(piece.last().unwrap().end()))
            .or_default()
            .push((i, true));
    }
    let mut used = vec![false; pieces.len()];
    let find = |p: Point,
                used: &[bool],
                grid: &HashMap<(i64, i64), Vec<(usize, bool)>>,
                pieces: &[Vec<PathSeg>]| {
        let (kx, ky) = key(p);
        let mut best: Option<(usize, bool, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = grid.get(&(kx + dx, ky + dy)) {
                    for &(i, is_end) in list {
                        if used[i] {
                            continue;
                        }
                        let q = if is_end {
                            pieces[i].last().unwrap().end()
                        } else {
                            pieces[i][0].start()
                        };
                        let d = p.distance(q);
                        if d <= eps && best.is_none_or(|b| d < b.2) {
                            best = Some((i, is_end, d));
                        }
                    }
                }
            }
        }
        best
    };

    let mut closed = Vec::new();
    let mut open = Vec::new();
    for start in 0..pieces.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut chain = pieces[start].clone();
        // Extend forward from the end.
        loop {
            let end = chain.last().unwrap().end();
            if chain[0].start().distance(end) <= eps && chain_len(&chain) > eps * 4.0 {
                break;
            }
            let Some((i, is_end, _)) = find(end, &used, &grid, &pieces) else {
                break;
            };
            used[i] = true;
            let mut next = pieces[i].clone();
            if is_end {
                next = reverse(next);
            }
            set_start(&mut next[0], end);
            chain.extend(next);
        }
        // Extend backward from the start.
        loop {
            let begin = chain[0].start();
            if chain.last().unwrap().end().distance(begin) <= eps && chain_len(&chain) > eps * 4.0 {
                break;
            }
            let Some((i, is_end, _)) = find(begin, &used, &grid, &pieces) else {
                break;
            };
            used[i] = true;
            let mut prev = pieces[i].clone();
            if !is_end {
                prev = reverse(prev);
            }
            let n = prev.len();
            set_end(&mut prev[n - 1], begin);
            prev.extend(chain);
            chain = prev;
        }
        let gap = chain[0].start().distance(chain.last().unwrap().end());
        if gap <= eps && chain_len(&chain) > eps * 4.0 {
            let s = chain[0].start();
            let n = chain.len();
            set_end(&mut chain[n - 1], s);
            closed.push(chain);
        } else {
            open.push(chain);
        }
    }
    (closed, open)
}

fn chain_len(chain: &[PathSeg]) -> f64 {
    chain.iter().map(|s| s.start().distance(s.end())).sum()
}

/// Samples a B-spline (rational if weighted) along its knot spans.
fn spline_points(s: &dxf::entities::Spline) -> Vec<Point> {
    let ctrl: Vec<Point> = s.control_points.iter().map(pt).collect();
    let degree = s.degree_of_curve.max(1) as usize;
    let knots = &s.knot_values;
    if ctrl.len() <= degree || knots.len() != ctrl.len() + degree + 1 {
        // Fall back to fit points as a polyline.
        return s.fit_points.iter().map(pt).collect();
    }
    let weights: Vec<f64> = if s.weight_values.len() == ctrl.len() {
        s.weight_values.clone()
    } else {
        vec![1.0; ctrl.len()]
    };
    let lo = knots[degree];
    let hi = knots[ctrl.len()];
    let mut out = Vec::new();
    let spans: Vec<(f64, f64)> = (degree..ctrl.len())
        .map(|i| (knots[i], knots[i + 1]))
        .filter(|(a, b)| b > a)
        .collect();
    for (si, (a, b)) in spans.iter().enumerate() {
        let steps = 32;
        for k in 0..steps {
            let u = a + (b - a) * k as f64 / steps as f64;
            out.push(de_boor(u, degree, knots, &ctrl, &weights));
        }
        if si == spans.len() - 1 {
            out.push(de_boor(*b, degree, knots, &ctrl, &weights));
        }
    }
    let _ = (lo, hi);
    out
}

fn de_boor(u: f64, p: usize, knots: &[f64], ctrl: &[Point], w: &[f64]) -> Point {
    let n = ctrl.len();
    // Span index k with knots[k] <= u < knots[k+1], clamped to valid range.
    let mut k = p;
    while k < n - 1 && u >= knots[k + 1] {
        k += 1;
    }
    let mut d: Vec<(f64, f64, f64)> = (0..=p)
        .map(|j| {
            let c = ctrl[j + k - p];
            let wt = w[j + k - p];
            (c.x * wt, c.y * wt, wt)
        })
        .collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = j + k - p;
            let denom = knots[i + p + 1 - r] - knots[i];
            let alpha = if denom.abs() < 1e-15 {
                0.0
            } else {
                (u - knots[i]) / denom
            };
            d[j] = (
                (1.0 - alpha) * d[j - 1].0 + alpha * d[j].0,
                (1.0 - alpha) * d[j - 1].1 + alpha * d[j].1,
                (1.0 - alpha) * d[j - 1].2 + alpha * d[j].2,
            );
        }
    }
    let (x, y, wt) = d[p];
    Point::new(x / wt, y / wt)
}

#[cfg(test)]
mod tests {
    use crate::import_dxf;
    use dxf::entities::*;
    use dxf::{Drawing, LwPolylineVertex, Point as DPoint};
    use kurbo::Shape;

    fn save(d: &Drawing) -> Vec<u8> {
        // R2000 or later: older versions have no LWPOLYLINE or $INSUNITS.
        let mut d = d.clone();
        d.header.version = dxf::enums::AcadVersion::R2000;
        let mut buf = Vec::new();
        d.save(&mut buf).unwrap();
        buf
    }

    fn line(d: &mut Drawing, a: (f64, f64), b: (f64, f64)) {
        d.add_entity(Entity::new(EntityType::Line(Line::new(
            DPoint::new(a.0, a.1, 0.0),
            DPoint::new(b.0, b.1, 0.0),
        ))));
    }

    #[test]
    fn loose_lines_and_arc_chain_into_one_part_with_hole() {
        let mut d = Drawing::new();
        d.header.default_drawing_units = dxf::enums::Units::Millimeters;
        // 0..100 x 0..50 with a rounded right end: lines out of order and reversed.
        line(&mut d, (0.0, 0.0), (75.0, 0.0));
        line(&mut d, (0.0, 50.0), (0.0, 0.0));
        line(&mut d, (75.0, 50.0), (0.0, 50.0));
        let mut arc = Arc::new(DPoint::new(75.0, 25.0, 0.0), 25.0, 270.0, 90.0);
        arc.normal.z = 1.0;
        d.add_entity(Entity::new(EntityType::Arc(arc)));
        d.add_entity(Entity::new(EntityType::Circle(Circle::new(
            DPoint::new(25.0, 25.0, 0.0),
            10.0,
        ))));
        let r = import_dxf(&save(&d)).unwrap();
        assert_eq!(r.parts.len(), 1, "{:?}", r.warnings);
        let g = &r.parts[0].geometry;
        assert_eq!(g.holes.len(), 1);
        let b = g.bbox();
        assert!(
            (b.width() - 100.0).abs() < 1e-3 && (b.height() - 50.0).abs() < 1e-3,
            "{b:?}"
        );
        let expected =
            75.0 * 50.0 + std::f64::consts::PI * 625.0 / 2.0 - std::f64::consts::PI * 100.0;
        assert!(
            (g.area() - expected).abs() < 0.05,
            "{} vs {expected}",
            g.area()
        );
    }

    #[test]
    fn inches_scale_to_mm() {
        let mut d = Drawing::new();
        d.header.default_drawing_units = dxf::enums::Units::Inches;
        d.add_entity(Entity::new(EntityType::Circle(Circle::new(
            DPoint::new(0.0, 0.0, 0.0),
            1.0,
        ))));
        let r = import_dxf(&save(&d)).unwrap();
        let b = r.parts[0].geometry.bbox();
        assert!((b.width() - 50.8).abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn lwpolyline_bulge_makes_semicircle() {
        let mut d = Drawing::new();
        let mut p = LwPolyline::default();
        // Straight bottom edge, then a bulge-1 arc back (a semicircle of radius 10 above it).
        p.vertices.push(LwPolylineVertex {
            x: 0.0,
            y: 0.0,
            bulge: 0.0,
            ..Default::default()
        });
        p.vertices.push(LwPolylineVertex {
            x: 20.0,
            y: 0.0,
            bulge: 1.0,
            ..Default::default()
        });
        p.set_is_closed(true);
        d.add_entity(Entity::new(EntityType::LwPolyline(p)));
        let r = import_dxf(&save(&d)).unwrap();
        let g = &r.parts[0].geometry;
        let b = g.bbox();
        assert!(
            (b.width() - 20.0).abs() < 1e-3 && (b.height() - 10.0).abs() < 1e-3,
            "{b:?}"
        );
        let expected = std::f64::consts::PI * 100.0 / 2.0;
        assert!(
            (g.outer.area().abs() - expected).abs() < 0.01,
            "{}",
            g.outer.area()
        );
    }

    #[test]
    fn small_gaps_close_with_warning() {
        let mut d = Drawing::new();
        line(&mut d, (0.0, 0.0), (10.0, 0.0));
        line(&mut d, (10.1, 0.0), (10.0, 10.0));
        line(&mut d, (10.0, 10.0), (0.0, 0.0));
        let r = import_dxf(&save(&d)).unwrap();
        assert_eq!(r.parts.len(), 1);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.starts_with("Closed 1 small gap")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn hidden_layers_are_skipped() {
        let mut d = Drawing::new();
        d.add_layer(dxf::tables::Layer {
            name: "HIDDEN".into(),
            is_layer_on: false,
            ..Default::default()
        });
        let mut e = Entity::new(EntityType::Circle(Circle::new(
            DPoint::new(0.0, 0.0, 0.0),
            5.0,
        )));
        e.common.layer = "HIDDEN".into();
        d.add_entity(e);
        d.add_entity(Entity::new(EntityType::Circle(Circle::new(
            DPoint::new(50.0, 0.0, 0.0),
            5.0,
        ))));
        let r = import_dxf(&save(&d)).unwrap();
        assert_eq!(r.parts.len(), 1);
    }
}
