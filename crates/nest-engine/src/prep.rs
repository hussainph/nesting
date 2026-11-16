//! Turns a `NestJob` into jagua-rs items and containers.
//!
//! Spacing and margins:
//! - jagua's `min_item_separation` inflates items and deflates containers by half of it, so the
//!   collision polygons stay that far apart from each other, from container edges and from defects.
//! - Collision polygons are flattened curves; the true curve can sit up to `curve_tolerance`
//!   outside its polygon. We pass `spacing + 2 * tol` as the separation so the true curves keep
//!   `spacing` apart.
//! - Container edges then sit `spacing + 2 * tol` from the polygons, or at least `spacing` from
//!   true curves. To get `edge_margin` instead, the stock outline is offset outward by
//!   `spacing - edge_margin` (inward when negative) before import.

use anyhow::{Context, Result, bail};
use jagua_rs::collision_detection::CDEConfig;
use jagua_rs::entities::{Container, Item};
use jagua_rs::geometry::convex_hull::convex_hull_from_points;
use jagua_rs::geometry::fail_fast::SPSurrogateConfig;
use jagua_rs::geometry::primitives::{Point as JPoint, SPolygon};
use jagua_rs::geometry::shape_modification::{ShapeModifyMode, offset_shape};
use jagua_rs::io::ext_repr::{
    ExtContainer, ExtItem, ExtOrientation, ExtQualityZone, ExtRotation, ExtSPolygon, ExtShape,
};
use jagua_rs::io::import::Importer;
use kurbo::{Affine, Point};
use nest_core::geom::{flatten, flatten_ring, ring_area};
use nest_core::{NestJob, NestSettings, Part, Rotation, StockKind};
use std::sync::Arc;

/// A child part placed inside a hole of a parent, in the parent's local coordinates.
#[derive(Clone, Debug)]
pub struct Child {
    pub part_idx: usize,
    pub local: Affine,
}

/// One kind of thing the optimizer places: a part, or a part with other parts already nested in
/// its holes ("composite").
pub struct ItemType {
    pub item: Arc<Item>,
    pub part_idx: usize,
    pub children: Vec<Child>,
    pub demand: usize,
    /// Area used by the objective: the outer area of the part plus its children.
    pub area: f64,
}

pub struct BinType {
    pub container: Container,
    /// Outline (after the margin offset) and defect rings, to rebuild the container with a wall.
    pub outer_ring: Vec<Point>,
    pub hole_rings: Vec<Vec<Point>>,
    pub stock_idx: usize,
    pub qty: usize,
    pub cost: f64,
    pub area: f64,
    /// Scrap that costs nothing to use: a remnant while `remnants_first` is on. Filling it
    /// saves fresh sheet material, so the objective counts only fresh material consumed.
    pub free: bool,
}

pub struct Prepared {
    pub importer: Importer,
    pub items: Vec<ItemType>,
    pub bins: Vec<BinType>,
    /// Bin indices in the order new bins are opened.
    pub bin_order: Vec<usize>,
    /// Copies of parts that can't be placed at all (no valid outline), by part index.
    pub skipped: Vec<(usize, usize)>,
}

pub fn cde_config() -> CDEConfig {
    CDEConfig {
        item_surrogate_config: SPSurrogateConfig {
            n_pole_limits: [(64, 0.0), (16, 0.8), (8, 0.9)],
            ff_pole_area_ratio: 0.5,
        },
    }
}

pub fn importer(settings: &NestSettings) -> Result<Importer> {
    let sep = (settings.spacing.max(0.0) + 2.0 * settings.curve_tolerance.max(0.0)) as f32;
    Importer::new(cde_config(), Some(0.001), Some((0.01, 0.01))).with_min_item_separation(sep)
}

fn ext_poly(ring: &[Point]) -> ExtSPolygon {
    ExtSPolygon(ring.iter().map(|p| (p.x as f32, p.y as f32)).collect())
}

/// A valid simple polygon for `ring`, falling back to its convex hull when the ring self-intersects.
fn simple_ring(ring: &[Point], what: &str) -> Result<Vec<Point>> {
    let pts: Vec<JPoint> = ring
        .iter()
        .map(|p| JPoint(p.x as f32, p.y as f32))
        .collect();
    if SPolygon::new(pts.clone()).is_ok() {
        return Ok(ring.to_vec());
    }
    log::warn!("{what}: outline intersects itself, nesting its convex hull instead");
    let hull = convex_hull_from_points(pts);
    let hull_pts: Vec<Point> = hull
        .iter()
        .map(|p| Point::new(p.0 as f64, p.1 as f64))
        .collect();
    SPolygon::new(hull).with_context(|| format!("{what}: outline has no usable area"))?;
    Ok(hull_pts)
}

fn ext_orientation(rotation: Rotation, mirror: bool) -> ExtOrientation {
    let rotation = match rotation {
        Rotation::Any => ExtRotation::Continuous {},
        Rotation::Fixed => ExtRotation::Discrete { angles: vec![0.0] },
        Rotation::Step(d) if d > 0.0 && d <= 360.0 => {
            let count = (360.0 / d).round();
            if (count * d - 360.0).abs() < 1e-6 {
                ExtRotation::Stepped { step: d as f32 }
            } else {
                let n = (360.0 / d).floor().max(1.0) as usize;
                ExtRotation::Discrete {
                    angles: (0..n).map(|i| (i as f64 * d) as f32).collect(),
                }
            }
        }
        Rotation::Step(_) => ExtRotation::Discrete { angles: vec![0.0] },
    };
    ExtOrientation {
        rotation,
        reflection_axes: if mirror { vec![0.0] } else { vec![] },
    }
}

pub fn part_rotation(part: &Part, settings: &NestSettings) -> Rotation {
    part.rotation.unwrap_or(settings.default_rotation)
}

pub fn outer_ring(part: &Part, tol: f64) -> Result<Vec<Point>> {
    let ring = flatten_ring(&part.geometry.outer, tol);
    if ring.len() < 3 {
        bail!("Part \"{}\" has no closed outline", part.name);
    }
    simple_ring(&ring, &format!("Part \"{}\"", part.name))
}

fn import_item(
    importer: &Importer,
    idx: usize,
    ring: &[Point],
    rotation: Rotation,
    mirror: bool,
) -> Result<Arc<Item>> {
    let ext = ExtItem {
        id: idx as u64,
        orientation: ext_orientation(rotation, mirror),
        shape: ExtShape::SimplePolygon(ext_poly(ring)),
        min_quality: None,
    };
    importer.import_item(&ext, idx)
}

/// Offsets a ring outward (`d > 0`) or inward (`d < 0`).
fn offset_ring(ring: &[Point], d: f64) -> Result<Vec<Point>> {
    if d.abs() < 1e-6 {
        return Ok(ring.to_vec());
    }
    let sp = SPolygon::new(
        ring.iter()
            .map(|p| JPoint(p.x as f32, p.y as f32))
            .collect(),
    )?;
    let mode = if d > 0.0 {
        ShapeModifyMode::Inflate
    } else {
        ShapeModifyMode::Deflate
    };
    let off = offset_shape(&sp, mode, d.abs() as f32)?;
    Ok(off
        .vertices()
        .iter()
        .map(|p| Point::new(p.0 as f64, p.1 as f64))
        .collect())
}

pub fn container_from_ring(
    importer: &Importer,
    id: u64,
    outer: &[Point],
    holes: &[Vec<Point>],
) -> Result<Container> {
    let zones = holes
        .iter()
        .map(|h| ExtQualityZone {
            quality: 0,
            shape: ExtShape::SimplePolygon(ext_poly(h)),
        })
        .collect();
    let ext = ExtContainer {
        id,
        shape: ExtShape::SimplePolygon(ext_poly(outer)),
        zones,
    };
    importer.import_container(&ext)
}

pub fn prepare(job: &NestJob) -> Result<Prepared> {
    let s = &job.settings;
    let tol = s.curve_tolerance.max(0.01);
    let importer = importer(s)?;

    let mut items = Vec::new();
    let mut skipped = Vec::new();
    let mut rings: Vec<Option<Vec<Point>>> = Vec::with_capacity(job.parts.len());
    for (pi, part) in job.parts.iter().enumerate() {
        if part.quantity == 0 {
            rings.push(None);
            continue;
        }
        let ring = match outer_ring(part, tol) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("{e:#}");
                skipped.push((pi, part.quantity as usize));
                rings.push(None);
                continue;
            }
        };
        rings.push(Some(ring));
    }

    let mut demand: Vec<usize> = job.parts.iter().map(|p| p.quantity as usize).collect();
    for (pi, _) in &skipped {
        demand[*pi] = 0;
    }

    // Part-in-part: fill holes of larger parts with smaller ones before the main nest.
    let composites = if s.part_in_part {
        crate::pip::fill_holes(job, &importer, &rings, &mut demand, tol)?
    } else {
        Vec::new()
    };

    for (pi, part) in job.parts.iter().enumerate() {
        let Some(ring) = &rings[pi] else { continue };
        let rot = part_rotation(part, s);
        let outer_area = ring_area(ring).abs();
        for comp in composites.iter().filter(|c| c.parent == pi) {
            let idx = items.len();
            let item = import_item(&importer, idx, ring, rot, part.allow_mirror)
                .with_context(|| format!("Part \"{}\"", part.name))?;
            let child_area: f64 = comp
                .children
                .iter()
                .map(|c| {
                    rings[c.part_idx]
                        .as_ref()
                        .map_or(0.0, |r| ring_area(r).abs())
                })
                .sum();
            items.push(ItemType {
                item,
                part_idx: pi,
                children: comp.children.clone(),
                demand: comp.count,
                area: outer_area + child_area,
            });
        }
        if demand[pi] > 0 {
            let idx = items.len();
            match import_item(&importer, idx, ring, rot, part.allow_mirror) {
                Ok(item) => items.push(ItemType {
                    item,
                    part_idx: pi,
                    children: Vec::new(),
                    demand: demand[pi],
                    area: outer_area,
                }),
                Err(e) => {
                    log::warn!("Part \"{}\": {e:#}", part.name);
                    skipped.push((pi, demand[pi]));
                }
            }
        }
    }

    let mut bins = Vec::new();
    let delta = s.spacing.max(0.0) - s.edge_margin.max(0.0);
    for (si, stock) in job.stock.iter().enumerate() {
        if stock.quantity == 0 {
            continue;
        }
        let outline = flatten_ring(&stock.outline, tol);
        if outline.len() < 3 {
            log::warn!("Stock \"{}\" has no closed outline", stock.name);
            continue;
        }
        let outline = simple_ring(&outline, &format!("Stock \"{}\"", stock.name))?;
        let outline = offset_ring(&outline, delta).with_context(|| {
            format!("Stock \"{}\" is too small for the edge margin", stock.name)
        })?;
        let mut holes = Vec::new();
        for d in &stock.defects {
            for (ring, closed) in flatten(d, tol) {
                if closed && ring.len() >= 3 {
                    match simple_ring(&ring, &format!("Defect on \"{}\"", stock.name)) {
                        Ok(r) => holes.push(r),
                        Err(e) => log::warn!("{e:#}"),
                    }
                }
            }
        }
        let container = match container_from_ring(&importer, si as u64, &outline, &holes) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Stock \"{}\": {e:#}", stock.name);
                continue;
            }
        };
        let area = stock.area();
        let free = s.remnants_first && stock.kind == StockKind::Remnant;
        // Free stock adds nothing to cost; the compactness term still prefers fewer remnants.
        let weight = if free { 0.0 } else { 1.0 };
        bins.push(BinType {
            container,
            outer_ring: outline,
            hole_rings: holes,
            stock_idx: si,
            qty: stock.quantity as usize,
            cost: area * weight,
            area,
            free,
        });
    }

    // Remnants first (when asked), then smaller before larger.
    let mut bin_order: Vec<usize> = (0..bins.len()).collect();
    bin_order.sort_by(|&a, &b| {
        let ka = job.stock[bins[a].stock_idx].kind == StockKind::Remnant && s.remnants_first;
        let kb = job.stock[bins[b].stock_idx].kind == StockKind::Remnant && s.remnants_first;
        kb.cmp(&ka).then(bins[a].area.total_cmp(&bins[b].area))
    });

    Ok(Prepared {
        importer,
        items,
        bins,
        bin_order,
        skipped,
    })
}
