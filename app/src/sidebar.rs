//! The left panel: parts, stock and nest settings.

use kurbo::{Affine, Point};
use rune::{Color, Cx, El, LineCap, Shape, canvas, col, el, row, spacer, text};
use rune_kit::{Checkbox, IconButton, IconButtonVariant, Kit, Select, Switch, TextInput};
use rune_theme_opencode::icons as oc;

use crate::state::{App, Flat, fmt_mm};
use nest_core::{Rotation, StockKind};

pub const WIDTH: f32 = 312.0;
const ROTATIONS: [&str; 4] = ["Any angle", "90° steps", "180° steps", "As drawn"];

/// A small drawing of an outline, fitted into a `size` square.
pub fn thumb(flat: &Flat, size: f32, fill: Color, edge: Color) -> El {
    let mut min = Point::new(f64::MAX, f64::MAX);
    let mut max = Point::new(f64::MIN, f64::MIN);
    for p in &flat.outer {
        min = Point::new(min.x.min(p.x), min.y.min(p.y));
        max = Point::new(max.x.max(p.x), max.y.max(p.y));
    }
    let mut shape = Shape::new();
    if flat.outer.len() > 2 {
        let pad = 3.0;
        let w = (max.x - min.x).max(1e-6);
        let h = (max.y - min.y).max(1e-6);
        let k = ((size as f64 - 2.0 * pad) / w.max(h)).max(1e-6);
        let ox = (size as f64 - w * k) * 0.5;
        let oy = (size as f64 - h * k) * 0.5;
        // y-up to y-down
        let t = Affine::new([k, 0.0, 0.0, -k, ox - min.x * k, oy + max.y * k]);
        let map = |pts: &[Point]| pts.iter().map(|p| { let q = t * *p; [q.x as f32, q.y as f32] }).collect::<Vec<_>>();
        let outer = map(&flat.outer);
        shape = shape.fill(outer.clone(), fill).stroke_with(outer, true, 1.0, LineCap::Round, edge);
        for h in &flat.holes {
            if h.len() > 2 {
                let hole = map(h);
                shape = shape.stroke_with(hole, true, 1.0, LineCap::Round, edge);
            }
        }
    }
    canvas(shape).size(size, size)
}

fn section_header(k: &Kit, title: &str, count: Option<String>, action: El) -> El {
    let mut r = row().items_center().h(32.0).px(12.0).gap(6.0).child(text(title).font(k.ty.ui_medium).color(k.c.text_strong));
    if let Some(n) = count {
        r = r.child(text(n).font(k.ty.ui.features("'tnum' 1")).color(k.c.text_faint));
    }
    r.child(spacer()).child(action)
}

fn parts_section(s: &App, k: &Kit) -> El {
    let add = IconButton::new(&oc::PLUS)
        .variant(IconButtonVariant::Ghost)
        .label("Add files")
        .help("Add files")
        .on_press_tx(|s: &mut App, tx| crate::view::pick_files(s, tx))
        .build(k);
    let count = (!s.parts.is_empty()).then(|| s.total_quantity().to_string());
    let mut list = col().gap(1.0).px(6.0);
    if s.parts.is_empty() {
        list = list.child(text(if s.importing > 0 { "Importing…" } else { "No parts yet" }).font(k.ty.ui).color(k.c.text_faint).px(6.0).py(4.0));
    }
    let fill = k.alpha(k.c.text_base, 0.08);
    for part in &s.parts {
        let id = part.id;
        let selected = s.selected == Some(id);
        let b = part.geometry.bbox();
        let flat = s.flats.get(&id).cloned().unwrap_or_default();
        let qty = s.qty_edits.get(&id);
        let qty_el = match qty {
            Some(edit) => TextInput::new(edit)
                .width(Some(52.0))
                .height(26.0)
                .text_style(k.ty.ui_compact.features("'tnum' 1"))
                .on_edit(move |s: &mut App| s.qty_edits.get_mut(&id).expect("qty edit for part"))
                .on_change(move |s: &mut App, _tx| s.qty_changed(id))
                .build(k)
                .key(format!("qty-{id}")),
            None => el(),
        };
        let remove = IconButton::new(&oc::CLOSE)
            .variant(IconButtonVariant::GhostMuted)
            .label("Remove")
            .on_press(move |s: &mut App| s.remove_part(id))
            .build(k);
        let info = col()
            .grow(1.0)
            .min_w(0.0)
            .child(text(part.name.clone()).font(k.ty.ui).color(k.c.text_strong).single_line().truncate())
            .child(text(format!("{} × {} mm", fmt_mm(b.width()), fmt_mm(b.height()))).font(k.ty.small.features("'tnum' 1")).color(k.c.text_muted));
        let r = row()
            .key(format!("part-{id}"))
            .items_center()
            .gap(10.0)
            .px(6.0)
            .py(5.0)
            .radius(k.radius.md)
            .fill(if selected { k.c.surface_hover } else { Color::TRANSPARENT })
            .hover(k.c.hover)
            .on_press(move |s: &mut App| s.selected = if s.selected == Some(id) { None } else { Some(id) })
            .child(thumb(&flat, 32.0, fill, k.c.text_muted))
            .child(info)
            .child(qty_el)
            .child(remove);
        list = list.child(r);
    }
    col().child(section_header(k, "Parts", count, add)).child(list)
}

fn stock_section(s: &App, k: &Kit) -> El {
    let add = IconButton::new(&oc::PLUS)
        .variant(IconButtonVariant::Ghost)
        .label("Add stock")
        .help("Add stock")
        .on_press(|s: &mut App| crate::view::new_stock(s))
        .build(k);
    let mut list = col().gap(1.0).px(6.0);
    if s.library.stock.is_empty() {
        list = list.child(text("No stock yet").font(k.ty.ui).color(k.c.text_faint).px(6.0).py(4.0));
    }
    let fill = k.alpha(k.c.text_base, 0.04);
    for stock in &s.library.stock {
        let id = stock.id;
        let on = s.use_stock.contains(&id);
        let b = stock.bbox();
        let flat = s.stock_flats.get(&id).cloned().unwrap_or_default();
        let mut detail = vec![if stock.kind == StockKind::Remnant { "Remnant".to_string() } else { "Sheet".to_string() }];
        if !stock.material.is_empty() {
            detail.push(stock.material.clone());
        }
        if stock.thickness > 0.0 {
            detail.push(format!("{} mm", fmt_mm(stock.thickness)));
        }
        if stock.quantity > 1 {
            detail.push(format!("×{}", stock.quantity));
        }
        let check = Checkbox::new(on).on_toggle(move |s: &mut App| s.toggle_stock(id)).build(k);
        let name = if stock.name.is_empty() { format!("{} × {}", fmt_mm(b.width()), fmt_mm(b.height())) } else { stock.name.clone() };
        let info = col()
            .grow(1.0)
            .min_w(0.0)
            .child(text(name).font(k.ty.ui).color(if on { k.c.text_strong } else { k.c.text_muted }).single_line().truncate())
            .child(text(detail.join(" · ")).font(k.ty.small.features("'tnum' 1")).color(k.c.text_muted).single_line().truncate());
        let edit = IconButton::new(&oc::EDIT)
            .variant(IconButtonVariant::GhostMuted)
            .label("Edit")
            .on_press(move |s: &mut App| s.open_editor(Some(id)))
            .build(k);
        list = list.child(
            row()
                .key(format!("stock-{id}"))
                .items_center()
                .gap(10.0)
                .px(6.0)
                .py(5.0)
                .radius(k.radius.md)
                .hover(k.c.hover)
                .child(check)
                .child(thumb(&flat, 32.0, fill, k.c.text_muted))
                .child(info)
                .child(edit),
        );
    }
    col().child(section_header(k, "Stock", None, add)).child(list)
}

fn setting_row(k: &Kit, label: &str, control: El) -> El {
    row().items_center().h(34.0).px(12.0).child(text(label).font(k.ty.ui).color(k.c.text_base)).child(spacer()).child(control)
}

fn number_input(k: &Kit, key: &str, edit: &rune::TextEdit, suffix: &str, get: fn(&mut App) -> &mut rune::TextEdit) -> El {
    row()
        .items_center()
        .gap(6.0)
        .child(
            TextInput::new(edit)
                .width(Some(64.0))
                .height(26.0)
                .text_style(k.ty.ui_compact.features("'tnum' 1"))
                .on_edit(get)
                .on_change(|s: &mut App, _tx| s.settings_changed())
                .build(k)
                .key(key.to_string()),
        )
        .child(text(suffix.to_string()).font(k.ty.ui).color(k.c.text_muted).w(18.0))
}

fn settings_section(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let current = match s.settings.default_rotation {
        Rotation::Any => ROTATIONS[0],
        Rotation::Step(d) if d >= 180.0 => ROTATIONS[2],
        Rotation::Step(_) => ROTATIONS[1],
        Rotation::Fixed => ROTATIONS[3],
    };
    let rotation = Select::new("rotation", &ROTATIONS)
        .current(Some(current))
        .width(132.0)
        .appearance(rune_kit::SelectAppearance::Field)
        .open(s.open_select == Some("rotation"))
        .on_toggle(|s: &mut App| s.open_select = if s.open_select.is_some() { None } else { Some("rotation") })
        .on_close(|s: &mut App| s.open_select = None)
        .on_choose(|s: &mut App, v: &str, _tx| {
            s.open_select = None;
            s.set_rotation(match v {
                "90° steps" => Rotation::Step(90.0),
                "180° steps" => Rotation::Step(180.0),
                "As drawn" => Rotation::Fixed,
                _ => Rotation::Any,
            });
        })
        .build(k, layer);
    let remnants = Switch::new(s.settings.remnants_first)
        .on_toggle(|s: &mut App| {
            s.settings.remnants_first = !s.settings.remnants_first;
            s.settings_changed();
        })
        .build(k);
    col()
        .pb(8.0)
        .child(row().items_center().h(32.0).px(12.0).child(text("Settings").font(k.ty.ui_medium).color(k.c.text_strong)))
        .child(setting_row(k, "Spacing", number_input(k, "spacing", &s.spacing, "mm", |s| &mut s.spacing)))
        .child(setting_row(k, "Edge margin", number_input(k, "margin", &s.margin, "mm", |s| &mut s.margin)))
        .child(setting_row(k, "Rotation", rotation))
        .child(setting_row(k, "Time limit", number_input(k, "time", &s.time, "s", |s| &mut s.time)))
        .child(setting_row(k, "Use remnants first", remnants))
}

pub fn view(s: &App, k: &Kit, _cx: &mut Cx<App>, layer: &mut Vec<El>) -> El {
    let divider = || el().h(1.0).w_full().fill(k.c.border_weak).my(6.0);
    let body = col()
        .w_full()
        .pt(6.0)
        .child(parts_section(s, k))
        .child(divider())
        .child(stock_section(s, k))
        .child(divider())
        .child(settings_section(s, k, layer));
    let scroller = col().key("sidebar-scroll").w_full().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec { stick_bottom: false }).child(body);
    col()
        .key("sidebar")
        .w(WIDTH)
        .h_full()
        .min_h(0.0)
        .fill(k.c.bg_base)
        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_weak)
        .child(k.scroll_bar(scroller))
}
