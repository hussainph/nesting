use super::*;
use kurbo::{BezPath, Circle, Point, Rect, Shape};
use nest_core::geom::{flatten_ring, point_in_ring, polygon_path};
use nest_core::{NestSettings, Part, PartGeometry, Rotation, Stock, StockKind};

pub(crate) mod fixtures {
    use super::*;

    pub fn part(id: u64, name: &str, outer: BezPath, holes: Vec<BezPath>, qty: u32) -> Part {
        let g = PartGeometry {
            outer,
            holes,
            marks: vec![],
        }
        .normalized();
        Part {
            id,
            name: name.into(),
            source: None,
            geometry: g,
            quantity: qty,
            rotation: None,
            allow_mirror: false,
        }
    }

    pub fn rect(w: f64, h: f64) -> BezPath {
        Rect::new(0.0, 0.0, w, h).to_path(0.01)
    }

    pub fn l_shape(w: f64, h: f64, t: f64) -> BezPath {
        polygon_path(&[(0.0, 0.0), (w, 0.0), (w, t), (t, t), (t, h), (0.0, h)].map(Point::from))
    }

    pub fn star(n: usize, ro: f64, ri: f64) -> BezPath {
        let pts: Vec<Point> = (0..2 * n)
            .map(|i| {
                let a = i as f64 * std::f64::consts::PI / n as f64;
                let r = if i % 2 == 0 { ro } else { ri };
                Point::new(r * a.cos(), r * a.sin())
            })
            .collect();
        polygon_path(&pts)
    }

    pub fn circle(r: f64) -> BezPath {
        Circle::new((0.0, 0.0), r).to_path(0.01)
    }

    /// A curvy closed shape standing in for a letter body: a bean made of cubics.
    pub fn bean(s: f64) -> BezPath {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.curve_to((s * 0.6, -s * 0.4), (s * 1.6, -s * 0.2), (s * 2.0, s * 0.3));
        p.curve_to((s * 2.3, s * 0.8), (s * 1.6, s * 1.0), (s * 1.0, s * 0.6));
        p.curve_to((s * 0.6, s * 0.4), (s * 0.3, s * 0.9), (-s * 0.1, s * 0.6));
        p.curve_to((-s * 0.4, s * 0.4), (-s * 0.3, s * 0.15), (0.0, 0.0));
        p.close_path();
        p
    }

    pub fn ring(ro: f64, ri: f64) -> (BezPath, Vec<BezPath>) {
        let mut hole = Circle::new((0.0, 0.0), ri).to_path(0.01);
        hole = kurbo::Affine::IDENTITY * hole;
        (circle(ro), vec![hole])
    }

    pub fn remnant_with_defect(id: u64) -> Stock {
        // An L-shaped offcut with a notch and a burn mark.
        let outline = polygon_path(
            &[
                (0.0, 0.0),
                (500.0, 0.0),
                (500.0, 180.0),
                (320.0, 180.0),
                (300.0, 220.0),
                (320.0, 260.0),
                (320.0, 400.0),
                (0.0, 400.0),
            ]
            .map(Point::from),
        );
        let defect = Circle::new((150.0, 200.0), 25.0).to_path(0.01);
        Stock {
            id,
            name: "Offcut".into(),
            kind: StockKind::Remnant,
            outline,
            defects: vec![defect],
            quantity: 1,
            material: "Acrylic".into(),
            thickness: 3.0,
        }
    }

    pub fn mixed_parts() -> Vec<Part> {
        let (ro, rh) = ring(40.0, 28.0);
        vec![
            part(1, "Rect", rect(80.0, 40.0), vec![], 6),
            part(2, "L", l_shape(90.0, 70.0, 20.0), vec![], 5),
            part(3, "Star", star(5, 35.0, 15.0), vec![], 5),
            part(4, "Ring", ro, rh, 3),
            part(5, "Dot", circle(4.0), vec![], 30),
            part(6, "Bean", bean(30.0), vec![], 8),
        ]
    }

    pub fn settings(secs: f64) -> NestSettings {
        NestSettings {
            time_limit_secs: secs,
            seed: Some(42),
            ..NestSettings::default()
        }
    }
}

use fixtures::*;

fn run(job: &NestJob) -> NestResult {
    let ctl = Control::default();
    let mut n = 0;
    let res = nest(job, &ctl, |_| n += 1).unwrap();
    assert!(n > 0, "progress was never reported");
    res
}

fn assert_valid(job: &NestJob, res: &NestResult) {
    let errs = validate::check(job, res);
    assert!(
        errs.is_empty(),
        "{} problems:\n{}",
        errs.len(),
        errs.join("\n")
    );
}

#[test]
fn placement_maps_real_outline_into_stock() {
    let mut p = part(1, "Star", star(5, 40.0, 15.0), vec![], 3);
    p.allow_mirror = true;
    let job = NestJob {
        parts: vec![p.clone()],
        stock: vec![Stock::rectangle(1, "Sheet", 300.0, 200.0)],
        settings: settings(1.0),
    };
    let res = run(&job);
    assert_eq!(res.placed_count(), 3);
    let stock_ring = flatten_ring(&job.stock[0].outline, 0.01);
    for pl in &res.sheets[0].placements {
        let ring = flatten_ring(&(pl.affine() * p.geometry.outer.clone()), 0.01);
        for pt in ring {
            assert!(point_in_ring(pt, &stock_ring), "{pt:?} outside");
            assert!(
                pt.x >= job.settings.edge_margin - 0.06 && pt.y >= job.settings.edge_margin - 0.06,
                "{pt:?} within margin"
            );
        }
    }
    assert_valid(&job, &res);
}

#[test]
fn rotation_rules_hold() {
    let mut a = part(1, "Fixed", l_shape(60.0, 40.0, 10.0), vec![], 4);
    a.rotation = Some(Rotation::Fixed);
    let mut b = part(2, "Step", l_shape(50.0, 50.0, 12.0), vec![], 4);
    b.rotation = Some(Rotation::Step(90.0));
    let job = NestJob {
        parts: vec![a, b],
        stock: vec![Stock::rectangle(1, "Sheet", 300.0, 200.0)],
        settings: settings(1.0),
    };
    let res = run(&job);
    assert_valid(&job, &res);
    for pl in res.sheets.iter().flat_map(|s| &s.placements) {
        let r = pl.rotation_deg.rem_euclid(360.0);
        if pl.part_id == 1 {
            assert!(r < 0.01 || r > 359.99, "fixed part rotated {r}");
        } else {
            let k = r / 90.0;
            assert!((k - k.round()).abs() < 1e-3, "step part rotated {r}");
        }
        assert!(!pl.mirrored);
    }
}

#[test]
fn irregular_remnant_with_defect() {
    let job = NestJob {
        parts: mixed_parts(),
        stock: vec![remnant_with_defect(1)],
        settings: settings(3.0),
    };
    let res = run(&job);
    assert!(res.placed_count() > 20, "placed {}", res.placed_count());
    assert_valid(&job, &res);
}

#[test]
fn remnant_used_before_sheet() {
    let parts = vec![part(1, "Rect", rect(60.0, 40.0), vec![], 10)];
    let job = NestJob {
        parts,
        stock: vec![
            Stock::rectangle(1, "Sheet", 1220.0, 610.0),
            remnant_with_defect(2),
        ],
        settings: settings(2.0),
    };
    let res = run(&job);
    assert_valid(&job, &res);
    assert_eq!(res.placed_count(), 10);
    assert_eq!(res.sheets.len(), 1);
    assert_eq!(
        res.sheets[0].stock_id, 2,
        "should fill the remnant, not the sheet"
    );
}

#[test]
fn overflow_goes_to_sheet_and_reports_missing() {
    let parts = vec![part(1, "Big", rect(200.0, 150.0), vec![], 20)];
    let job = NestJob {
        parts,
        stock: vec![
            remnant_with_defect(2),
            Stock::rectangle(1, "Sheet", 640.0, 480.0),
        ],
        settings: settings(2.0),
    };
    let res = run(&job);
    assert_valid(&job, &res);
    assert!(res.unplaced_count() > 0);
    assert_eq!(res.placed_count() as u32 + res.unplaced_count(), 20);
}

#[test]
fn part_in_part_fills_holes() {
    let (ro, rh) = ring(60.0, 45.0);
    let parts = vec![
        part(1, "Ring", ro, rh, 2),
        part(2, "Dot", circle(8.0), vec![], 10),
    ];
    let mut s = settings(2.0);
    s.part_in_part = true;
    let job = NestJob {
        parts,
        stock: vec![Stock::rectangle(1, "Sheet", 300.0, 160.0)],
        settings: s,
    };
    let res = run(&job);
    assert_valid(&job, &res);
    assert_eq!(res.placed_count(), 12);
    // Dots inside ring holes: their centres lie within 45 mm of a ring centre.
    let pls = &res.sheets[0].placements;
    let ring_centres: Vec<Point> = pls
        .iter()
        .filter(|p| p.part_id == 1)
        .map(|p| p.affine() * Point::new(60.0, 60.0))
        .collect();
    let inside = pls
        .iter()
        .filter(|p| p.part_id == 2)
        .filter(|p| {
            let c = p.affine() * Point::new(8.0, 8.0);
            ring_centres.iter().any(|rc| (c - *rc).hypot() < 45.0)
        })
        .count();
    assert!(inside >= 4, "only {inside} dots inside holes");
}

#[test]
fn cancel_returns_quickly() {
    let job = NestJob {
        parts: mixed_parts(),
        stock: vec![Stock::rectangle(1, "Sheet", 600.0, 400.0)],
        settings: settings(30.0),
    };
    let ctl = Control::default();
    let t = Instant::now();
    let res = nest(&job, &ctl, |_| ctl.cancel.store(true, AtOrd::Relaxed)).unwrap();
    assert!(t.elapsed().as_secs_f64() < 5.0, "took {:?}", t.elapsed());
    assert!(res.placed_count() > 0);
    assert_valid(&job, &res);
}

#[test]
fn empty_job_is_ok() {
    let job = NestJob {
        parts: vec![],
        stock: vec![Stock::rectangle(1, "Sheet", 100.0, 100.0)],
        settings: settings(1.0),
    };
    let res = run(&job);
    assert_eq!(res.placed_count(), 0);
}

/// Runs jagua-rs's reference LBF on the same prepared instance and converts it to our result type.
fn run_lbf(job: &NestJob) -> NestResult {
    use jagua_rs::entities::Layout;
    use jagua_rs::probs::bpp::entities::{BPInstance, Bin};
    let prep = prep::prepare(job).unwrap();
    let items = prep
        .items
        .iter()
        .map(|i| (i.item.clone(), i.demand))
        .collect();
    let bins = prep
        .bins
        .iter()
        .enumerate()
        .map(|(i, b)| {
            Bin::new(
                i,
                i as u64,
                b.container.clone(),
                b.qty,
                b.cost.round() as u64,
            )
        })
        .collect();
    let inst = BPInstance::new(items, bins);
    let cfg = lbf::config::LBFConfig {
        prng_seed: Some(42),
        ..Default::default()
    };
    let mut opt = lbf::opt::lbf_bpp::LBFOptimizerBP::new(inst, cfg, SmallRng::seed_from_u64(42));
    let sol = opt.solve();
    let mut ours = Sol::empty(&prep);
    for (k, ls) in sol.layout_snapshots() {
        let bin = sol.layout_bins()[k];
        let layout = Layout::from_snapshot(ls);
        for pi in layout.placed_items().values() {
            ours.missing[pi.item().idx()] -= 1;
        }
        ours.used[bin] += 1;
        ours.sheets.push(solve::Sheet { bin, layout });
    }
    to_result(job, &prep, &ours)
}

/// (placed share of part area, utilization of used stock, used length of the last sheet as a share of its width)
fn metrics(job: &NestJob, res: &NestResult) -> (f64, f64, f64) {
    let total: f64 = job
        .parts
        .iter()
        .map(|p| p.quantity as f64 * p.geometry.area())
        .sum();
    let placed: f64 = res
        .sheets
        .iter()
        .flat_map(|s| &s.placements)
        .map(|pl| {
            job.parts
                .iter()
                .find(|p| p.id == pl.part_id)
                .unwrap()
                .geometry
                .area()
        })
        .sum();
    let used: f64 = res
        .sheets
        .iter()
        .map(|sh| {
            job.stock
                .iter()
                .find(|s| s.id == sh.stock_id)
                .unwrap()
                .area()
        })
        .sum();
    let last = res.sheets.last().map_or(0.0, |sh| {
        let st = job.stock.iter().find(|s| s.id == sh.stock_id).unwrap();
        let x = sh
            .placements
            .iter()
            .map(|pl| {
                let p = job.parts.iter().find(|p| p.id == pl.part_id).unwrap();
                (pl.affine() * p.geometry.outer.clone()).bounding_box().x1
            })
            .fold(0.0, f64::max);
        x / st.bbox().width()
    });
    (placed / total, placed / used.max(1.0), last)
}

fn scaled(mut parts: Vec<Part>, k: u32) -> Vec<Part> {
    for p in &mut parts {
        p.quantity *= k;
    }
    parts
}

/// `cargo test -p nest-engine --release -- --ignored --nocapture bench`
#[test]
#[ignore]
fn bench() {
    let secs = 15.0;
    let letters = || {
        vec![
            part(1, "Dot", circle(4.0), vec![], 120),
            part(2, "Bean", bean(25.0), vec![], 40),
            part(3, "Star", star(6, 20.0, 9.0), vec![], 25),
        ]
    };
    let cases: Vec<(&str, NestJob)> = vec![
        (
            "overfull remnant w/ defect (mixed x3)",
            NestJob {
                parts: scaled(mixed_parts(), 3),
                stock: vec![remnant_with_defect(1)],
                settings: settings(secs),
            },
        ),
        (
            "overfull 600x400 sheet (letters x2)",
            NestJob {
                parts: scaled(letters(), 2),
                stock: vec![Stock::rectangle(1, "Sheet", 600.0, 400.0)],
                settings: settings(secs),
            },
        ),
        (
            "length used, 1200x400 sheet (mixed)",
            NestJob {
                parts: mixed_parts(),
                stock: vec![Stock::rectangle(1, "Sheet", 1200.0, 400.0)],
                settings: settings(secs),
            },
        ),
        ("remnant + 600x400 sheets (mixed x3)", {
            let mut sheet = Stock::rectangle(2, "Sheet", 600.0, 400.0);
            sheet.quantity = 6;
            NestJob {
                parts: scaled(mixed_parts(), 3),
                stock: vec![remnant_with_defect(1), sheet],
                settings: settings(secs),
            }
        }),
    ];
    for (name, job) in cases {
        let t = Instant::now();
        let l = run_lbf(&job);
        let lt = t.elapsed().as_secs_f64();
        assert_valid(&job, &l);
        let o = run(&job);
        assert_valid(&job, &o);
        let (lp, lu, lx) = metrics(&job, &l);
        let (op, ou, ox) = metrics(&job, &o);
        println!(
            "{name}\n  lbf : placed {:5.1}% of part area, utilization {:5.1}%, last sheet length used {:5.1}%, {} sheets, {lt:.1}s\n  ours: placed {:5.1}% of part area, utilization {:5.1}%, last sheet length used {:5.1}%, {} sheets, {:.1}s",
            lp * 100.0,
            lu * 100.0,
            lx * 100.0,
            l.sheets.len(),
            op * 100.0,
            ou * 100.0,
            ox * 100.0,
            o.sheets.len(),
            o.elapsed_secs
        );
    }
}

#[test]
fn validator_catches_bad_results() {
    let job = NestJob {
        parts: vec![part(1, "Rect", rect(50.0, 30.0), vec![], 2)],
        stock: vec![remnant_with_defect(1)],
        settings: settings(1.0),
    };
    let mut res = run(&job);
    assert_valid(&job, &res);
    let good = res.clone();
    // Stack both copies on top of each other.
    res.sheets[0].placements[1] = res.sheets[0].placements[0];
    assert!(!validate::check(&job, &res).is_empty());
    // Too close to the edge.
    let mut r2 = good.clone();
    r2.sheets[0].placements[0].translation = (1.0, 1.0);
    r2.sheets[0].placements[0].rotation_deg = 0.0;
    assert!(
        validate::check(&job, &r2)
            .iter()
            .any(|e| e.contains("edge")),
        "{:?}",
        validate::check(&job, &r2)
    );
    // On the burn mark.
    let mut r3 = good.clone();
    r3.sheets[0].placements[0] = Placement {
        part_id: 1,
        mirrored: false,
        rotation_deg: 0.0,
        translation: (130.0, 190.0),
    };
    assert!(
        validate::check(&job, &r3)
            .iter()
            .any(|e| e.contains("defect")),
        "{:?}",
        validate::check(&job, &r3)
    );
    // Outside the notch.
    let mut r4 = good;
    r4.sheets[0].placements[0] = Placement {
        part_id: 1,
        mirrored: false,
        rotation_deg: 0.0,
        translation: (400.0, 300.0),
    };
    assert!(
        validate::check(&job, &r4)
            .iter()
            .any(|e| e.contains("outside"))
    );
}

/// Writes an SVG of a result for eyeballing.
pub(crate) fn dump_svg(job: &NestJob, res: &NestResult, path: &str) {
    use std::fmt::Write;
    let mut out = String::new();
    let mut y_off = 0.0;
    let mut body = String::new();
    let mut w_max: f64 = 0.0;
    for sh in &res.sheets {
        let st = job.stock.iter().find(|s| s.id == sh.stock_id).unwrap();
        let b = st.bbox();
        w_max = w_max.max(b.width());
        let flip = kurbo::Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, y_off + b.height()]);
        let _ = write!(
            body,
            "<path d='{}' fill='#eee' stroke='#333'/>",
            (flip * st.outline.clone()).to_svg()
        );
        for d in &st.defects {
            let _ = write!(
                body,
                "<path d='{}' fill='#f99'/>",
                (flip * d.clone()).to_svg()
            );
        }
        for pl in &sh.placements {
            let p = job.parts.iter().find(|p| p.id == pl.part_id).unwrap();
            let g = p.geometry.transformed(flip * pl.affine());
            let mut d = g.outer.to_svg();
            for h in &g.holes {
                d.push(' ');
                d.push_str(&h.to_svg());
            }
            let _ = write!(
                body,
                "<path d='{d}' fill='#7aa7d8' fill-rule='evenodd' stroke='#1d4f86' stroke-width='0.5'/>"
            );
        }
        y_off += b.height() + 20.0;
    }
    let _ = write!(
        out,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='-10 -10 {} {}' width='{}' height='{}'><rect x='-10' y='-10' width='100%' height='100%' fill='white'/>{body}</svg>",
        w_max + 20.0,
        y_off + 20.0,
        (w_max + 20.0) * 1.5,
        (y_off + 20.0) * 1.5
    );
    std::fs::write(path, out).unwrap();
}

#[test]
#[ignore]
fn dump_examples() {
    let dir = env!("CARGO_MANIFEST_DIR").to_string() + "/../../target-engine/";
    let cases: Vec<(&str, NestJob)> = vec![
        (
            "strip",
            NestJob {
                parts: mixed_parts(),
                stock: vec![Stock::rectangle(1, "Sheet", 1200.0, 400.0)],
                settings: settings(15.0),
            },
        ),
        (
            "remnant",
            NestJob {
                parts: scaled(mixed_parts(), 3),
                stock: vec![remnant_with_defect(1)],
                settings: settings(15.0),
            },
        ),
    ];
    for (name, job) in cases {
        let res = run(&job);
        assert_valid(&job, &res);
        dump_svg(&job, &res, &format!("{dir}{name}.svg"));
        let l = run_lbf(&job);
        dump_svg(&job, &l, &format!("{dir}{name}-lbf.svg"));
    }
}
