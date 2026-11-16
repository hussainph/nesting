//! The board: the sheets of the current job or result, drawn on a pannable, zoomable canvas.

use kurbo::{Affine, Point};
use rune::{Color, Cx, El, LineCap, Shape, canvas, col, el, text};
use rune_kit::{Button, ButtonVariant, Kit};

use crate::icons;
use crate::state::{App, Camera, Flat, fmt_mm};
use nest_core::StockKind;

pub struct Colors {
    pub board: Color,
    pub sheet: Color,
    pub sheet_edge: Color,
    pub defect: Color,
    pub part: Color,
    pub part_edge: Color,
    pub part_selected: Color,
    pub part_selected_edge: Color,
    pub mark: Color,
}

pub fn colors(k: &Kit) -> Colors {
    let accent = k.c.border_focus;
    Colors {
        board: if k.dark { rune::oklch(0.17, 0.0, 0.0) } else { rune::oklch(0.935, 0.0, 0.0) },
        sheet: if k.dark { rune::oklch(0.25, 0.0, 0.0) } else { rune::oklch(1.0, 0.0, 0.0) },
        sheet_edge: if k.dark { rune::oklch(0.42, 0.0, 0.0) } else { rune::oklch(0.72, 0.0, 0.0) },
        defect: k.c.danger.fg.alpha(0.18),
        part: k.alpha(k.c.text_base, if k.dark { 0.16 } else { 0.09 }),
        part_edge: k.c.text_base,
        part_selected: accent.alpha(0.22),
        part_selected_edge: accent,
        mark: k.c.danger.fg,
    }
}

fn ring(cam: &Camera, t: Affine, pts: &[Point]) -> Vec<[f32; 2]> {
    pts.iter().map(|p| cam.to_screen(t * *p)).collect()
}

/// Paints one item: fill, holes punched in `under`, outline, marks.
fn paint(mut shape: Shape, cam: &Camera, t: Affine, flat: &Flat, fill: Color, under: Color, edge: Color, mark: Color, width: f32) -> Shape {
    let outer = ring(cam, t, &flat.outer);
    if outer.len() < 3 {
        return shape;
    }
    shape = shape.fill(outer.clone(), fill);
    let holes: Vec<Vec<[f32; 2]>> = flat.holes.iter().map(|h| ring(cam, t, h)).filter(|h| h.len() > 2).collect();
    for h in &holes {
        shape = shape.fill(h.clone(), under);
    }
    shape = shape.stroke_with(outer, true, width, LineCap::Round, edge);
    for h in holes {
        shape = shape.stroke_with(h, true, width, LineCap::Round, edge);
    }
    for (pts, closed) in &flat.marks {
        let m = ring(cam, t, pts);
        if m.len() > 1 {
            shape = shape.stroke_with(m, *closed, width, LineCap::Round, mark);
        }
    }
    shape
}

pub fn view(s: &App, k: &Kit, _cx: &mut Cx<App>) -> El {
    let c = colors(k);
    let cam = s.cam;
    let [cw, ch] = s.canvas;
    let board = s.board();
    let mut shape = Shape::new();
    let mut labels = Vec::new();
    let stock_src = |id| s.result_job.as_ref().and_then(|j| j.stock.iter().find(|x| x.id == id)).or(s.stock(id));

    for (i, sheet) in board.iter().enumerate() {
        let Some(stock) = stock_src(sheet.stock_id) else { continue };
        let flat = s.stock_flats.get(&stock.id).cloned().unwrap_or_else(|| Flat::of_stock(stock));
        let t = Affine::translate(sheet.offset.to_vec2());
        let b = stock.bbox();
        let tl = cam.to_screen(t * Point::new(b.x0, b.y1));
        let br = cam.to_screen(t * Point::new(b.x1, b.y0));
        if br[0] < 0.0 || tl[0] > cw || br[1] < 0.0 || tl[1] > ch + 40.0 {
            continue;
        }
        shape = paint(shape, &cam, t, &flat, c.sheet, c.defect, c.sheet_edge, c.mark, 1.0);
        for p in &sheet.placements {
            let Some(pf) = s.flats.get(&p.part_id) else { continue };
            let sel = s.selected == Some(p.part_id);
            let (fill, edge) = if sel { (c.part_selected, c.part_selected_edge) } else { (c.part, c.part_edge) };
            shape = paint(shape, &cam, t * p.affine(), pf, fill, c.sheet, edge, c.mark, if sel { 1.5 } else { 1.0 });
        }

        let kind = if stock.kind == StockKind::Remnant { "Remnant" } else { "Sheet" };
        let mut label = if s.result.is_some() { format!("{} {}", kind, i + 1) } else { stock.name.clone() };
        label.push_str(&format!("  ·  {} × {} mm", fmt_mm(b.width()), fmt_mm(b.height())));
        if let Some(u) = sheet.utilization {
            label.push_str(&format!("  ·  {:.1}% used", u * 100.0));
        }
        labels.push(
            text(label)
                .font(k.ty.small.features("'tnum' 1"))
                .color(k.c.text_muted)
                .single_line()
                .absolute(tl[0], tl[1] - 20.0),
        );
    }

    let board_el = canvas(shape)
        .key("board")
        .w_full()
        .h_full()
        .fill(c.board)
        .on_layout(|s: &mut App, r: [f32; 4]| {
            let first = s.canvas[0] <= 0.0;
            s.canvas = [r[2], r[3]];
            if first {
                s.fit_pending = true;
            }
        })
        .on_drag(|s: &mut App, d, _tx| {
            if let Some(last) = s.drag_last {
                s.cam.pan[0] += d.x - last[0];
                s.cam.pan[1] += d.y - last[1];
            }
            s.drag_last = if matches!(d.phase, rune::Phase::Up) { None } else { Some([d.x, d.y]) };
        })
        .on_wheel(|s: &mut App, e, _tx| {
            if e.precise && !(e.mods.cmd || e.mods.ctrl) {
                s.cam.pan[0] += e.dx;
                s.cam.pan[1] += e.dy;
            } else {
                s.cam.zoom_about(e.x, e.y, (1.0 + e.dy * 0.01).clamp(0.5, 2.0));
            }
            true
        })
        .on_magnify(|s: &mut App, e, _tx| {
            s.cam.zoom_about(e.x, e.y, 1.0 + e.delta);
            true
        })
        .children(labels);

    let mut layers = vec![board_el];
    if let Some(empty) = empty_state(s, k) {
        layers.push(empty);
    }
    el().key("board-wrap").w_full().grow(1.0).min_h(0.0).clip_rounded(0.0).children(layers)
}

fn empty_state(s: &App, k: &Kit) -> Option<El> {
    let (title, line, action): (&str, &str, Option<El>) = if s.parts.is_empty() {
        let btn = Button::new("Add files")
            .icon(&icons::IMPORT)
            .variant(ButtonVariant::Neutral)
            .on_press_tx(|s: &mut App, tx| crate::view::pick_files(s, tx))
            .build(k);
        if s.importing > 0 {
            ("Importing…", "", None)
        } else {
            ("Add parts to start", "Drop AI, DXF or SVG files here.", Some(btn))
        }
    } else if s.use_stock.is_empty() {
        let btn = Button::new("Add stock").variant(ButtonVariant::Neutral).on_press(|s: &mut App| crate::view::new_stock(s)).build(k);
        ("No stock selected", "Add a sheet or remnant, or switch one on in the list.", Some(btn))
    } else {
        return None;
    };
    let mut body = col()
        .items_center()
        .gap(6.0)
        .child(text(title).font(k.ty.body_medium).color(k.c.text_strong))
        .child(text(line).font(k.ty.ui).color(k.c.text_muted));
    if let Some(a) = action {
        body = body.child(el().mt(10.0).child(a));
    }
    Some(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().child(body))
}
