//! Writes one nested sheet as SVG or DXF.
//!
//! Both put the stock outline, the part cuts and the marks on separate layers. SVG layers are
//! colours (blue stock, black cuts, red marks), which is how LightBurn assigns layers. DXF uses
//! layers named SHEET, CUT and MARK.

use anyhow::{Context, Result};
use dxf::LwPolylineVertex;
use dxf::entities::{Entity, EntityType, LwPolyline};
use dxf::enums::{AcadVersion, Units};
use dxf::tables::Layer;
use dxf::{Color, Drawing};
use kurbo::{Affine, BezPath, PathEl, Rect};
use nest_core::geom::flatten;
use nest_core::{NestJob, SheetLayout, Stock};
use std::fmt::Write as _;

/// Max distance, in mm, between a curve and the polyline written to DXF.
pub const DXF_TOLERANCE: f64 = 0.01;

const SHEET_COLOR: &str = "#0000ff";
const CUT_COLOR: &str = "#000000";
const MARK_COLOR: &str = "#ff0000";

/// Paths of one sheet, in sheet coordinates (mm, y-up).
struct SheetPaths<'a> {
    stock: Option<&'a Stock>,
    bbox: Rect,
    /// (part name, outer + holes)
    cuts: Vec<(String, Vec<BezPath>)>,
    marks: Vec<BezPath>,
}

fn collect<'a>(job: &'a NestJob, sheet: &SheetLayout) -> SheetPaths<'a> {
    let stock = job.stock.iter().find(|s| s.id == sheet.stock_id);
    let mut cuts = Vec::new();
    let mut marks = Vec::new();
    let mut bbox = stock.map(|s| s.bbox());
    for placement in &sheet.placements {
        let Some(part) = job.parts.iter().find(|p| p.id == placement.part_id) else {
            continue;
        };
        let g = part.geometry.transformed(placement.affine());
        let b = g.bbox();
        bbox = Some(bbox.map_or(b, |a| a.union(b)));
        let mut paths = vec![g.outer];
        paths.extend(g.holes);
        cuts.push((part.name.clone(), paths));
        marks.extend(g.marks);
    }
    SheetPaths {
        stock,
        bbox: bbox.unwrap_or(Rect::ZERO),
        cuts,
        marks,
    }
}

/// One sheet as an SVG document in mm. Curves stay curves.
pub fn export_svg(job: &NestJob, sheet: &SheetLayout) -> String {
    let s = collect(job, sheet);
    let b = s.bbox;
    let (w, h) = (b.width(), b.height());
    // y-up sheet coordinates → SVG y-down, with the sheet's corner at the origin.
    let to_svg = Affine::new([1.0, 0.0, 0.0, -1.0, -b.x0, b.y1]);

    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}mm" height="{}mm" viewBox="0 0 {} {}">"#,
        num(w),
        num(h),
        num(w),
        num(h)
    );
    let group = |out: &mut String, id: &str, color: &str| {
        let _ = writeln!(
            out,
            r#"  <g id="{id}" fill="none" stroke="{color}" stroke-width="0.1">"#
        );
    };

    if let Some(stock) = s.stock {
        group(&mut out, "sheet", SHEET_COLOR);
        let mut d = path_data(&(to_svg * stock.outline.clone()));
        for defect in &stock.defects {
            d.push(' ');
            d.push_str(&path_data(&(to_svg * defect.clone())));
        }
        let _ = writeln!(
            out,
            r#"    <path data-name="{}" d="{d}"/>"#,
            xml_escape(&stock.name)
        );
        out.push_str("  </g>\n");
    }

    group(&mut out, "cut", CUT_COLOR);
    for (name, paths) in &s.cuts {
        let d: Vec<String> = paths
            .iter()
            .map(|p| path_data(&(to_svg * p.clone())))
            .collect();
        let _ = writeln!(
            out,
            r#"    <path data-part="{}" d="{}"/>"#,
            xml_escape(name),
            d.join(" ")
        );
    }
    out.push_str("  </g>\n");

    if !s.marks.is_empty() {
        group(&mut out, "mark", MARK_COLOR);
        for m in &s.marks {
            let _ = writeln!(
                out,
                r#"    <path d="{}"/>"#,
                path_data(&(to_svg * m.clone()))
            );
        }
        out.push_str("  </g>\n");
    }
    out.push_str("</svg>\n");
    out
}

/// One sheet as DXF (R2000, mm). Curves are written as polylines within [`DXF_TOLERANCE`].
pub fn export_dxf(job: &NestJob, sheet: &SheetLayout) -> Result<String> {
    let s = collect(job, sheet);
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = Units::Millimeters;
    for (name, color) in [("SHEET", 5u8), ("CUT", 7), ("MARK", 1)] {
        d.add_layer(Layer {
            name: name.into(),
            color: Color::from_index(color),
            ..Default::default()
        });
    }
    let mut add = |path: &BezPath, layer: &str| {
        for (pts, closed) in flatten(path, DXF_TOLERANCE) {
            let mut poly = LwPolyline {
                vertices: pts
                    .iter()
                    .map(|p| LwPolylineVertex {
                        x: p.x,
                        y: p.y,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            poly.set_is_closed(closed);
            let mut e = Entity::new(EntityType::LwPolyline(poly));
            e.common.layer = layer.into();
            d.add_entity(e);
        }
    };
    if let Some(stock) = s.stock {
        add(&stock.outline, "SHEET");
        for defect in &stock.defects {
            add(defect, "SHEET");
        }
    }
    for (_, paths) in &s.cuts {
        for p in paths {
            add(p, "CUT");
        }
    }
    for m in &s.marks {
        add(m, "MARK");
    }
    let mut buf = Vec::new();
    d.save(&mut buf).context("Couldn't write the DXF")?;
    String::from_utf8(buf).context("Couldn't write the DXF")
}

fn path_data(path: &BezPath) -> String {
    let mut d = String::new();
    for el in path.elements() {
        if !d.is_empty() {
            d.push(' ');
        }
        match *el {
            PathEl::MoveTo(p) => {
                let _ = write!(d, "M{} {}", num(p.x), num(p.y));
            }
            PathEl::LineTo(p) => {
                let _ = write!(d, "L{} {}", num(p.x), num(p.y));
            }
            PathEl::QuadTo(a, p) => {
                let _ = write!(d, "Q{} {} {} {}", num(a.x), num(a.y), num(p.x), num(p.y));
            }
            PathEl::CurveTo(a, b, p) => {
                let _ = write!(
                    d,
                    "C{} {} {} {} {} {}",
                    num(a.x),
                    num(a.y),
                    num(b.x),
                    num(b.y),
                    num(p.x),
                    num(p.y)
                );
            }
            PathEl::ClosePath => d.push('Z'),
        }
    }
    d
}

/// Up to 4 decimals (0.1 µm), trailing zeros trimmed.
fn num(v: f64) -> String {
    let v = if v.abs() < 5e-5 { 0.0 } else { v };
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{import_dxf, import_svg};
    use kurbo::{Circle, Shape};
    use nest_core::{Part, PartGeometry, Placement, Stock};

    fn job() -> (NestJob, SheetLayout) {
        // A ring (curved, with a hole) and a mark inside it.
        let mut mark = BezPath::new();
        mark.move_to((15.0, 20.0));
        mark.line_to((25.0, 20.0));
        let ring = PartGeometry {
            outer: Circle::new((20.0, 20.0), 20.0).to_path(1e-4),
            holes: vec![Circle::new((20.0, 20.0), 8.0).to_path(1e-4)],
            marks: vec![mark],
        }
        .normalized();
        let part = Part {
            id: 1,
            name: "Ring <1>".into(),
            source: None,
            geometry: ring,
            quantity: 2,
            rotation: None,
            allow_mirror: false,
        };
        let stock = Stock::rectangle(7, "Acrylic 600x400", 600.0, 400.0);
        let sheet = SheetLayout {
            stock_id: 7,
            placements: vec![
                Placement {
                    part_id: 1,
                    mirrored: false,
                    rotation_deg: 0.0,
                    translation: (10.0, 10.0),
                },
                Placement {
                    part_id: 1,
                    mirrored: true,
                    rotation_deg: 90.0,
                    translation: (200.0, 100.0),
                },
            ],
            utilization: 0.0,
        };
        (
            NestJob {
                parts: vec![part],
                stock: vec![stock],
                ..Default::default()
            },
            sheet,
        )
    }

    fn strip_group(svg: &str, id: &str) -> String {
        let start = svg.find(&format!(r#"<g id="{id}""#)).unwrap();
        let end = start + svg[start..].find("</g>").unwrap() + 4;
        format!("{}{}", &svg[..start], &svg[end..])
    }

    #[test]
    fn svg_round_trip_keeps_area_and_size() {
        let (job, sheet) = job();
        let svg = export_svg(&job, &sheet);
        assert!(svg.contains(r#"width="600mm" height="400mm""#), "{svg}");
        assert!(svg.contains(" C"), "curves stay curves");
        assert!(svg.contains("Ring &lt;1&gt;"));
        // Without the sheet outline the parts come back as parts.
        let r = import_svg(strip_group(&svg, "sheet").as_bytes()).unwrap();
        assert_eq!(r.parts.len(), 2, "{:?}", r.warnings);
        let expected = std::f64::consts::PI * (400.0 - 64.0);
        for p in &r.parts {
            assert!(
                (p.geometry.area() - expected).abs() < 0.05,
                "{}",
                p.geometry.area()
            );
            let b = p.geometry.bbox();
            assert!(
                (b.width() - 40.0).abs() < 1e-3 && (b.height() - 40.0).abs() < 1e-3,
                "{b:?}"
            );
            assert_eq!(p.geometry.marks.len(), 1);
            assert_eq!(p.geometry.holes.len(), 1);
        }
    }

    #[test]
    fn svg_keeps_positions() {
        // The first ring sits 10 mm in from the left and bottom edges of a 600x400 sheet.
        let (job, sheet) = job();
        let svg = export_svg(&job, &sheet);
        // In SVG (y-down) its circle starts at x = 10 + 40 = 50 (rightmost point), y = 400 - 30 = 370.
        assert!(svg.contains("M50 370"), "{svg}");
    }

    #[test]
    fn dxf_round_trip() {
        let (job, sheet) = job();
        let dxf = export_dxf(&job, &sheet).unwrap();
        assert!(dxf.contains("AC1015"));
        assert!(dxf.contains("$INSUNITS"));
        let r = import_dxf(dxf.as_bytes()).unwrap();
        // The sheet outline holds both rings, so they come back as holes of the sheet,
        // and their own holes come back as island parts.
        let sheet_part = r
            .parts
            .iter()
            .find(|p| p.geometry.bbox().width() > 500.0)
            .unwrap();
        assert_eq!(sheet_part.geometry.holes.len(), 2);
        assert!((sheet_part.geometry.bbox().height() - 400.0).abs() < 1e-6);
        let ring_area = std::f64::consts::PI * 400.0;
        // Flattening at 0.01 mm loses a sliver along the 126 mm perimeter.
        for h in &sheet_part.geometry.holes {
            assert!((h.area().abs() - ring_area).abs() < 1.0, "{}", h.area());
        }
    }

    #[test]
    fn number_format() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(-0.00001), "0");
        assert_eq!(num(2.50006), "2.5001");
        assert_eq!(num(-3.25), "-3.25");
    }
}
