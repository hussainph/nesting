//! Shared types for the nesting app: parts, stock, settings, jobs and results.
//!
//! Conventions used everywhere in the workspace:
//! - Units are millimetres.
//! - Coordinates are y-up (like DXF and the nesting engine). Importers of y-down formats
//!   (SVG, PDF/AI pages are already y-up) flip on the way in; renderers flip on the way out.
//! - Geometry is kept as `kurbo::BezPath` so curves survive the round trip from import to export.
//!   The engine flattens to polygons only for collision checks.

pub mod geom;
pub mod library;

pub use kurbo;
use kurbo::{Affine, BezPath, Rect, Shape, Vec2};
use serde::{Deserialize, Serialize};

pub type PartId = u64;
pub type StockId = u64;

/// Closed contours and marks that make up one physical piece.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PartGeometry {
    /// The cut outline. One closed subpath.
    pub outer: BezPath,
    /// Closed contours inside `outer` (counters of letters, cutouts). Cut, but ignored for
    /// collision unless part-in-part nesting is on.
    pub holes: Vec<BezPath>,
    /// Open paths or engraving inside `outer`. They travel with the part and are exported, but
    /// play no role in nesting.
    pub marks: Vec<BezPath>,
}

impl PartGeometry {
    pub fn bbox(&self) -> Rect {
        self.outer.bounding_box()
    }

    /// Net area in mm² (outer minus holes).
    pub fn area(&self) -> f64 {
        let outer = self.outer.area().abs();
        let holes: f64 = self.holes.iter().map(|h| h.area().abs()).sum();
        (outer - holes).max(0.0)
    }

    pub fn transformed(&self, t: Affine) -> PartGeometry {
        PartGeometry {
            outer: t * self.outer.clone(),
            holes: self.holes.iter().map(|p| t * p.clone()).collect(),
            marks: self.marks.iter().map(|p| t * p.clone()).collect(),
        }
    }

    /// Every path (outer, holes, marks), in that order.
    pub fn all_paths(&self) -> impl Iterator<Item = &BezPath> {
        std::iter::once(&self.outer).chain(self.holes.iter()).chain(self.marks.iter())
    }

    /// Moves the geometry so its bounding box starts at the origin.
    pub fn normalized(&self) -> PartGeometry {
        let b = self.bbox();
        self.transformed(Affine::translate(Vec2::new(-b.x0, -b.y0)))
    }
}

/// Which rotations the engine may use for a part.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", content = "deg", rename_all = "snake_case")]
pub enum Rotation {
    /// Any angle. Best yield.
    #[default]
    Any,
    /// Multiples of this many degrees (90 gives 0/90/180/270).
    Step(f64),
    /// Keep the part as drawn. Use for grain or texture direction.
    Fixed,
}

impl Rotation {
    pub fn label(&self) -> String {
        match self {
            Rotation::Any => "Any angle".into(),
            Rotation::Step(d) => format!("{d}° steps"),
            Rotation::Fixed => "As drawn".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub id: PartId,
    pub name: String,
    /// File the part came from, for display.
    pub source: Option<String>,
    /// Geometry with its bounding box at the origin.
    pub geometry: PartGeometry,
    pub quantity: u32,
    /// `None` uses the job's default rotation.
    pub rotation: Option<Rotation>,
    pub allow_mirror: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StockKind {
    #[default]
    Sheet,
    Remnant,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stock {
    pub id: StockId,
    pub name: String,
    pub kind: StockKind,
    /// Usable outline. One closed subpath, bounding box at the origin.
    pub outline: BezPath,
    /// Areas inside the outline that can't be used (holes already cut, burns, warps).
    pub defects: Vec<BezPath>,
    pub quantity: u32,
    pub material: String,
    /// Sheet thickness in mm. 0 when unknown.
    pub thickness: f64,
}

impl Stock {
    pub fn rectangle(id: StockId, name: impl Into<String>, width: f64, height: f64) -> Stock {
        Stock {
            id,
            name: name.into(),
            kind: StockKind::Sheet,
            outline: Rect::new(0.0, 0.0, width, height).to_path(0.01),
            defects: Vec::new(),
            quantity: 1,
            material: String::new(),
            thickness: 0.0,
        }
    }

    pub fn bbox(&self) -> Rect {
        self.outline.bounding_box()
    }

    /// Usable area in mm².
    pub fn area(&self) -> f64 {
        let outer = self.outline.area().abs();
        let defects: f64 = self.defects.iter().map(|d| d.area().abs()).sum();
        (outer - defects).max(0.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NestSettings {
    /// Gap between parts, in mm. Cover the kerf plus whatever the material needs.
    pub spacing: f64,
    /// Gap between parts and the stock edge, in mm.
    pub edge_margin: f64,
    pub default_rotation: Rotation,
    /// Search time budget in seconds. The engine returns its best result when it runs out.
    pub time_limit_secs: f64,
    /// Max distance, in mm, between a curve and the polygon used for collision checks.
    pub curve_tolerance: f64,
    /// Fill remnants before starting full sheets.
    pub remnants_first: bool,
    /// Place small parts inside the holes of larger ones.
    pub part_in_part: bool,
    /// Fixed seed for repeatable runs. `None` picks one at random.
    pub seed: Option<u64>,
}

impl Default for NestSettings {
    fn default() -> Self {
        NestSettings {
            spacing: 2.0,
            edge_margin: 5.0,
            default_rotation: Rotation::Any,
            time_limit_secs: 30.0,
            curve_tolerance: 0.1,
            remnants_first: true,
            part_in_part: false,
            seed: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NestJob {
    pub parts: Vec<Part>,
    pub stock: Vec<Stock>,
    pub settings: NestSettings,
}

/// Placement of one part copy: optional mirror across the x-axis (y → -y), then rotation about
/// the origin, then translation. Applied to the part's stored geometry, it gives sheet coordinates.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Placement {
    pub part_id: PartId,
    pub mirrored: bool,
    pub rotation_deg: f64,
    pub translation: (f64, f64),
}

impl Placement {
    pub fn affine(&self) -> Affine {
        let mirror = if self.mirrored { Affine::FLIP_Y } else { Affine::IDENTITY };
        Affine::translate(Vec2::new(self.translation.0, self.translation.1))
            * Affine::rotate(self.rotation_deg.to_radians())
            * mirror
    }
}

/// One used copy of a stock item and the parts placed on it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SheetLayout {
    pub stock_id: StockId,
    pub placements: Vec<Placement>,
    /// Placed part area divided by usable stock area, 0..1.
    pub utilization: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NestResult {
    pub sheets: Vec<SheetLayout>,
    /// Parts that didn't fit, with how many copies are missing.
    pub unplaced: Vec<(PartId, u32)>,
    pub elapsed_secs: f64,
}

impl NestResult {
    pub fn placed_count(&self) -> usize {
        self.sheets.iter().map(|s| s.placements.len()).sum()
    }

    pub fn unplaced_count(&self) -> u32 {
        self.unplaced.iter().map(|(_, n)| n).sum()
    }

    /// Placed area over the area of the stock used, 0..1.
    pub fn utilization(&self, job: &NestJob) -> f64 {
        let mut placed = 0.0;
        let mut used = 0.0;
        for sheet in &self.sheets {
            if let Some(stock) = job.stock.iter().find(|s| s.id == sheet.stock_id) {
                used += stock.area();
            }
            for p in &sheet.placements {
                if let Some(part) = job.parts.iter().find(|q| q.id == p.part_id) {
                    placed += part.geometry.area();
                }
            }
        }
        if used > 0.0 { placed / used } else { 0.0 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}
