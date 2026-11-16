//! Part-in-part: nests smaller parts inside the holes of larger ones before the main nest.
//!
//! Every copy of a part has the same holes, so a fill computed once in the parent's local
//! coordinates applies to any copy. Each distinct fill becomes a "composite" item type: the parent
//! outline with children attached. The main nest places composites like plain parts and the
//! children follow the parent's placement.

use crate::place::{Budget, search};
use crate::prep::{Child, container_from_ring, part_rotation};
use anyhow::Result;
use jagua_rs::entities::Layout;
use jagua_rs::io::ext_repr::{ExtItem, ExtOrientation, ExtRotation, ExtSPolygon, ExtShape};
use jagua_rs::io::import::Importer;
use kurbo::{Affine, Point};
use nest_core::geom::{flatten, ring_area};
use nest_core::{NestJob, Rotation};
use rand::SeedableRng;
use rand::rngs::SmallRng;

pub struct Composite {
    pub parent: usize,
    pub children: Vec<Child>,
    pub count: usize,
}

/// Whether `child` may ride inside `parent` without breaking its own rotation or mirror rules.
fn compatible(job: &NestJob, parent: usize, child: usize) -> bool {
    let s = &job.settings;
    let (p, c) = (&job.parts[parent], &job.parts[child]);
    if p.allow_mirror && !c.allow_mirror {
        return false;
    }
    match (part_rotation(p, s), part_rotation(c, s)) {
        (_, Rotation::Any) => true,
        (Rotation::Fixed, Rotation::Fixed) => true,
        (Rotation::Fixed, Rotation::Step(_)) => true,
        (Rotation::Step(a), Rotation::Step(b)) => {
            let k = a / b;
            (k - k.round()).abs() < 1e-6
        }
        _ => false,
    }
}

fn child_rotation(job: &NestJob, child: usize) -> ExtRotation {
    match part_rotation(&job.parts[child], &job.settings) {
        Rotation::Any => ExtRotation::Continuous {},
        Rotation::Fixed => ExtRotation::Discrete { angles: vec![0.0] },
        Rotation::Step(d) => {
            let n = (360.0 / d).floor().max(1.0) as usize;
            ExtRotation::Discrete {
                angles: (0..n).map(|i| (i as f64 * d) as f32).collect(),
            }
        }
    }
}

pub fn fill_holes(
    job: &NestJob,
    importer: &Importer,
    rings: &[Option<Vec<Point>>],
    demand: &mut [usize],
    tol: f64,
) -> Result<Vec<Composite>> {
    let mut rng = SmallRng::seed_from_u64(job.settings.seed.unwrap_or(7));
    let areas: Vec<f64> = rings
        .iter()
        .map(|r| r.as_ref().map_or(0.0, |r| ring_area(r).abs()))
        .collect();
    let smallest = areas
        .iter()
        .copied()
        .filter(|a| *a > 0.0)
        .fold(f64::INFINITY, f64::min);
    let mut out = Vec::new();

    // Children are imported once each, as plain items (no rotation inherited from a parent).
    let mut child_items = Vec::new();
    for (ci, ring) in rings.iter().enumerate() {
        let item = match ring {
            Some(r) => {
                let ext = ExtItem {
                    id: ci as u64,
                    orientation: ExtOrientation {
                        rotation: child_rotation(job, ci),
                        reflection_axes: vec![],
                    },
                    shape: ExtShape::SimplePolygon(ExtSPolygon(
                        r.iter().map(|p| (p.x as f32, p.y as f32)).collect(),
                    )),
                    min_quality: None,
                };
                importer.import_item(&ext, ci).ok()
            }
            None => None,
        };
        child_items.push(item);
    }

    // Biggest parents first, so the largest holes get first pick.
    let mut parents: Vec<usize> = (0..job.parts.len())
        .filter(|&i| rings[i].is_some())
        .collect();
    parents.sort_by(|&a, &b| areas[b].total_cmp(&areas[a]));

    for &pi in &parents {
        let holes: Vec<Vec<Point>> = job.parts[pi]
            .geometry
            .holes
            .iter()
            .flat_map(|h| flatten(h, tol))
            .filter(|(r, closed)| *closed && r.len() >= 3 && ring_area(r).abs() > smallest * 1.2)
            .map(|(r, _)| r)
            .collect();
        if holes.is_empty() {
            continue;
        }
        while demand[pi] > 0 {
            let mut avail = demand.to_vec();
            let mut children = Vec::new();
            for (hi, hole) in holes.iter().enumerate() {
                let Ok(container) = container_from_ring(importer, hi as u64, hole, &[]) else {
                    continue;
                };
                let mut layout = Layout::new(container);
                let hole_area = ring_area(hole).abs();
                let mut cands: Vec<usize> = (0..job.parts.len())
                    .filter(|&c| {
                        c != pi
                            && child_items[c].is_some()
                            && areas[c] < hole_area
                            && compatible(job, pi, c)
                    })
                    .collect();
                cands.sort_by(|&a, &b| areas[b].total_cmp(&areas[a]));
                for c in cands {
                    let item = child_items[c].as_ref().unwrap();
                    while avail[c] > 0 {
                        let Some((dt, _)) = search(layout.cde(), item, Budget::CONSTRUCT, &mut rng)
                        else {
                            break;
                        };
                        layout.place_item(item, dt);
                        avail[c] -= 1;
                        let ext = jagua_rs::io::export::int_to_ext_transformation(
                            &dt,
                            &item.shape_orig().pre_transform,
                        );
                        children.push(Child {
                            part_idx: c,
                            local: affine_of(&ext),
                        });
                    }
                }
            }
            if children.is_empty() {
                break;
            }
            // How many parent copies can carry this exact fill.
            let mut per: Vec<usize> = vec![0; job.parts.len()];
            for ch in &children {
                per[ch.part_idx] += 1;
            }
            let mut count = demand[pi];
            for (c, &n) in per.iter().enumerate() {
                if let Some(k) = demand[c].checked_div(n) {
                    count = count.min(k);
                }
            }
            if count == 0 {
                break;
            }
            for (c, &n) in per.iter().enumerate() {
                demand[c] -= n * count;
            }
            demand[pi] -= count;
            out.push(Composite {
                parent: pi,
                children,
                count,
            });
        }
    }
    Ok(out)
}

/// kurbo affine for a jagua transformation (mirror y, rotate, translate).
pub fn affine_of(dt: &jagua_rs::geometry::DTransformation) -> Affine {
    let (tx, ty) = dt.translation();
    let m = if dt.reflected {
        Affine::FLIP_Y
    } else {
        Affine::IDENTITY
    };
    Affine::translate((tx as f64, ty as f64)) * Affine::rotate(dt.rotation() as f64) * m
}
