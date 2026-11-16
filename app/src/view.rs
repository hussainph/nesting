//! The window: toolbar, sidebar, board and status bar.

use rune::{Cx, El, FileDialogOptions, Tx, col, el, row, spacer, text};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, IconButton, IconButtonVariant, Kit, Spinner, Toast, ToastRegion};
use rune_theme_opencode::icons as oc;

use crate::state::{App, StockDraft};
use crate::{board, dialog, icons, sidebar};
use nest_core::StockKind;

pub const IMPORT_EXTENSIONS: [&str; 5] = ["ai", "pdf", "dxf", "svg", "eps"];

pub fn pick_files(_s: &mut App, tx: &mut Tx) {
    let opts = FileDialogOptions { title: "Add parts".into(), ..Default::default() }.filter("Drawings", &["ai", "pdf", "dxf", "svg"]);
    tx.open_file(opts, |s: &mut App, r, _tx| {
        if let Ok(Some(path)) = r {
            s.import_files(vec![path]);
        }
    });
}

pub fn new_stock(s: &mut App) {
    s.draft = Some(StockDraft::new(if s.library.stock.is_empty() { StockKind::Sheet } else { StockKind::Remnant }));
}

pub fn export(_s: &mut App, tx: &mut Tx) {
    let opts = FileDialogOptions { title: "Export".into(), file_name: Some("nest.dxf".into()), ..Default::default() }
        .filter("DXF", &["dxf"])
        .filter("SVG", &["svg"]);
    tx.save_file(opts, |s: &mut App, r, _tx| {
        if let Ok(Some(path)) = r {
            s.export(path);
        }
    });
}

fn toolbar(s: &App, k: &Kit) -> El {
    let add = Button::new("Add files").icon(&icons::IMPORT).variant(ButtonVariant::Ghost).on_press_tx(pick_files).build(k);
    let export_btn = Button::new("Export")
        .icon(&icons::EXPORT)
        .variant(ButtonVariant::Neutral)
        .disabled(s.result.is_none() || s.is_running())
        .on_press_tx(export)
        .build(k);
    let nest = if s.is_running() {
        Button::new("Stop").icon(&icons::STOP).variant(ButtonVariant::Neutral).on_press(|s: &mut App| s.stop_nest()).build(k)
    } else {
        Button::new(if s.result.is_some() { "Nest again" } else { "Nest" })
            .icon(&icons::PLAY)
            .variant(ButtonVariant::Submit)
            .disabled(!s.can_nest())
            .shortcut(if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+Enter" })
            .on_press(|s: &mut App| s.start_nest())
            .build(k)
    };
    let mut clear = el();
    if !s.parts.is_empty() && !s.is_running() {
        clear = Button::new("Clear parts").variant(ButtonVariant::GhostMuted).on_press(|s: &mut App| s.clear_parts()).build(k);
    }
    row()
        .key("toolbar")
        .h(48.0)
        .items_center()
        .gap(8.0)
        .px(10.0)
        .fill(k.c.bg_base)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
        .child(add)
        .child(clear)
        .child(spacer())
        .child(export_btn)
        .child(nest)
}

fn status_bar(s: &App, k: &Kit) -> El {
    let num = k.ty.ui.features("'tnum' 1");
    let mut left = row().items_center().gap(10.0);
    if let Some(run) = &s.run {
        let secs = run.started.elapsed().as_secs_f64();
        left = left.child(Spinner::new().build(k)).child(text(format!("Nesting · {:.0} s of {:.0} s", secs, s.settings.time_limit_secs)).font(num).color(k.c.text_base));
    }
    if let (Some(r), Some(job)) = (&s.result, &s.result_job) {
        let total: u32 = job.parts.iter().map(|p| p.quantity).sum();
        let sheets = r.sheets.len();
        left = left.child(
            text(format!(
                "{} {} · {:.1}% used · {} of {} placed",
                sheets,
                if sheets == 1 { "sheet" } else { "sheets" },
                r.utilization(job) * 100.0,
                r.placed_count(),
                total
            ))
            .font(num)
            .color(k.c.text_base),
        );
        let missing = r.unplaced_count();
        if missing > 0 && s.run.is_none() {
            left = left.child(Badge::new(format!("{missing} didn't fit")).variant(BadgeVariant::Accent).build(k));
        }
        if s.stale && s.run.is_none() {
            left = left.child(text("Changed since this nest").font(k.ty.ui).color(k.c.text_muted));
        }
    } else if s.run.is_none() {
        let q = s.total_quantity();
        let n = s.use_stock.len();
        left = left.child(
            text(format!("{} {} · {} stock {}", q, if q == 1 { "part" } else { "parts" }, n, if n == 1 { "item" } else { "items" }))
                .font(num)
                .color(k.c.text_muted),
        );
    }
    let zoom = row()
        .items_center()
        .gap(2.0)
        .child(IconButton::new(&oc::MINUS).variant(IconButtonVariant::GhostMuted).label("Zoom out").on_press(|s: &mut App| {
            let [w, h] = s.canvas;
            s.cam.zoom_about(w * 0.5, h * 0.5, 1.0 / 1.25);
        }).build(k))
        .child(text(format!("{:.0}%", s.cam.zoom * 100.0 / (72.0 / 25.4))).font(num).color(k.c.text_muted).w(44.0).align(rune::Align::Center))
        .child(IconButton::new(&oc::PLUS).variant(IconButtonVariant::GhostMuted).label("Zoom in").on_press(|s: &mut App| {
            let [w, h] = s.canvas;
            s.cam.zoom_about(w * 0.5, h * 0.5, 1.25);
        }).build(k))
        .child(IconButton::new(&icons::FIT).variant(IconButtonVariant::GhostMuted).label("Fit to window").help("Fit to window").on_press(|s: &mut App| s.fit()).build(k));
    row()
        .key("status")
        .h(36.0)
        .items_center()
        .px(12.0)
        .fill(k.c.bg_base)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_weak)
        .child(left)
        .child(spacer())
        .child(zoom)
}

fn notices(s: &App, k: &Kit) -> Option<El> {
    if s.notices.is_empty() {
        return None;
    }
    let mut region = ToastRegion::new();
    for n in &s.notices {
        let id = n.id;
        let mut t = Toast::new(format!("notice-{id}"), n.title.clone()).closable().on_close(move |s: &mut App| s.notices.retain(|x| x.id != id));
        if n.error {
            t = t.icon(k, &oc::CIRCLE_EXCLAMATION);
        }
        if let Some(d) = &n.detail {
            t = t.description(d.clone());
        }
        region = region.toast(t.build(k));
    }
    Some(region.build(k))
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = rune_theme_opencode::kit(cx.system_dark());
    let mut layer = Vec::new();
    let side = sidebar::view(s, k, cx, &mut layer);
    let main = col().grow(1.0).min_w(0.0).h_full().min_h(0.0).child(board::view(s, k, cx)).child(status_bar(s, k));
    let body = col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(toolbar(s, k))
        .child(row().w_full().grow(1.0).min_h(0.0).child(side).child(main))
        .on_drop_files(|s: &mut App, files, _tx| {
            s.drop_over = false;
            s.import_files(files.to_vec());
        })
        .accepts_extensions(&IMPORT_EXTENSIONS)
        .on_drop_hover(|s: &mut App, over, _tx| s.drop_over = over);
    dialog::view(s, k, &mut layer);
    if let Some(n) = notices(s, k) {
        layer.push(n);
    }
    if s.drop_over {
        layer.push(
            el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .fill(k.alpha(k.c.border_focus, 0.08))
                .border(2.0, k.c.border_focus)
                .center()
                .child(text("Drop to add parts").font(k.ty.body_medium).color(k.c.text_strong)),
        );
    }
    rune_kit::root(body, layer)
}
