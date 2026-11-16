//! Turns loose paths into parts.
//!
//! Every closed contour gets a depth in the containment tree. Even depths start a part, odd
//! depths are holes of their parent, so an island inside a hole is its own part (as it is when
//! cut). Sibling outlines that overlap, such as joined glyphs in unwelded text, are merged into
//! one outline with a boolean union, since that's the piece the laser actually frees.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, PathEl, Point, Rect, Shape};
use nest_core::PartGeometry;
use nest_core::geom::{flatten, point_in_ring, polygon_path, ring_area};

/// Ends closer than this (mm) count as touching, so a path that returns to its start is closed.
const CLOSE_EPS: f64 = 1e-3;
/// Max vertices sampled per ring for containment checks.
const SAMPLES: usize = 96;

struct Contour {
    path: BezPath,
    ring: Vec<Point>,
    area: f64,
    bbox: Rect,
    order: usize,
}

/// Groups paths into parts. Paths are in mm, y-up; parts keep the input coordinates (callers
/// normalize). Returns the parts and any warnings.
pub fn assemble_parts(paths: Vec<BezPath>, tolerance: f64) -> (Vec<PartGeometry>, Vec<String>) {
    let (parts, warnings) = assemble(paths, tolerance);
    let warnings = warnings
        .into_iter()
        .map(|w| w.replace("{file}", "the file"))
        .collect();
    (parts, warnings)
}

/// Same as [`assemble_parts`], with `{file}` left in warnings for the caller to fill in.
pub(crate) fn assemble(paths: Vec<BezPath>, tolerance: f64) -> (Vec<PartGeometry>, Vec<String>) {
    let tolerance = tolerance.max(1e-4);
    let mut warnings = Vec::new();
    let mut contours: Vec<Contour> = Vec::new();
    let mut open: Vec<BezPath> = Vec::new();
    let mut duplicates = 0;
    let mut seen = std::collections::HashSet::new();

    for path in paths {
        for (sub, closed) in split_subpaths(&path) {
            if !closed {
                open.push(sub);
                continue;
            }
            let ring = flatten(&sub, tolerance)
                .into_iter()
                .next()
                .map(|(r, _)| r)
                .unwrap_or_default();
            let area = ring_area(&ring).abs();
            if ring.len() < 3 || area < tolerance * tolerance {
                open.push(sub);
                continue;
            }
            let bbox = sub.bounding_box();
            let key = (
                (bbox.x0 * 1e3).round() as i64,
                (bbox.y0 * 1e3).round() as i64,
                (bbox.x1 * 1e3).round() as i64,
                (bbox.y1 * 1e3).round() as i64,
                (area * 1e2).round() as i64,
            );
            if !seen.insert(key) {
                duplicates += 1;
                continue;
            }
            let order = contours.len();
            contours.push(Contour {
                path: sub,
                ring,
                area,
                bbox,
                order,
            });
        }
    }
    if duplicates > 0 {
        warnings.push(format!(
            "Skipped {duplicates} duplicate {} in {{file}}.",
            plural(duplicates, "shape", "shapes")
        ));
    }

    // Containment tree. Sorted by area, a contour's parent is the smallest larger one holding it.
    contours.sort_by(|a, b| b.area.total_cmp(&a.area));
    let n = contours.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut depth = vec![0usize; n];
    for i in 0..n {
        for j in (0..i).rev() {
            if contains(&contours[j], &contours[i]) {
                parent[i] = Some(j);
                depth[i] = depth[j] + 1;
                break;
            }
        }
    }

    // Root (even depth) for each contour; holes belong to their parent root.
    let roots: Vec<usize> = (0..n).filter(|&i| depth[i].is_multiple_of(2)).collect();
    let mut holes_of: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        if depth[i] % 2 == 1 {
            holes_of[parent[i].expect("odd depth has a parent")].push(i);
        }
    }

    // Overlapping sibling roots merge into one part.
    let mut uf = UnionFind::new(n);
    let mut merged = 0;
    for (a_pos, &a) in roots.iter().enumerate() {
        for &b in &roots[a_pos + 1..] {
            if parent[a] == parent[b]
                && overlaps_bbox(contours[a].bbox, contours[b].bbox)
                && overlaps(&contours[a], &contours[b])
                && uf.union(a, b)
            {
                merged += 1;
            }
        }
    }

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of = std::collections::HashMap::new();
    for &r in &roots {
        let g = *group_of.entry(uf.find(r)).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(r);
    }

    // Build parts, each with the ring used for attaching marks.
    let mut built: Vec<(usize, PartGeometry, Vec<Point>, f64)> = Vec::new();
    let mut merged_parts = 0;
    for group in &groups {
        let order = group.iter().map(|&i| contours[i].order).min().unwrap_or(0);
        if let [r] = group.as_slice() {
            let c = &contours[*r];
            let holes = holes_of[*r]
                .iter()
                .map(|&h| contours[h].path.clone())
                .collect();
            built.push((
                order,
                PartGeometry {
                    outer: c.path.clone(),
                    holes,
                    marks: Vec::new(),
                },
                c.ring.clone(),
                c.area,
            ));
            continue;
        }
        merged_parts += 1;
        let fine = tolerance.min(0.01);
        let subject: Vec<Vec<[f64; 2]>> = group
            .iter()
            .map(|&i| ccw_ring(&contours[i].path, fine))
            .collect();
        let clip: Vec<Vec<[f64; 2]>> = group
            .iter()
            .flat_map(|&i| holes_of[i].iter())
            .map(|&h| ccw_ring(&contours[h].path, fine))
            .collect();
        let shapes = if clip.is_empty() {
            subject.overlay_as::<i64>(
                &Vec::<Vec<[f64; 2]>>::new(),
                OverlayRule::Union,
                FillRule::NonZero,
            )
        } else {
            subject.overlay_as::<i64>(&clip, OverlayRule::Difference, FillRule::NonZero)
        };
        for shape in shapes {
            let mut rings = shape.into_iter().map(|c| {
                c.into_iter()
                    .map(|[x, y]| Point::new(x, y))
                    .collect::<Vec<Point>>()
            });
            let Some(outer) = rings.next() else { continue };
            if outer.len() < 3 {
                continue;
            }
            let area = ring_area(&outer).abs();
            let holes = rings
                .filter(|r| r.len() >= 3)
                .map(|r| polygon_path(&r))
                .collect();
            built.push((
                order,
                PartGeometry {
                    outer: polygon_path(&outer),
                    holes,
                    marks: Vec::new(),
                },
                outer,
                area,
            ));
        }
    }
    if merged > 0 {
        warnings.push(format!(
            "Merged overlapping shapes in {{file}} into {merged_parts} single {}.",
            plural(merged_parts, "outline", "outlines")
        ));
    }

    // Open paths become marks on the smallest part that holds them.
    let mut dropped = 0;
    for path in open {
        let pts: Vec<Point> = flatten(&path, tolerance)
            .into_iter()
            .flat_map(|(r, _)| r)
            .collect();
        if pts.is_empty() {
            continue;
        }
        let target = built
            .iter()
            .enumerate()
            .filter(|(_, (_, g, ring, _))| {
                let b = g.outer.bounding_box();
                pts.iter()
                    .all(|p| b.inflate(tolerance, tolerance).contains(*p))
                    && fraction_inside(&pts, ring) >= 0.9
            })
            .min_by(|a, b| a.1.3.total_cmp(&b.1.3))
            .map(|(i, _)| i);
        match target {
            Some(i) => built[i].1.marks.push(path),
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        warnings.push(format!(
            "Skipped {dropped} open {} in {{file}} that {} inside a closed shape. Close {} if {} should be cut.",
            plural(dropped, "path", "paths"),
            plural(dropped, "isn't", "aren't"),
            plural(dropped, "it", "them"),
            plural(dropped, "it", "they"),
        ));
    }

    built.sort_by_key(|(order, ..)| *order);
    (built.into_iter().map(|(_, g, ..)| g).collect(), warnings)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Splits a path into subpaths, each with whether it's closed.
fn split_subpaths(path: &BezPath) -> Vec<(BezPath, bool)> {
    let mut out = Vec::new();
    let mut cur = BezPath::new();
    let mut start = Point::ZERO;
    let mut last = Point::ZERO;
    let mut segments = 0;
    let flush = |cur: &mut BezPath,
                 segments: &mut usize,
                 start: Point,
                 last: Point,
                 out: &mut Vec<(BezPath, bool)>,
                 explicit: bool| {
        if *segments > 0 {
            let mut p = std::mem::take(cur);
            let closed = explicit || start.distance(last) <= CLOSE_EPS;
            if closed && !explicit {
                p.close_path();
            }
            out.push((p, closed));
        }
        *cur = BezPath::new();
        *segments = 0;
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                flush(&mut cur, &mut segments, start, last, &mut out, false);
                cur.move_to(p);
                start = p;
                last = p;
            }
            PathEl::LineTo(p) => {
                cur.line_to(p);
                last = p;
                segments += 1;
            }
            PathEl::QuadTo(a, p) => {
                cur.quad_to(a, p);
                last = p;
                segments += 1;
            }
            PathEl::CurveTo(a, b, p) => {
                cur.curve_to(a, b, p);
                last = p;
                segments += 1;
            }
            PathEl::ClosePath => {
                if segments > 0 {
                    cur.close_path();
                }
                flush(&mut cur, &mut segments, start, last, &mut out, true);
                last = start;
            }
        }
    }
    flush(&mut cur, &mut segments, start, last, &mut out, false);
    out
}

fn sample(ring: &[Point]) -> impl Iterator<Item = &Point> {
    let step = (ring.len() / SAMPLES).max(1);
    ring.iter().step_by(step)
}

fn fraction_inside(pts: &[Point], ring: &[Point]) -> f64 {
    let mut total = 0;
    let mut inside = 0;
    for p in sample(pts) {
        total += 1;
        if point_in_ring(*p, ring) {
            inside += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        inside as f64 / total as f64
    }
}

/// Whether `outer` holds `inner`. Allows a few touching or numerically off vertices.
fn contains(outer: &Contour, inner: &Contour) -> bool {
    let eps = 1e-6 + 1e-6 * outer.bbox.width().max(outer.bbox.height());
    let b = outer.bbox.inflate(eps, eps);
    b.x0 <= inner.bbox.x0
        && b.y0 <= inner.bbox.y0
        && b.x1 >= inner.bbox.x1
        && b.y1 >= inner.bbox.y1
        && inner.area < outer.area
        && fraction_inside(&inner.ring, &outer.ring) >= 0.97
}

fn overlaps_bbox(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// Whether two rings share area: a vertex of one inside the other, or crossing edges.
fn overlaps(a: &Contour, b: &Contour) -> bool {
    if a.ring.iter().any(|p| point_in_ring(*p, &b.ring))
        || b.ring.iter().any(|p| point_in_ring(*p, &a.ring))
    {
        return true;
    }
    edges_cross(&a.ring, &b.ring)
}

fn edges_cross(a: &[Point], b: &[Point]) -> bool {
    let bb = |p: Point, q: Point| Rect::from_points(p, q);
    for i in 0..a.len() {
        let (p1, p2) = (a[i], a[(i + 1) % a.len()]);
        let ra = bb(p1, p2);
        for j in 0..b.len() {
            let (q1, q2) = (b[j], b[(j + 1) % b.len()]);
            if !overlaps_bbox(ra, bb(q1, q2)) {
                continue;
            }
            let d1 = cross(q1, q2, p1);
            let d2 = cross(q1, q2, p2);
            let d3 = cross(p1, p2, q1);
            let d4 = cross(p1, p2, q2);
            if (d1 > 0.0) != (d2 > 0.0)
                && (d3 > 0.0) != (d4 > 0.0)
                && d1 != 0.0
                && d2 != 0.0
                && d3 != 0.0
                && d4 != 0.0
            {
                return true;
            }
        }
    }
    false
}

fn cross(o: Point, a: Point, b: Point) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn ccw_ring(path: &BezPath, tolerance: f64) -> Vec<[f64; 2]> {
    let mut ring = flatten(path, tolerance)
        .into_iter()
        .next()
        .map(|(r, _)| r)
        .unwrap_or_default();
    if ring_area(&ring) < 0.0 {
        ring.reverse();
    }
    ring.into_iter().map(|p| [p.x, p.y]).collect()
}

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind((0..n).collect())
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    /// Joins the sets; true if they were separate.
    fn union(&mut self, a: usize, b: usize) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        self.0[rb] = ra;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Circle, Rect};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
        Rect::new(x0, y0, x1, y1).to_path(0.01)
    }

    #[test]
    fn hole_and_island() {
        // Square with a square hole, and an island inside the hole.
        let paths = vec![
            rect(0.0, 0.0, 100.0, 100.0),
            rect(20.0, 20.0, 80.0, 80.0),
            rect(40.0, 40.0, 60.0, 60.0),
        ];
        let (parts, warnings) = assemble_parts(paths, 0.05);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(parts.len(), 2);
        let big = parts.iter().find(|p| p.bbox().width() > 90.0).unwrap();
        assert_eq!(big.holes.len(), 1);
        assert!((big.area() - (10000.0 - 3600.0)).abs() < 1e-6);
        let island = parts.iter().find(|p| p.bbox().width() < 30.0).unwrap();
        assert!(island.holes.is_empty());
    }

    #[test]
    fn separate_dots_are_separate_parts() {
        let body = Circle::new((0.0, 0.0), 20.0).to_path(0.01);
        let dot1 = Circle::new((-5.0, 30.0), 3.0).to_path(0.01);
        let dot2 = Circle::new((5.0, 30.0), 3.0).to_path(0.01);
        let (parts, _) = assemble_parts(vec![body, dot1, dot2], 0.05);
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn overlapping_outlines_merge() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 15.0, 15.0);
        let (parts, warnings) = assemble_parts(vec![a, b], 0.05);
        assert_eq!(parts.len(), 1, "{warnings:?}");
        assert!(
            (parts[0].area() - 175.0).abs() < 1e-3,
            "{}",
            parts[0].area()
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn crossing_without_contained_vertices_merges() {
        // A plus sign made of two bars: no vertex of one lies inside the other.
        let a = rect(0.0, 4.0, 12.0, 8.0);
        let b = rect(4.0, 0.0, 8.0, 12.0);
        let (parts, _) = assemble_parts(vec![a, b], 0.05);
        assert_eq!(parts.len(), 1);
        assert!((parts[0].area() - 80.0).abs() < 1e-3);
    }

    #[test]
    fn open_paths_become_marks_or_are_dropped() {
        let mut inside = BezPath::new();
        inside.move_to((2.0, 2.0));
        inside.line_to((8.0, 8.0));
        let mut outside = BezPath::new();
        outside.move_to((50.0, 50.0));
        outside.line_to((60.0, 60.0));
        let (parts, warnings) =
            assemble_parts(vec![rect(0.0, 0.0, 10.0, 10.0), inside, outside], 0.05);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].marks.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("Skipped 1 open path"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn duplicates_are_skipped() {
        let (parts, warnings) = assemble_parts(
            vec![rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 10.0, 10.0)],
            0.05,
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(
            warnings,
            vec!["Skipped 1 duplicate shape in the file.".to_string()]
        );
    }

    #[test]
    fn nearly_closed_path_counts_as_closed() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        p.line_to((10.0, 10.0));
        p.line_to((0.0, 0.0005));
        let (parts, _) = assemble_parts(vec![p], 0.05);
        assert_eq!(parts.len(), 1);
    }
}
