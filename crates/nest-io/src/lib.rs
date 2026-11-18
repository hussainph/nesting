//! File import and export.
//!
//! Importers turn SVG, DXF and AI/PDF files into parts: closed outlines with their holes and
//! marks, in millimetres, y-up, with each part's bounding box at the origin. Exporters write one
//! nested sheet as SVG or DXF for the laser or router software.

mod assemble;
mod dxf_in;
mod export;
mod pdf_in;
mod ps_in;
mod svg_in;

use anyhow::{Context, Result, bail};
use kurbo::BezPath;
use nest_core::PartGeometry;
use std::path::Path;

pub use assemble::assemble_parts;
pub use export::{export_dxf, export_svg};

/// Tolerance, in mm, used when flattening curves for containment tests during import.
pub const IMPORT_TOLERANCE: f64 = 0.02;

/// One part found in a file.
#[derive(Clone, Debug)]
pub struct Imported {
    pub name: String,
    /// Normalized geometry: mm, y-up, bounding box at the origin.
    pub geometry: PartGeometry,
}

#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub parts: Vec<Imported>,
    pub warnings: Vec<String>,
}

/// Imports a file, picking the reader from its extension (.svg, .dxf, .ai, .pdf).
/// Parts are named after the file: "<stem> 1", "<stem> 2", and so on.
pub fn import_path(path: &Path) -> Result<ImportReport> {
    let data = std::fs::read(path).with_context(|| format!("Couldn't open {}", path.display()))?;
    let file_name = display_name(path);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Part".into());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let raw = match ext.as_str() {
        "svg" | "svgz" => svg_in::read(&data),
        "dxf" => dxf_in::read(&data),
        "ai" | "pdf" | "eps" => read_ai(&data),
        _ => bail!("{file_name} isn't a supported file. Use AI, PDF, EPS, DXF or SVG."),
    }
    .with_context(|| format!("Couldn't read {file_name}"))?;
    Ok(finish(raw, &stem, &file_name))
}

pub fn import_svg(data: &[u8]) -> Result<ImportReport> {
    Ok(finish(svg_in::read(data)?, "Part", "the file"))
}

pub fn import_dxf(data: &[u8]) -> Result<ImportReport> {
    Ok(finish(dxf_in::read(data)?, "Part", "the file"))
}

/// Reads Illustrator files (with or without PDF compatibility), Illustrator EPS, and PDFs.
pub fn import_ai(data: &[u8]) -> Result<ImportReport> {
    Ok(finish(read_ai(data)?, "Part", "the file"))
}

fn read_ai(data: &[u8]) -> Result<RawImport> {
    if pdf_in::is_pdf(data) {
        pdf_in::read(data)
    } else if ps_in::is_postscript(data) {
        ps_in::read(data)
    } else {
        bail!("It isn't an Illustrator, EPS or PDF file.")
    }
}

/// An error's causes after its first line, for showing under a heading that already says
/// what failed: "Couldn't read a.ai" / "It isn't an Illustrator, EPS or PDF file."
pub fn error_detail(e: &anyhow::Error) -> String {
    let causes: Vec<String> = e.chain().skip(1).map(|c| c.to_string()).collect();
    if causes.is_empty() { e.to_string() } else { causes.join(": ") }
}

/// Reads a remnant outline from a file: its largest closed contour, normalized.
pub fn import_outline(path: &Path) -> Result<BezPath> {
    let report = import_path(path)?;
    let largest = report
        .parts
        .into_iter()
        .max_by(|a, b| {
            a.geometry
                .outer
                .area()
                .abs()
                .total_cmp(&b.geometry.outer.area().abs())
        })
        .with_context(|| {
            format!(
                "{} has no closed outline. Draw the remnant as one closed shape.",
                display_name(path)
            )
        })?;
    Ok(largest.geometry.normalized().outer)
}

/// Raw output of a file reader: paths in mm, y-up, in file coordinates.
pub(crate) struct RawImport {
    /// Groups of paths assembled independently (one per PDF page; one for SVG and DXF).
    pub groups: Vec<Vec<BezPath>>,
    pub warnings: Vec<String>,
}

fn finish(raw: RawImport, stem: &str, file_name: &str) -> ImportReport {
    let mut report = ImportReport {
        warnings: raw
            .warnings
            .iter()
            .map(|w| w.replace("{file}", file_name))
            .collect(),
        ..Default::default()
    };
    let mut n = 0;
    for group in raw.groups {
        let (parts, warnings) = assemble::assemble(group, IMPORT_TOLERANCE);
        report
            .warnings
            .extend(warnings.into_iter().map(|w| w.replace("{file}", file_name)));
        for geometry in parts {
            n += 1;
            report.parts.push(Imported {
                name: format!("{stem} {n}"),
                geometry: geometry.normalized(),
            });
        }
    }
    if report.parts.is_empty() {
        report.warnings.push(format!(
            "No closed shapes found in {file_name}. Parts need closed outlines."
        ));
    }
    report
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

use kurbo::Shape as _;
