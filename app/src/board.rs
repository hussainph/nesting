//! The board: the sheets of the current job or result, drawn on a pannable, zoomable canvas.

use kurbo::{Affine, Point};
use rune::{Color, Cx, El, LineCap, PointerButton, Shape, TextEdit, canvas, col, el, row, text};
use rune_kit::{Button, ButtonVariant, IconButton, IconButtonVariant, Kit, TextInput};

use crate::icons;
use crate::layout::{BoardSel, Field, Frame, GRIP, Grip};
use crate::state::{App, Camera, Cut, Flat, fmt_mm};
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
    pub conflict: Color,
    pub conflict_edge: Color,
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
        conflict: k.c.danger.fg.alpha(0.25),
        conflict_edge: k.c.danger.fg,
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
        let b = stock.bbox();
        let mut t = Affine::translate(sheet.offset.to_vec2());
        if let Some((si, deg)) = s.spin {
            if si == i {
                t = t * Affine::rotate_about(deg.to_radians(), b.center());
            }
        }
        let tl = cam.to_screen(t * Point::new(b.x0, b.y1));
        let br = cam.to_screen(t * Point::new(b.x1, b.y0));
        if br[0] < 0.0 || tl[0] > cw || br[1] < 0.0 || tl[1] > ch + 40.0 {
            continue;
        }
        shape = paint(shape, &cam, t, &flat, c.sheet, c.defect, c.sheet_edge, c.mark, 1.0);
        for (pi, p) in sheet.placements.iter().enumerate() {
            let Some(pf) = s.flats.get(&p.part_id) else { continue };
            let picked = s.is_selected((i, pi)) || (s.sel == BoardSel::None && s.selected == Some(p.part_id));
            let bad = s.has_conflict((i, pi));
            let (fill, edge) = match (picked, bad) {
                (_, true) => (c.conflict, c.conflict_edge),
                (true, false) => (c.part_selected, c.part_selected_edge),
                _ => (c.part, c.part_edge),
            };
            shape = paint(shape, &cam, t * p.affine(), pf, fill, c.sheet, edge, c.mark, if picked { 1.5 } else { 1.0 });
        }

        let kind = if stock.kind == StockKind::Remnant { "Remnant" } else { "Sheet" };
        let mut label = if s.result.is_some() { format!("{} {}", kind, i + 1) } else { stock.name.clone() };
        label.push_str(&format!("  ·  {} × {} mm", fmt_mm(b.width()), fmt_mm(b.height())));
        if let Some(u) = sheet.utilization {
            label.push_str(&format!("  ·  {:.1}% used", u * 100.0));
        }
        let mut head = row().items_center().gap(8.0).h(24.0).absolute(tl[0], tl[1] - 28.0).child(
            text(label).font(k.ty.small.features("'tnum' 1")).color(k.c.text_muted).single_line(),
        );
        if s.result.is_some() && !s.is_running() && !s.saved_offcuts.contains(&i) && s.spin.is_none() {
            if let Some(o) = s.offcut(i) {
                let (a, b2) = match o.cut {
                    Cut::X(x) => (Point::new(x, b.y0), Point::new(x, b.y1)),
                    Cut::Y(y) => (Point::new(b.x0, y), Point::new(b.x1, y)),
                };
                shape = shape.line(cam.to_screen(t * a), cam.to_screen(t * b2), 1.5, LineCap::Butt, c.part_selected_edge);
                let ob = kurbo::Shape::bounding_box(&o.outline);
                head = head.child(
                    Button::new(format!("Save offcut {} × {}", fmt_mm(ob.width()), fmt_mm(ob.height())))
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(rune_kit::ControlSize::Regular)
                        .on_press(move |s: &mut App| s.save_offcut(i))
                        .build(k),
                );
            }
        }
        // A selected sheet's name and size are in the bar, and its rotate handle sits here.
        if s.sel != BoardSel::Sheet(i) {
            labels.push(head);
        }
    }

    shape = selection(shape, s, k);

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
        .pointer_buttons([PointerButton::Middle])
        .cursor(s.board_cursor())
        .on_pointer_move_local(|s: &mut App, p, _tx| s.board_pointer(p.x, p.y))
        .on_hover_change(|s: &mut App, inside, _tx| {
            if !inside {
                s.hover = None;
                s.hover_part = false;
            }
        })
        .on_drag(|s: &mut App, d, tx| {
            let travel = (d.dx * d.dx + d.dy * d.dy).sqrt();
            s.board_drag(d.phase, d.x, d.y, travel, tx);
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
    if let Some(bar) = selection_bar(s, k) {
        layers.push(row().pin(Some(0.0), Some(12.0), Some(0.0), None).justify_center().child(bar));
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

fn circle(c: [f32; 2], r: f32) -> Vec<[f32; 2]> {
    (0..20)
        .map(|i| {
            let t = i as f32 / 20.0 * std::f32::consts::TAU;
            [c[0] + r * t.cos(), c[1] + r * t.sin()]
        })
        .collect()
}

/// The selection frame, its handles and the selection box.
fn selection(mut shape: Shape, s: &App, k: &Kit) -> Shape {
    let accent = k.c.border_focus;
    let handle_fill = colors(k).sheet;
    if let Some(crate::layout::Op::Marquee { from, to, moved: true, .. }) = &s.op {
        let r = kurbo::Rect::from_points(*from, *to);
        let pts = [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x1, r.y1), Point::new(r.x0, r.y1)].map(|p| s.cam.to_screen(p)).to_vec();
        shape = shape.fill(pts.clone(), k.alpha(accent, 0.08)).stroke_with(pts, true, 1.0, LineCap::Butt, accent);
    }
    if s.spin.is_some() {
        return shape;
    }
    let Some(f) = s.frame() else { return shape };
    let corners = s.frame_corners(&f).to_vec();
    let width = if matches!(f, Frame::Sheet(_)) { 1.5 } else { 1.0 };
    shape = shape.stroke_with(corners.clone(), true, width, LineCap::Butt, accent);
    let grips = s.grips();
    for (g, p) in &grips {
        let hot = s.hover == Some(*g);
        match g {
            Grip::Scale(..) => {
                let h = GRIP * 0.5 + if hot { 1.0 } else { 0.0 };
                let sq = vec![[p[0] - h, p[1] - h], [p[0] + h, p[1] - h], [p[0] + h, p[1] + h], [p[0] - h, p[1] + h]];
                shape = shape.fill(sq.clone(), if hot { accent } else { handle_fill }).stroke_with(sq, true, 1.0, LineCap::Butt, accent);
            }
            Grip::Rotate | Grip::SheetRotate => {
                let top = [(corners[2][0] + corners[3][0]) * 0.5, (corners[2][1] + corners[3][1]) * 0.5];
                shape = shape.line(top, *p, 1.0, LineCap::Butt, accent);
                let r = GRIP * 0.6 + if hot { 1.0 } else { 0.0 };
                shape = shape.fill(circle(*p, r), if hot { accent } else { handle_fill }).stroke_with(circle(*p, r), true, 1.0, LineCap::Round, accent);
            }
        }
    }
    shape
}

fn bar_input(k: &Kit, s: &App, f: Field, label: &str, width: f32) -> El {
    let edit: &TextEdit = match f {
        Field::X => &s.bar.x,
        Field::Y => &s.bar.y,
        Field::W => &s.bar.w,
        Field::H => &s.bar.h,
        Field::Angle => &s.bar.angle,
    };
    row()
        .items_center()
        .gap(5.0)
        .child(text(label.to_string()).font(k.ty.ui).color(k.c.text_muted))
        .child(
            TextInput::new(edit)
                .width(Some(width))
                .height(26.0)
                .text_style(k.ty.ui_compact.features("'tnum' 1"))
                .on_edit(move |s: &mut App| s.bar.field(f))
                .on_change(move |s: &mut App, _tx| s.bar.pending = Some(f))
                .on_submit(|s: &mut App, _tx| s.apply_bar())
                .build(k)
                .key(format!("bar-{f:?}")),
        )
}

fn divider(k: &Kit) -> El {
    el().w(1.0).h(20.0).fill(k.c.border_weak).mx(4.0)
}

fn turn_buttons(k: &Kit) -> El {
    row()
        .items_center()
        .gap(2.0)
        .child(IconButton::new(&icons::ROTATE_CCW).variant(IconButtonVariant::GhostMuted).label("Rotate left").help("Rotate left (Shift+R)").on_press(|s: &mut App| s.quarter_turn(true)).build(k))
        .child(IconButton::new(&icons::ROTATE_CW).variant(IconButtonVariant::GhostMuted).label("Rotate right").help("Rotate right (R)").on_press(|s: &mut App| s.quarter_turn(false)).build(k))
}

fn size_inputs(k: &Kit, s: &App, copies: u32) -> El {
    let lock = IconButton::new(if s.bar.lock { &icons::LINK } else { &icons::UNLINK })
        .variant(if s.bar.lock { IconButtonVariant::Neutral } else { IconButtonVariant::GhostMuted })
        .label("Keep proportions")
        .help("Keep proportions")
        .on_press(|s: &mut App| s.bar.lock = !s.bar.lock)
        .build(k);
    let mut r = row().items_center().gap(6.0).child(bar_input(k, s, Field::W, "W", 64.0)).child(lock).child(bar_input(k, s, Field::H, "H", 64.0));
    if copies > 1 {
        r = r.child(text(format!("All {copies} copies")).font(k.ty.ui).color(k.c.text_muted).single_line().ml(2.0));
    }
    r
}

/// The floating bar over the board for whatever is selected: position, size and angle fields,
/// quarter turns, and for a sheet its size and an edit button.
fn selection_bar(s: &App, k: &Kit) -> Option<El> {
    if s.editor.is_some() || s.spin.is_some() {
        return None;
    }
    let name = |id| s.part(id).map(|p| p.name.clone()).or_else(|| s.result_job.as_ref().and_then(|j| j.parts.iter().find(|p| p.id == id)).map(|p| p.name.clone()));
    let qty = |id| s.result_job.as_ref().and_then(|j| j.parts.iter().find(|p| p.id == id)).or(s.part(id)).map_or(1, |p| p.quantity);
    let title = |t: String| text(t).font(k.ty.ui_medium).color(k.c.text_strong).single_line().max_w(180.0).mr(4.0);
    let mut r = row().items_center().gap(8.0);
    match &s.sel {
        BoardSel::Parts(v) if v.len() == 1 => {
            let pl = s.result.as_ref()?.sheets.get(v[0].0)?.placements.get(v[0].1)?;
            r = r
                .child(title(name(pl.part_id).unwrap_or_default()))
                .child(bar_input(k, s, Field::X, "X", 64.0))
                .child(bar_input(k, s, Field::Y, "Y", 64.0))
                .child(divider(k))
                .child(size_inputs(k, s, qty(pl.part_id)))
                .child(divider(k))
                .child(row().items_center().gap(3.0).child(bar_input(k, s, Field::Angle, "Angle", 52.0)).child(text("°").font(k.ty.ui).color(k.c.text_muted)))
                .child(turn_buttons(k));
        }
        BoardSel::Parts(v) => {
            r = r
                .child(title(format!("{} parts", v.len())))
                .child(bar_input(k, s, Field::X, "X", 64.0))
                .child(bar_input(k, s, Field::Y, "Y", 64.0))
                .child(divider(k))
                .child(turn_buttons(k));
        }
        BoardSel::Sheet(i) => {
            let id = s.board().get(*i)?.stock_id;
            let stock = s.board_stock(id)?;
            let b = stock.bbox();
            r = r
                .child(title(stock.name.clone()))
                .child(text(format!("{} × {} mm", fmt_mm(b.width()), fmt_mm(b.height()))).font(k.ty.ui.features("'tnum' 1")).color(k.c.text_muted).single_line());
            if !s.is_running() {
                r = r.child(divider(k)).child(turn_buttons(k));
                if s.stock(id).is_some() {
                    r = r.child(Button::new("Edit").variant(ButtonVariant::Ghost).control_size(rune_kit::ControlSize::Regular).on_press(move |s: &mut App| s.open_editor(Some(id))).build(k));
                }
            }
        }
        BoardSel::None => {
            let id = s.selected?;
            if s.is_running() {
                return None;
            }
            r = r.child(title(name(id)?)).child(size_inputs(k, s, qty(id)));
        }
    }
    let mut bar = r.h(40.0).px(10.0).radius(10.0).fill(k.c.surface_raised).border(1.0, k.c.border_base);
    for sh in &k.elevation.floating {
        bar = bar.box_shadow(*sh);
    }
    Some(bar)
}
