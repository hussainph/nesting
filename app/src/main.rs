//! Nesting: lay out laser and router parts on sheets and remnants.

// No console window behind the app in Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod board;
mod editor;
mod icons;
mod scenes;
mod sidebar;
mod state;
mod view;

use rune::{AppSpec, Key, KeyEvent, Tx};
use state::App;

fn setup(s: &mut App, waker: rune::Waker) {
    s.bus.set_waker(waker);
}

fn poll(s: &mut App, now: f64, tx: &mut Tx) -> bool {
    let changed = s.poll(now);
    if s.run.is_some() {
        // Keep the elapsed time ticking while a nest runs.
        tx.wake_after(std::time::Duration::from_millis(500));
    }
    if let Some(next) = s.notices.iter().map(|n| n.until).reduce(f64::min) {
        tx.wake_after(std::time::Duration::from_secs_f64((next - now).max(0.05)));
    }
    changed || s.run.is_some()
}

fn on_key(s: &mut App, e: &KeyEvent, tx: &mut Tx) -> bool {
    let cmd = e.mods.cmd || e.mods.ctrl;
    if s.editor.is_some() {
        return editor_key(s, e);
    }
    match &e.key {
        Key::Enter if cmd => {
            s.start_nest();
            true
        }
        Key::Command('o') => {
            view::pick_files(s, tx);
            true
        }
        Key::Escape if s.run.is_some() => {
            s.stop_nest();
            true
        }
        _ => false,
    }
}

/// Editor keys: delete, nudge with arrows (Shift for 10 mm), undo and redo, Escape to deselect.
fn editor_key(s: &mut App, e: &KeyEvent) -> bool {
    use editor::{Sel, Tool};
    let Some(ed) = s.editor.as_mut() else { return false };
    let step = if e.mods.shift { 10.0 } else { 1.0 };
    match &e.key {
        Key::Backspace | Key::Delete => ed.delete_selection(),
        Key::Left => ed.nudge(-step, 0.0),
        Key::Right => ed.nudge(step, 0.0),
        Key::Up => ed.nudge(0.0, step),
        Key::Down => ed.nudge(0.0, -step),
        Key::Command('z') if e.mods.shift => ed.redo(),
        Key::Command('z') => ed.undo(),
        Key::Command('y') => ed.redo(),
        Key::Command('s') => s.save_editor(),
        Key::Escape => {
            if ed.tool == Tool::Damage {
                ed.tool = Tool::Shape;
            } else {
                ed.select(Sel::None);
            }
        }
        _ => return false,
    }
    true
}

fn main() {
    let spec = AppSpec {
        min_size: Some((900.0, 600.0)),
        scenes: scenes::SCENES,
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("Nesting", (1280.0, 820.0), App::new, view::view)
    };
    rune::App::new(spec).titlebar(rune::Titlebar::new(view::TITLEBAR_H)).run();
}
