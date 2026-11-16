//! Small geometry helpers shared by the engine, exporters and UI.

use kurbo::{BezPath, PathEl, Point};

/// Flattens every subpath of `path` into a polyline within `tolerance` mm of the curve.
/// Returns one point list per subpath and whether it was closed. Closed rings don't repeat
/// their first point.
pub fn flatten(path: &BezPath, tolerance: f64) -> Vec<(Vec<Point>, bool)> {
    let mut out: Vec<(Vec<Point>, bool)> = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut closed = false;
    kurbo::flatten(path.iter(), tolerance, |el| match el {
        PathEl::MoveTo(p) => {
            if current.len() > 1 {
                out.push((std::mem::take(&mut current), closed));
            }
            current.clear();
            closed = false;
            current.push(p);
        }
        PathEl::LineTo(p) => {
            if current.last() != Some(&p) {
                current.push(p);
            }
        }
        PathEl::ClosePath => {
            closed = true;
            if current.len() > 1 && current.first() == current.last() {
                current.pop();
            }
            if current.len() > 1 {
                out.push((std::mem::take(&mut current), true));
            }
            current.clear();
            closed = false;
        }
        _ => unreachable!("flatten only emits moves, lines and closes"),
    });
    if current.len() > 1 {
        out.push((current, closed));
    }
    out
}

/// The first closed ring of `path`, flattened. Empty when there is none.
pub fn flatten_ring(path: &BezPath, tolerance: f64) -> Vec<Point> {
    flatten(path, tolerance)
        .into_iter()
        .find(|(_, closed)| *closed)
        .map(|(pts, _)| pts)
        .unwrap_or_default()
}

/// Signed area of a ring (positive when counter-clockwise in y-up coordinates).
pub fn ring_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Even-odd point-in-ring test.
pub fn point_in_ring(pt: Point, ring: &[Point]) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > pt.y) != (b.y > pt.y) && pt.x < (b.x - a.x) * (pt.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// A closed polygon path through `points`.
pub fn polygon_path(points: &[Point]) -> BezPath {
    let mut path = BezPath::new();
    if let Some((first, rest)) = points.split_first() {
        path.move_to(*first);
        for p in rest {
            path.line_to(*p);
        }
        path.close_path();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Circle, Rect, Shape};

    #[test]
    fn flattens_a_rect_to_four_points() {
        let path = Rect::new(0.0, 0.0, 10.0, 5.0).to_path(0.1);
        let rings = flatten(&path, 0.1);
        assert_eq!(rings.len(), 1);
        assert!(rings[0].1);
        assert_eq!(rings[0].0.len(), 4);
        assert!((ring_area(&rings[0].0).abs() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn circle_area_is_close() {
        let path = Circle::new((0.0, 0.0), 10.0).to_path(0.01);
        let ring = flatten_ring(&path, 0.01);
        let a = ring_area(&ring).abs();
        assert!((a - std::f64::consts::PI * 100.0).abs() < 1.0, "{a}");
        assert!(point_in_ring(Point::new(1.0, 1.0), &ring));
        assert!(!point_in_ring(Point::new(11.0, 0.0), &ring));
    }
}
