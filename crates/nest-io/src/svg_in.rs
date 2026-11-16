//! SVG reader. usvg resolves units, viewBox, transforms, basic shapes and text into paths.

use crate::RawImport;
use anyhow::{Context, Result};
use kurbo::BezPath;
use std::sync::Arc;
use usvg::tiny_skia_path::{PathSegment, Transform};

const MM_PER_PX: f64 = 25.4 / 96.0;
/// Illustrator writes SVG user units as points.
const MM_PER_PT: f64 = 25.4 / 72.0;

pub(crate) fn read(data: &[u8]) -> Result<RawImport> {
    let mut opt = usvg::Options::default();
    let head = String::from_utf8_lossy(&data[..data.len().min(4096)]).into_owned();
    let has_text = memfind(data, b"<text");
    if has_text {
        Arc::make_mut(&mut opt.fontdb).load_system_fonts();
    }
    let tree = usvg::Tree::from_data(data, &opt).context("The SVG couldn't be parsed")?;

    let scale = if is_illustrator_px(&head) {
        MM_PER_PT
    } else {
        MM_PER_PX
    };
    let mut walker = Walker {
        paths: Vec::new(),
        images: 0,
        scale,
    };
    walker.group(tree.root());

    let mut warnings = Vec::new();
    if has_text {
        warnings.push(
            "Text in {file} was outlined with the fonts on this computer. Convert text to outlines before saving to be sure it matches."
                .to_string(),
        );
    }
    if walker.images > 0 {
        warnings
            .push("Images in {file} were skipped. Only vector shapes are imported.".to_string());
    }
    Ok(RawImport {
        groups: vec![walker.paths],
        warnings,
    })
}

struct Walker {
    paths: Vec<BezPath>,
    images: usize,
    scale: f64,
}

impl Walker {
    fn group(&mut self, g: &usvg::Group) {
        for node in g.children() {
            match node {
                usvg::Node::Group(g) => self.group(g),
                usvg::Node::Path(p) => {
                    if p.is_visible() && (p.fill().is_some() || p.stroke().is_some()) {
                        self.path(p.data(), p.abs_transform());
                    }
                }
                usvg::Node::Text(t) => self.group(t.flattened()),
                usvg::Node::Image(_) => self.images += 1,
            }
        }
    }

    fn path(&mut self, data: &usvg::tiny_skia_path::Path, t: Transform) {
        let s = self.scale;
        // Absolute transform, then px → mm and y-down → y-up.
        let map = |p: usvg::tiny_skia_path::Point| {
            let mut p = p;
            t.map_point(&mut p);
            kurbo::Point::new(p.x as f64 * s, -(p.y as f64) * s)
        };
        let mut out = BezPath::new();
        for seg in data.segments() {
            match seg {
                PathSegment::MoveTo(p) => out.move_to(map(p)),
                PathSegment::LineTo(p) => out.line_to(map(p)),
                PathSegment::QuadTo(a, p) => out.quad_to(map(a), map(p)),
                PathSegment::CubicTo(a, b, p) => out.curve_to(map(a), map(b), map(p)),
                PathSegment::Close => out.close_path(),
            }
        }
        if !out.elements().is_empty() {
            self.paths.push(out);
        }
    }
}

fn memfind(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Illustrator SVGs without real units on the root `width` use 72 units per inch.
fn is_illustrator_px(head: &str) -> bool {
    if !head.contains("Adobe Illustrator") {
        return false;
    }
    let Some(start) = head.find("<svg") else {
        return true;
    };
    let tag = &head[start..];
    let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
    let width = attr(tag, "width");
    match width {
        None => true,
        Some(w) => {
            let w = w.trim();
            !(w.ends_with("mm")
                || w.ends_with("cm")
                || w.ends_with("in")
                || w.ends_with("pt")
                || w.ends_with("pc"))
        }
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let rest = &tag[i..];
    Some(&rest[..rest.find('"')?])
}

#[cfg(test)]
mod tests {
    use crate::import_svg;
    use kurbo::Shape;

    #[test]
    fn mm_units_and_y_flip() {
        // 100 x 50 mm document; a 20 x 10 mm rect near the top-left.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
            <rect x="10" y="5" width="20" height="10" fill="black"/>
            <rect x="10" y="30" width="5" height="5" fill="black"/>
        </svg>"#;
        let r = import_svg(svg).unwrap();
        assert_eq!(r.parts.len(), 2);
        let big = r
            .parts
            .iter()
            .find(|p| p.geometry.bbox().width() > 10.0)
            .unwrap();
        let b = big.geometry.bbox();
        assert!(
            (b.width() - 20.0).abs() < 1e-3 && (b.height() - 10.0).abs() < 1e-3,
            "{b:?}"
        );
        assert!(b.x0.abs() < 1e-9 && b.y0.abs() < 1e-9, "normalized: {b:?}");
    }

    #[test]
    fn px_are_96_per_inch() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96">
            <rect x="0" y="0" width="96" height="48" fill="black"/></svg>"#;
        let r = import_svg(svg).unwrap();
        let b = r.parts[0].geometry.bbox();
        assert!((b.width() - 25.4).abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn illustrator_px_are_points() {
        let svg = br#"<?xml version="1.0"?><!-- Generator: Adobe Illustrator 28.0.0 -->
            <svg xmlns="http://www.w3.org/2000/svg" width="72px" height="72px" viewBox="0 0 72 72">
            <rect x="0" y="0" width="72" height="36" fill="black"/></svg>"#;
        let r = import_svg(svg).unwrap();
        let b = r.parts[0].geometry.bbox();
        assert!((b.width() - 25.4).abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn y_is_flipped() {
        // A triangle pointing down in SVG (y-down) points down in y-up too: its apex is the lowest point.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm" viewBox="0 0 10 10">
            <path d="M0 0 L10 0 L5 10 Z" fill="black"/></svg>"#;
        let r = import_svg(svg).unwrap();
        let g = &r.parts[0].geometry;
        // Apex at SVG (5,10) is the bottom; in y-up after normalizing it sits at y = 0.
        let apex_low = g
            .outer
            .elements()
            .iter()
            .filter_map(|e| e.end_point())
            .any(|p| (p.x - 5.0).abs() < 1e-3 && p.y.abs() < 1e-3);
        assert!(apex_low, "{:?}", g.outer);
        assert!((g.outer.area().abs() - 50.0).abs() < 1e-3);
    }
}
