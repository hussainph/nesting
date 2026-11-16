//! Command-line nesting, for scripts, benchmarks and the shop server.
//!
//! nest-cli parts/*.dxf --sheet 1200x900 --remnant offcut.dxf --qty 3 --time 20 --out out/ --format svg

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use nest_core::{NestJob, NestSettings, Part, Rotation, Stock, StockKind};

#[derive(Parser)]
#[command(name = "nest-cli", about = "Nest parts from AI, PDF, DXF or SVG files onto sheets and remnants.")]
struct Args {
    /// Part files (.ai, .pdf, .dxf, .svg). Every closed outline becomes a part.
    #[arg(required = true)]
    parts: Vec<PathBuf>,
    /// Copies of each part.
    #[arg(long, default_value_t = 1)]
    qty: u32,
    /// Full sheet as WIDTHxHEIGHT in mm. Repeat for more sizes; append :N for N sheets (1200x900:3).
    #[arg(long)]
    sheet: Vec<String>,
    /// Remnant outline file (.dxf or .svg). Repeat for more.
    #[arg(long)]
    remnant: Vec<PathBuf>,
    /// Use stock from a library JSON file instead of --sheet/--remnant.
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long, default_value_t = 2.0)]
    spacing: f64,
    #[arg(long, default_value_t = 5.0)]
    margin: f64,
    /// any, 90, 180 or fixed.
    #[arg(long, default_value = "any")]
    rotation: String,
    /// Search time in seconds.
    #[arg(long, default_value_t = 20.0)]
    time: f64,
    #[arg(long)]
    seed: Option<u64>,
    /// Folder to write one file per sheet into.
    #[arg(long)]
    out: Option<PathBuf>,
    /// dxf or svg.
    #[arg(long, default_value = "dxf")]
    format: String,
    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_sheet(spec: &str, id: u64) -> Result<Stock> {
    let (size, count) = spec.split_once(':').map_or((spec, "1"), |(a, b)| (a, b));
    let (w, h) = size.split_once(['x', 'X', '×']).with_context(|| format!("Sheet size must look like 1200x900, got {spec}"))?;
    let w: f64 = w.trim().parse().with_context(|| format!("Bad sheet width in {spec}"))?;
    let h: f64 = h.trim().parse().with_context(|| format!("Bad sheet height in {spec}"))?;
    let mut s = Stock::rectangle(id, format!("Sheet {w} × {h}"), w, h);
    s.quantity = count.trim().parse().with_context(|| format!("Bad sheet count in {spec}"))?;
    Ok(s)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut parts = Vec::new();
    for path in &args.parts {
        let report = nest_io::import_path(path).with_context(|| format!("Couldn't import {}", path.display()))?;
        for w in &report.warnings {
            eprintln!("{}: {w}", path.display());
        }
        for imp in report.parts {
            parts.push(Part {
                id: parts.len() as u64 + 1,
                name: imp.name,
                source: Some(path.display().to_string()),
                geometry: imp.geometry,
                quantity: args.qty,
                rotation: None,
                allow_mirror: false,
            });
        }
    }
    if parts.is_empty() {
        bail!("No closed outlines found in the part files.");
    }

    let mut stock = Vec::new();
    if let Some(lib) = &args.library {
        stock = nest_core::library::Library::load(lib)?.stock;
    }
    for spec in &args.sheet {
        let id = stock.len() as u64 + 1;
        stock.push(parse_sheet(spec, id)?);
    }
    for path in &args.remnant {
        let outline = nest_io::import_outline(path).with_context(|| format!("Couldn't read remnant {}", path.display()))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Remnant".into());
        stock.push(Stock {
            id: stock.len() as u64 + 1,
            name,
            kind: StockKind::Remnant,
            outline,
            defects: vec![],
            quantity: 1,
            material: String::new(),
            thickness: 0.0,
        });
    }
    if stock.is_empty() {
        bail!("Give at least one --sheet, --remnant or --library.");
    }

    let rotation = match args.rotation.as_str() {
        "any" => Rotation::Any,
        "fixed" | "none" => Rotation::Fixed,
        d => Rotation::Step(d.parse().context("--rotation must be any, fixed or a step in degrees")?),
    };
    let settings = NestSettings {
        spacing: args.spacing,
        edge_margin: args.margin,
        default_rotation: rotation,
        time_limit_secs: args.time,
        seed: args.seed,
        ..NestSettings::default()
    };
    let job = NestJob { parts, stock, settings };

    let ctl = nest_engine::Control::default();
    let total: u32 = job.parts.iter().map(|p| p.quantity).sum();
    let result = nest_engine::nest(&job, &ctl, |r| {
        eprintln!("  {:.1}% used, {} of {} placed, {} sheets", r.utilization(&job) * 100.0, r.placed_count(), total, r.sheets.len());
    })?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{} sheets, {:.1}% used, {} of {} placed, {:.1} s",
            result.sheets.len(),
            result.utilization(&job) * 100.0,
            result.placed_count(),
            total,
            result.elapsed_secs
        );
        for (i, s) in result.sheets.iter().enumerate() {
            println!("  sheet {}: stock {} with {} parts, {:.1}% used", i + 1, s.stock_id, s.placements.len(), s.utilization * 100.0);
        }
    }

    if let Some(dir) = &args.out {
        std::fs::create_dir_all(dir)?;
        for (i, sheet) in result.sheets.iter().enumerate() {
            let (body, ext) = match args.format.as_str() {
                "svg" => (nest_io::export_svg(&job, sheet), "svg"),
                _ => (nest_io::export_dxf(&job, sheet)?, "dxf"),
            };
            let path = dir.join(format!("sheet-{}.{ext}", i + 1));
            std::fs::write(&path, body)?;
            eprintln!("wrote {}", path.display());
        }
    }
    Ok(())
}
