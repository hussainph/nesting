//! Named states for headless snapshots (`rainbownest --snap out.png --scene result`).

use kurbo::{BezPath, Circle, Point, RoundedRect, Shape};
use nest_core::library::Library;
use nest_core::{NestResult, PartGeometry, Placement, SheetLayout, Stock, StockKind};

use crate::state::App;

pub const SCENES: &[(&str, fn(&mut App))] = &[
    ("empty", empty),
    ("parts", with_parts),
    ("result", result),
    ("running", running),
    ("select-part", select_part),
    ("select-sheet", select_sheet),
    ("update-ready", update_ready),
    ("add-remnant", add_remnant),
    ("edit-remnant", draw_remnant),
    ("arabic", arabic),
    ("engine", engine),
];

fn star(r1: f64, r2: f64, n: usize) -> BezPath {
    let pts: Vec<Point> = (0..n * 2)
        .map(|i| {
            let r = if i % 2 == 0 { r1 } else { r2 };
            let a = i as f64 / (n * 2) as f64 * std::f64::consts::TAU;
            Point::new(r1 + r * a.sin(), r1 + r * a.cos())
        })
        .collect();
    nest_core::geom::polygon_path(&pts)
}

fn demo_library(s: &mut App) {
    let sheet = Stock { material: "Acrylic".into(), thickness: 3.0, ..Stock::rectangle(1, "Acrylic 1200 × 900", 1200.0, 900.0) };
    let remnant = Stock {
        id: 2,
        name: "Offcut from job 214".into(),
        kind: StockKind::Remnant,
        outline: nest_core::geom::polygon_path(&[
            Point::new(0.0, 0.0),
            Point::new(620.0, 0.0),
            Point::new(620.0, 260.0),
            Point::new(380.0, 260.0),
            Point::new(380.0, 480.0),
            Point::new(0.0, 480.0),
        ]),
        defects: vec![Circle::new((120.0, 360.0), 40.0).to_path(0.1)],
        quantity: 1,
        material: "Acrylic".into(),
        thickness: 3.0,
    };
    s.library = Library { stock: vec![remnant, sheet] };
    s.use_stock = vec![2, 1];
    s.library_path = std::env::temp_dir().join("nesting-scene-library.json");
    s.refresh_stock_flats();
}

fn demo_parts(s: &mut App) {
    let ring = PartGeometry { outer: Circle::new((60.0, 60.0), 60.0).to_path(0.1), holes: vec![Circle::new((60.0, 60.0), 32.0).to_path(0.1)], marks: vec![] };
    let plate = PartGeometry {
        outer: RoundedRect::new(0.0, 0.0, 180.0, 90.0, 12.0).to_path(0.1),
        holes: vec![Circle::new((24.0, 45.0), 8.0).to_path(0.1), Circle::new((156.0, 45.0), 8.0).to_path(0.1)],
        marks: vec![],
    };
    let l = PartGeometry {
        outer: nest_core::geom::polygon_path(&[
            Point::new(0.0, 0.0),
            Point::new(140.0, 0.0),
            Point::new(140.0, 40.0),
            Point::new(40.0, 40.0),
            Point::new(40.0, 120.0),
            Point::new(0.0, 120.0),
        ]),
        ..Default::default()
    };
    let st = PartGeometry { outer: star(55.0, 24.0, 5), ..Default::default() };
    let dot = PartGeometry { outer: Circle::new((7.0, 7.0), 7.0).to_path(0.1), ..Default::default() };
    s.add_part("Ring", ring, 6);
    s.add_part("Name plate", plate, 8);
    s.add_part("Bracket", l, 10);
    s.add_part("Star", st, 5);
    s.add_part("Dot", dot, 24);
    for p in &mut s.parts {
        p.source = Some("demo.ai".into());
    }
}

fn empty(s: &mut App) {
    demo_library(s);
}

fn with_parts(s: &mut App) {
    demo_library(s);
    demo_parts(s);
}

/// A shelf-packed stand-in result, so the board can be checked without running the engine.
fn fake_result(s: &App) -> NestResult {
    let job = s.job();
    let mut sheets: Vec<SheetLayout> = job.stock.iter().map(|st| SheetLayout { stock_id: st.id, placements: vec![], utilization: 0.0 }).collect();
    let mut unplaced = Vec::new();
    let spacing = 6.0;
    let mut cursor: Vec<(f64, f64, f64)> = vec![(10.0, 10.0, 0.0); sheets.len()];
    for part in &job.parts {
        let b = part.geometry.bbox();
        let mut missing = 0;
        'copies: for _ in 0..part.quantity {
            for (i, st) in job.stock.iter().enumerate() {
                let sb = st.bbox();
                let limit_w = if st.kind == StockKind::Remnant { 370.0 } else { sb.width() - 10.0 };
                let (mut x, mut y, mut row_h) = cursor[i];
                if x + b.width() > limit_w {
                    x = 10.0;
                    y += row_h + spacing;
                    row_h = 0.0;
                }
                if y + b.height() > sb.height() - 10.0 || (st.kind == StockKind::Remnant && y < 170.0 && x + b.width() > 300.0 && y + b.height() > 300.0) {
                    continue;
                }
                sheets[i].placements.push(Placement { part_id: part.id, mirrored: false, rotation_deg: 0.0, translation: (x - b.x0, y - b.y0) });
                cursor[i] = (x + b.width() + spacing, y, row_h.max(b.height()));
                continue 'copies;
            }
            missing += 1;
        }
        if missing > 0 {
            unplaced.push((part.id, missing));
        }
    }
    for sh in &mut sheets {
        let st = job.stock.iter().find(|x| x.id == sh.stock_id).unwrap();
        let placed: f64 = sh.placements.iter().map(|p| job.parts.iter().find(|q| q.id == p.part_id).unwrap().geometry.area()).sum();
        sh.utilization = placed / st.area();
    }
    sheets.retain(|s| !s.placements.is_empty());
    NestResult { sheets, unplaced, elapsed_secs: 12.0 }
}

fn result(s: &mut App) {
    with_parts(s);
    s.parts.iter_mut().find(|p| p.name == "Dot").unwrap().quantity = 400;
    let r = fake_result(s);
    s.result_job = Some(s.job());
    s.result = Some(r);
    s.selected = s.parts.iter().find(|p| p.name == "Ring").map(|p| p.id);
    s.fit_pending = true;
}

fn select_part(s: &mut App) {
    result(s);
    s.selected = None;
    let r = s.result.as_ref().unwrap();
    let at = r
        .sheets
        .iter()
        .enumerate()
        .find_map(|(si, sh)| sh.placements.iter().position(|p| s.part(p.part_id).is_some_and(|q| q.name == "Name plate")).map(|pi| (si, pi)))
        .unwrap();
    s.set_sel(crate::layout::BoardSel::Parts(vec![at]));
}

fn select_sheet(s: &mut App) {
    result(s);
    s.selected = None;
    s.set_sel(crate::layout::BoardSel::Sheet(0));
}

fn update_ready(s: &mut App) {
    result(s);
    s.update_ready = Some("0.1.3".into());
    s.notify("Updated to 0.1.3", Some("Restart RainbowNest to use it.".into()), false);
}

fn running(s: &mut App) {
    result(s);
    s.run = Some(crate::state::Run { ctl: Default::default(), started: std::time::Instant::now() });
}

fn add_remnant(s: &mut App) {
    with_parts(s);
    s.open_editor(None);
}

fn draw_remnant(s: &mut App) {
    with_parts(s);
    s.open_editor(Some(2));
    let e = s.editor.as_mut().unwrap();
    e.select(crate::editor::Sel::Vertex(3));
    e.hover = crate::editor::Hit::Mid(1);
}

/// Real imports from `samples/`, nested with the stand-in packer.
fn arabic(s: &mut App) {
    demo_library(s);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples");
    for f in ["arabic.svg", "bracket.dxf", "shapes.ai"] {
        if let Ok(report) = nest_io::import_path(&dir.join(f)) {
            for p in report.parts {
                s.add_part(&p.name, p.geometry, 3);
            }
        }
    }
    let r = fake_result(s);
    s.result_job = Some(s.job());
    s.result = Some(r);
}

/// The real engine on the demo job, run to completion before the first frame.
fn engine(s: &mut App) {
    with_parts(s);
    for f in ["arabic.svg"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples");
        if let Ok(report) = nest_io::import_path(&dir.join(f)) {
            for p in report.parts {
                s.add_part(&p.name, p.geometry, 4);
            }
        }
    }
    s.settings.time_limit_secs = 6.0;
    s.settings.seed = Some(1);
    let job = s.job();
    let r = nest_engine::nest(&job, &nest_engine::Control::default(), |_| {}).expect("nest");
    for v in nest_engine::validate::check(&job, &r) {
        eprintln!("invalid: {v}");
    }
    s.result_job = Some(job);
    s.result = Some(r);
}
