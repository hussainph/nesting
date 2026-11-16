//! Add or edit a sheet or remnant: a rectangle, a shape drawn by clicking corners, or an
//! outline from a DXF or SVG file.

use kurbo::Point;
use rune::{El, LineCap, Shape, Tx, canvas, col, el, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Dialog, Field, Kit, SegmentedControl, TextInput};

use crate::state::{App, Flat, StockDraft, fmt_mm};
use nest_core::StockKind;

const DRAW_W: f32 = 512.0;
const DRAW_H: f32 = 280.0;
const SNAP_MM: f64 = 5.0;

fn field(k: &Kit, label: &str, edit: &rune::TextEdit, key: &str, width: Option<f32>, get: fn(&mut App) -> &mut rune::TextEdit) -> El {
    let input = TextInput::new(edit)
        .label(label)
        .width(width)
        .on_edit(get)
        .on_change(|s: &mut App, _tx| {
            if let Some(d) = s.draft.as_mut() {
                d.error = None;
            }
        })
        .build(k)
        .key(key.to_string());
    let f = Field::new().label(label).control(input).build(k);
    match width {
        Some(w) => el().w(w).child(f),
        None => f,
    }
}

fn draft(s: &mut App) -> &mut StockDraft {
    s.draft.as_mut().expect("stock dialog is open")
}

/// Maps the drawing area (`w` × `h` mm) into the canvas, y-up.
struct DrawMap {
    k: f64,
    ox: f64,
    oy: f64,
}

impl DrawMap {
    fn new(w: f64, h: f64) -> DrawMap {
        let pad = 16.0;
        let k = ((DRAW_W as f64 - 2.0 * pad) / w).min((DRAW_H as f64 - 2.0 * pad) / h).max(1e-6);
        DrawMap { k, ox: (DRAW_W as f64 - w * k) * 0.5, oy: (DRAW_H as f64 + h * k) * 0.5 }
    }
    fn to_screen(&self, p: Point) -> [f32; 2] {
        [(self.ox + p.x * self.k) as f32, (self.oy - p.y * self.k) as f32]
    }
    fn to_mm(&self, x: f32, y: f32, w: f64, h: f64) -> Point {
        let px = ((x as f64 - self.ox) / self.k / SNAP_MM).round() * SNAP_MM;
        let py = ((self.oy - y as f64) / self.k / SNAP_MM).round() * SNAP_MM;
        Point::new(px.clamp(0.0, w), py.clamp(0.0, h))
    }
}

fn draw_canvas(d: &StockDraft, k: &Kit) -> El {
    let (w, h) = d.area_size();
    let map = DrawMap::new(w, h);
    let grid = k.alpha(k.c.text_base, 0.06);
    let edge = k.c.text_base;
    let mut shape = Shape::new();
    // area and grid every 100 mm
    let area = [map.to_screen(Point::new(0.0, 0.0)), map.to_screen(Point::new(w, 0.0)), map.to_screen(Point::new(w, h)), map.to_screen(Point::new(0.0, h))];
    shape = shape.fill(area.to_vec(), k.c.surface_raised).stroke_with(area.to_vec(), true, 1.0, LineCap::Butt, k.c.border_base);
    let step = if w.max(h) > 2000.0 { 200.0 } else { 100.0 };
    let mut x = step;
    while x < w {
        shape = shape.line(map.to_screen(Point::new(x, 0.0)), map.to_screen(Point::new(x, h)), 1.0, LineCap::Butt, grid);
        x += step;
    }
    let mut y = step;
    while y < h {
        shape = shape.line(map.to_screen(Point::new(0.0, y)), map.to_screen(Point::new(w, y)), 1.0, LineCap::Butt, grid);
        y += step;
    }
    let pts: Vec<[f32; 2]> = d.points.iter().map(|p| map.to_screen(*p)).collect();
    if d.closed && pts.len() > 2 {
        shape = shape.fill(pts.clone(), k.alpha(k.c.border_focus, 0.12));
    }
    if pts.len() > 1 {
        shape = shape.stroke_with(pts.clone(), d.closed, 1.5, LineCap::Round, edge);
    }
    if !d.closed {
        if let (Some(last), Some(hv)) = (pts.last(), d.hover) {
            shape = shape.line(*last, map.to_screen(hv), 1.0, LineCap::Round, k.alpha(k.c.text_base, 0.4));
        }
    }
    for (i, p) in pts.iter().enumerate() {
        let r = if i == 0 && !d.closed && pts.len() > 2 { 5.0 } else { 3.0 };
        let dot = (0..12).map(|a| {
            let t = a as f32 / 12.0 * std::f32::consts::TAU;
            [p[0] + r * t.cos(), p[1] + r * t.sin()]
        });
        shape = shape.fill(dot.collect::<Vec<_>>(), if i == 0 { k.c.border_focus } else { edge });
    }
    let readout = match d.hover {
        Some(p) if !d.closed => format!("x {}  y {} mm", fmt_mm(p.x), fmt_mm(p.y)),
        _ => String::new(),
    };
    el().w(DRAW_W)
        .h(DRAW_H)
        .radius(k.radius.md)
        .fill(k.c.bg_deep)
        .clip_rounded(k.radius.md)
        .child(
            canvas(shape)
                .key("draw-area")
                .w(DRAW_W)
                .h(DRAW_H)
                .cursor(rune::Cursor::Crosshair)
                .on_pointer_move_local(|s: &mut App, e, _tx| {
                    let d = draft(s);
                    let (w, h) = d.area_size();
                    d.hover = Some(DrawMap::new(w, h).to_mm(e.x, e.y, w, h));
                })
                .on_hover_change(|s: &mut App, over, _tx| {
                    if !over {
                        draft(s).hover = None;
                    }
                })
                .on_press(|s: &mut App| {
                    let d = draft(s);
                    if d.closed {
                        return;
                    }
                    let Some(p) = d.hover else { return };
                    if d.points.len() > 2 && d.points[0].distance(p) < SNAP_MM * 0.5 {
                        d.closed = true;
                    } else if d.points.last() != Some(&p) {
                        d.points.push(p);
                    }
                    d.error = None;
                }),
        )
        .child(text(readout).font(k.ty.small.features("'tnum' 1")).color(k.c.text_muted).pin(None, Some(8.0), Some(10.0), None))
}

fn preview(outline: &kurbo::BezPath, k: &Kit) -> El {
    let flat = Flat { outer: nest_core::geom::flatten_ring(outline, 0.1), ..Default::default() };
    el().w(DRAW_W)
        .h(DRAW_H)
        .radius(k.radius.md)
        .fill(k.c.bg_deep)
        .center()
        .child(crate::sidebar::thumb(&flat, DRAW_H - 24.0, k.c.surface_raised, k.c.text_base))
}

pub fn view(s: &App, k: &Kit, layer: &mut Vec<El>) {
    let Some(d) = &s.draft else { return };
    let title = match (d.editing.is_some(), d.kind) {
        (true, StockKind::Remnant) => "Edit remnant",
        (true, StockKind::Sheet) => "Edit sheet",
        (false, StockKind::Remnant) => "Add remnant",
        (false, StockKind::Sheet) => "Add sheet",
    };
    let kind = SegmentedControl::new(&["Sheet", "Remnant"], Some(if d.kind == StockKind::Remnant { 1 } else { 0 }))
        .width(Some(DRAW_W))
        .on_pick(|s: &mut App, i, _tx| draft(s).kind = if i == 1 { StockKind::Remnant } else { StockKind::Sheet })
        .build(k);
    let mode = SegmentedControl::new(&["Rectangle", "Draw", "From file"], Some(d.mode))
        .width(Some(DRAW_W))
        .on_pick(|s: &mut App, i, _tx| {
            let d = draft(s);
            d.mode = i;
            d.error = None;
        })
        .build(k);

    let half = (DRAW_W - 12.0) / 2.0;
    let third = (DRAW_W - 24.0) / 3.0;
    let size_row = row()
        .gap(12.0)
        .child(field(k, if d.mode == 1 { "Drawing area width (mm)" } else { "Width (mm)" }, &d.width, "w", Some(half), |s| &mut draft(s).width))
        .child(field(k, if d.mode == 1 { "Drawing area height (mm)" } else { "Height (mm)" }, &d.height, "h", Some(half), |s| &mut draft(s).height));

    let shape_body: El = match d.mode {
        0 => size_row,
        1 => {
            let undo = Button::new("Undo corner")
                .variant(ButtonVariant::Ghost)
                .disabled(d.points.is_empty())
                .on_press(|s: &mut App| {
                    let d = draft(s);
                    if d.closed {
                        d.closed = false;
                    } else {
                        d.points.pop();
                    }
                })
                .build(k);
            let clear = Button::new("Clear")
                .variant(ButtonVariant::Ghost)
                .disabled(d.points.is_empty())
                .on_press(|s: &mut App| {
                    let d = draft(s);
                    d.points.clear();
                    d.closed = false;
                })
                .build(k);
            let hint = if d.closed {
                "Shape closed."
            } else if d.points.len() > 2 {
                "Click the first corner to close the shape."
            } else {
                "Click to place corners. Snaps to 5 mm."
            };
            col()
                .gap(10.0)
                .child(size_row)
                .child(draw_canvas(d, k))
                .child(row().items_center().gap(4.0).child(text(hint).font(k.ty.ui).color(k.c.text_muted)).child(spacer()).child(undo).child(clear))
        }
        _ => {
            let choose = Button::new(if d.file_outline.is_some() { "Choose another file" } else { "Choose file" })
                .variant(ButtonVariant::Neutral)
                .on_press_tx(|_s: &mut App, tx: &mut Tx| {
                    let opts = rune::FileDialogOptions { title: "Choose outline".into(), ..Default::default() }.filter("Drawings", &["dxf", "svg"]);
                    tx.open_file(opts, |s: &mut App, r, _tx| {
                        if let Ok(Some(path)) = r {
                            s.load_outline(path);
                        }
                    });
                })
                .build(k);
            let mut c = col().gap(10.0).child(
                row()
                    .items_center()
                    .gap(10.0)
                    .child(choose)
                    .child(text(d.file_name.clone().unwrap_or_default()).font(k.ty.ui).color(k.c.text_muted).single_line().truncate()),
            );
            if let Some(o) = &d.file_outline {
                let b = kurbo::Shape::bounding_box(o);
                c = c.child(preview(o, k)).child(
                    text(format!("{} × {} mm", fmt_mm(b.width()), fmt_mm(b.height()))).font(k.ty.ui.features("'tnum' 1")).color(k.c.text_muted),
                );
            }
            c
        }
    };

    let details = row()
        .gap(12.0)
        .child(field(k, "Material", &d.material, "material", Some(third), |s| &mut draft(s).material))
        .child(field(k, "Thickness (mm)", &d.thickness, "thickness", Some(third), |s| &mut draft(s).thickness))
        .child(field(k, "Quantity", &d.quantity, "quantity", Some(third), |s| &mut draft(s).quantity));

    let mut footer = row().items_center().gap(8.0).mt(4.0);
    if let Some(id) = d.editing {
        footer = footer.child(Button::new("Delete").variant(ButtonVariant::Danger).on_press(move |s: &mut App| s.delete_stock(id)).build(k));
    }
    footer = footer
        .child(spacer())
        .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.draft = None).build(k))
        .child(Button::new("Save").variant(ButtonVariant::Contrast).on_press(|s: &mut App| s.save_draft()).build(k));

    let mut body = col()
        .gap(14.0)
        .w(DRAW_W + 32.0)
        .px(16.0)
        .pb(16.0)
        .child(kind)
        .child(field(k, "Name", &d.name, "name", Some(DRAW_W), |s| &mut draft(s).name))
        .child(details)
        .child(mode)
        .child(shape_body);
    if let Some(e) = &d.error {
        body = body.child(text(e.clone()).font(k.ty.ui).color(k.c.danger.fg));
    }
    body = body.child(footer);

    Dialog::new("stock-dialog", title)
        .open(true)
        .width(DRAW_W + 32.0)
        .fit()
        .child(body)
        .on_close(|s: &mut App| s.draft = None)
        .build(k, layer);
}

