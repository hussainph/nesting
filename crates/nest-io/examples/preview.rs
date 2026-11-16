//! Lays out every part of a file in a row and writes an SVG preview, one colour per part.
//! `cargo run -p nest-io --example preview -- <file> <out.svg>`
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let r = nest_io::import_path(std::path::Path::new(&args[1]))?;
    let mut job = nest_core::NestJob::default();
    let mut sheet = nest_core::SheetLayout::default();
    let mut x = 0.0;
    for (i, p) in r.parts.iter().enumerate() {
        let b = p.geometry.bbox();
        println!(
            "{}: {:.1} x {:.1} mm, {} holes, {} marks, area {:.1}",
            p.name,
            b.width(),
            b.height(),
            p.geometry.holes.len(),
            p.geometry.marks.len(),
            p.geometry.area()
        );
        job.parts.push(nest_core::Part {
            id: i as u64,
            name: p.name.clone(),
            source: None,
            geometry: p.geometry.clone(),
            quantity: 1,
            rotation: None,
            allow_mirror: false,
        });
        sheet.placements.push(nest_core::Placement {
            part_id: i as u64,
            translation: (x, 0.0),
            ..Default::default()
        });
        x += b.width() + 5.0;
    }
    for w in &r.warnings {
        println!("warning: {w}");
    }
    std::fs::write(&args[2], nest_io::export_svg(&job, &sheet))?;
    Ok(())
}
