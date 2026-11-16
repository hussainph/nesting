//! Writes the sample files in `samples/`. Run from the workspace root:
//! `cargo run -p nest-io --example make_samples`
//!
//! - `arabic.svg`: Arabic words outlined with a system font, plus a ring and a star.
//! - `bracket.dxf`: loose lines and an arc that need chaining, a circle hole, and a polyline
//!   with bulges.
//! - `shapes.ai`: a PDF with vector paths, standing in for an Illustrator file saved with PDF
//!   compatibility (a real .ai can only come from Illustrator).

use anyhow::{Context, Result};
use dxf::entities::*;
use dxf::{Drawing, LwPolylineVertex, Point};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use std::f64::consts::FRAC_1_SQRT_2;
use std::sync::Arc;

fn main() -> Result<()> {
    let dir = std::path::Path::new("samples");
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("arabic.svg"), arabic_svg()?)?;
    std::fs::write(dir.join("bracket.dxf"), bracket_dxf()?)?;
    std::fs::write(dir.join("shapes.ai"), shapes_pdf()?)?;
    for name in ["arabic.svg", "bracket.dxf", "shapes.ai"] {
        let r = nest_io::import_path(&dir.join(name))?;
        println!("{name}: {} parts", r.parts.len());
        for w in &r.warnings {
            println!("  warning: {w}");
        }
    }
    Ok(())
}

/// Outlines the text with system fonts and writes paths only, like "Create Outlines" would.
fn arabic_svg() -> Result<String> {
    let src = r##"<svg xmlns="http://www.w3.org/2000/svg" width="300mm" height="200mm" viewBox="0 0 300 200">
  <g font-family="Geeza Pro, Al Nile, Baghdad, Damascus, Noto Naskh Arabic, Arial" font-size="40" fill="black">
    <text x="280" y="60" text-anchor="end" direction="rtl">بسم الله</text>
    <text x="280" y="130" text-anchor="end" direction="rtl">محمد</text>
  </g>
  <path d="M60 160 m-30 0 a30 30 0 1 0 60 0 a30 30 0 1 0 -60 0 Z M60 160 m-15 0 a15 15 0 1 1 30 0 a15 15 0 1 1 -30 0 Z" fill="black" fill-rule="evenodd"/>
  <path d="M150 130 L157 152 L180 152 L161 165 L168 188 L150 174 L132 188 L139 165 L120 152 L143 152 Z" fill="black"/>
</svg>"##;
    let mut opt = usvg::Options::default();
    Arc::make_mut(&mut opt.fontdb).load_system_fonts();
    let tree = usvg::Tree::from_data(src.as_bytes(), &opt).context("parse sample SVG")?;
    let out = tree.to_string(&usvg::WriteOptions::default());
    anyhow::ensure!(
        !out.contains("<text"),
        "text wasn't outlined; no Arabic font found"
    );
    Ok(out)
}

fn bracket_dxf() -> Result<Vec<u8>> {
    let mut d = Drawing::new();
    d.header.default_drawing_units = dxf::enums::Units::Millimeters;
    d.header.version = dxf::enums::AcadVersion::R2000;
    let line = |d: &mut Drawing, a: (f64, f64), b: (f64, f64)| {
        d.add_entity(Entity::new(EntityType::Line(Line::new(
            Point::new(a.0, a.1, 0.0),
            Point::new(b.0, b.1, 0.0),
        ))));
    };
    // Bracket 120 x 60 with a rounded right end, drawn as loose pieces in mixed order and direction.
    line(&mut d, (0.0, 0.0), (90.0, 0.0));
    line(&mut d, (0.0, 60.0), (0.0, 0.0));
    line(&mut d, (90.0, 60.0), (0.0, 60.0));
    d.add_entity(Entity::new(EntityType::Arc(dxf::entities::Arc::new(
        Point::new(90.0, 30.0, 0.0),
        30.0,
        270.0,
        90.0,
    ))));
    // Two mounting holes.
    for x in [25.0, 90.0] {
        d.add_entity(Entity::new(EntityType::Circle(Circle::new(
            Point::new(x, 30.0, 0.0),
            8.0,
        ))));
    }
    // A slot drawn as one closed polyline with bulged ends.
    let mut slot = LwPolyline::default();
    for (x, y, bulge) in [
        (45.0, 22.0, 0.0),
        (65.0, 22.0, 1.0),
        (65.0, 38.0, 0.0),
        (45.0, 38.0, 1.0),
    ] {
        slot.vertices.push(LwPolylineVertex {
            x,
            y,
            bulge,
            ..Default::default()
        });
    }
    slot.set_is_closed(true);
    d.add_entity(Entity::new(EntityType::LwPolyline(slot)));
    // A separate washer: circle with a circle hole.
    d.add_entity(Entity::new(EntityType::Circle(Circle::new(
        Point::new(170.0, 30.0, 0.0),
        25.0,
    ))));
    d.add_entity(Entity::new(EntityType::Circle(Circle::new(
        Point::new(170.0, 30.0, 0.0),
        10.0,
    ))));
    let mut buf = Vec::new();
    d.save(&mut buf)?;
    Ok(buf)
}

fn shapes_pdf() -> Result<Vec<u8>> {
    let op = |name: &str, args: &[f64]| {
        Operation::new(name, args.iter().map(|v| Object::Real(*v as f32)).collect())
    };
    let ops = vec![
        // Artboard clip, as Illustrator writes it.
        op("re", &[0.0, 0.0, 595.0, 842.0]),
        Operation::new("W", vec![]),
        op("n", &[]),
        // A rounded tag with a hole, stroked.
        op("m", &[100.0, 100.0]),
        op("l", &[260.0, 100.0]),
        op("c", &[290.0, 100.0, 290.0, 180.0, 260.0, 180.0]),
        op("l", &[100.0, 180.0]),
        op("h", &[]),
        op("m", &[250.0, 140.0]),
        op("c", &[250.0, 145.5, 254.5, 150.0, 260.0, 150.0]),
        op("c", &[265.5, 150.0, 270.0, 145.5, 270.0, 140.0]),
        op("c", &[270.0, 134.5, 265.5, 130.0, 260.0, 130.0]),
        op("c", &[254.5, 130.0, 250.0, 134.5, 250.0, 140.0]),
        op("h", &[]),
        op("S", &[]),
        // A filled square under a 45° rotation.
        op("q", &[]),
        op(
            "cm",
            &[
                FRAC_1_SQRT_2,
                FRAC_1_SQRT_2,
                -FRAC_1_SQRT_2,
                FRAC_1_SQRT_2,
                400.0,
                300.0,
            ],
        ),
        op("re", &[0.0, 0.0, 100.0, 100.0]),
        op("f", &[]),
        op("Q", &[]),
    ];
    let mut doc = Document::with_version("1.6");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }.encode()?,
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => dictionary! {},
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 },
        ),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut buf = Vec::new();
    doc.save_to(&mut buf)?;
    Ok(buf)
}
