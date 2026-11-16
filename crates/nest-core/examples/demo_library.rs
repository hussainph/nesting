//! Writes a demo stock library: a full sheet and an L-shaped remnant with a damaged spot.
//! cargo run -p nest-core --example demo_library -- out/demo-library.json

use kurbo::{Circle, Point, Shape};
use nest_core::library::Library;
use nest_core::{Stock, StockKind};

fn main() {
    let path = std::env::args().nth(1).expect("give an output path");
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
    Library { stock: vec![remnant, sheet] }.save(std::path::Path::new(&path)).unwrap();
}
