//! App state and the actions that change it. Slow work (imports, nesting, export) runs on
//! threads and reports back through `Bus`, which `poll` drains on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kurbo::{BezPath, Point, Shape};
use nest_core::geom::{flatten, flatten_ring};
use nest_core::library::Library;
use nest_core::{NestJob, NestResult, NestSettings, Part, PartId, Rotation, Stock, StockId, StockKind};
use rune::{TextEdit, Waker};

/// Tolerance for the polylines the canvas draws, in mm.
const DRAW_TOLERANCE: f64 = 0.05;

pub enum Msg {
    Imported { path: PathBuf, result: Result<nest_io::ImportReport, String> },
    Progress(NestResult),
    Done(Result<NestResult, String>),
    Outline(Result<BezPath, String>),
    Exported(Result<Vec<PathBuf>, String>),
}

#[derive(Default)]
pub struct Bus {
    queue: Mutex<Vec<Msg>>,
    waker: Mutex<Option<Waker>>,
}

impl Bus {
    pub fn send(&self, msg: Msg) {
        self.queue.lock().unwrap().push(msg);
        if let Some(w) = self.waker.lock().unwrap().as_ref() {
            w();
        }
    }
    pub fn set_waker(&self, w: Waker) {
        *self.waker.lock().unwrap() = Some(w);
    }
    fn drain(&self) -> Vec<Msg> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

/// Part outlines flattened once, in part coordinates (mm), for drawing.
#[derive(Clone, Default)]
pub struct Flat {
    pub outer: Vec<Point>,
    pub holes: Vec<Vec<Point>>,
    pub marks: Vec<(Vec<Point>, bool)>,
}

impl Flat {
    pub fn of_part(p: &Part) -> Flat {
        Flat {
            outer: flatten_ring(&p.geometry.outer, DRAW_TOLERANCE),
            holes: p.geometry.holes.iter().map(|h| flatten_ring(h, DRAW_TOLERANCE)).collect(),
            marks: p.geometry.marks.iter().flat_map(|m| flatten(m, DRAW_TOLERANCE)).collect(),
        }
    }
    pub fn of_stock(s: &Stock) -> Flat {
        Flat {
            outer: flatten_ring(&s.outline, DRAW_TOLERANCE),
            holes: s.defects.iter().map(|h| flatten_ring(h, DRAW_TOLERANCE)).collect(),
            marks: Vec::new(),
        }
    }
}

pub struct Run {
    pub ctl: Arc<nest_engine::Control>,
    pub started: Instant,
}

/// Screen = world (mm, y-up) × zoom, y flipped, + pan. Points, relative to the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub zoom: f32,
    pub pan: [f32; 2],
}

impl Default for Camera {
    fn default() -> Self {
        Camera { zoom: 0.3, pan: [40.0, 400.0] }
    }
}

impl Camera {
    pub fn to_screen(&self, p: Point) -> [f32; 2] {
        [p.x as f32 * self.zoom + self.pan[0], -(p.y as f32) * self.zoom + self.pan[1]]
    }
    pub fn to_world(&self, x: f32, y: f32) -> Point {
        Point::new(((x - self.pan[0]) / self.zoom) as f64, (-(y - self.pan[1]) / self.zoom) as f64)
    }
    /// Zooms by `factor` keeping the world point under (x, y) fixed.
    pub fn zoom_about(&mut self, x: f32, y: f32, factor: f32) {
        let z = (self.zoom * factor).clamp(0.01, 200.0);
        let k = z / self.zoom;
        self.pan = [x - (x - self.pan[0]) * k, y - (y - self.pan[1]) * k];
        self.zoom = z;
    }
}

/// A sheet on the board: a stock item at a board offset, with the parts placed on it.
pub struct BoardSheet {
    pub stock_id: StockId,
    pub offset: Point,
    pub placements: Vec<nest_core::Placement>,
    pub utilization: Option<f64>,
}

pub struct StockDraft {
    pub editing: Option<StockId>,
    pub mode: usize, // 0 rectangle, 1 draw, 2 from file
    pub kind: StockKind,
    pub name: TextEdit,
    pub material: TextEdit,
    pub thickness: TextEdit,
    pub quantity: TextEdit,
    pub width: TextEdit,
    pub height: TextEdit,
    pub points: Vec<Point>,
    pub closed: bool,
    pub hover: Option<Point>,
    pub file_outline: Option<BezPath>,
    pub file_name: Option<String>,
    pub error: Option<String>,
}

impl StockDraft {
    pub fn new(kind: StockKind) -> StockDraft {
        StockDraft {
            editing: None,
            mode: 0,
            kind,
            name: TextEdit::new(""),
            material: TextEdit::new(""),
            thickness: TextEdit::new(""),
            quantity: TextEdit::new("1"),
            width: TextEdit::new(if kind == StockKind::Sheet { "1200" } else { "600" }),
            height: TextEdit::new(if kind == StockKind::Sheet { "900" } else { "400" }),
            points: Vec::new(),
            closed: false,
            hover: None,
            file_outline: None,
            file_name: None,
            error: None,
        }
    }

    pub fn area_size(&self) -> (f64, f64) {
        let w = parse_pos(&self.width.text).unwrap_or(600.0);
        let h = parse_pos(&self.height.text).unwrap_or(400.0);
        (w, h)
    }

    /// The outline the draft describes, or a message saying what's missing.
    pub fn outline(&self) -> Result<BezPath, String> {
        match self.mode {
            0 => {
                let w = parse_pos(&self.width.text).ok_or("Enter a width in mm.")?;
                let h = parse_pos(&self.height.text).ok_or("Enter a height in mm.")?;
                Ok(kurbo::Rect::new(0.0, 0.0, w, h).to_path(0.01))
            }
            1 => {
                if self.points.len() < 3 {
                    return Err("Click at least 3 corners to draw the shape.".into());
                }
                let path = nest_core::geom::polygon_path(&self.points);
                let b = path.bounding_box();
                Ok(kurbo::Affine::translate((-b.x0, -b.y0)) * path)
            }
            _ => self.file_outline.clone().ok_or_else(|| "Choose a DXF or SVG file with the outline.".into()),
        }
    }
}

#[derive(Clone)]
pub struct Notice {
    pub id: u64,
    pub title: String,
    pub detail: Option<String>,
    pub error: bool,
    pub until: f64,
}

pub struct App {
    pub bus: Arc<Bus>,
    pub parts: Vec<Part>,
    pub flats: HashMap<PartId, Flat>,
    pub qty_edits: HashMap<PartId, TextEdit>,
    pub next_part: PartId,
    pub selected: Option<PartId>,
    pub importing: usize,

    pub library: Library,
    pub library_path: PathBuf,
    pub stock_flats: HashMap<StockId, Flat>,
    /// Stock items switched on for this job.
    pub use_stock: Vec<StockId>,

    pub settings: NestSettings,
    pub spacing: TextEdit,
    pub margin: TextEdit,
    pub time: TextEdit,
    pub open_select: Option<&'static str>,

    pub run: Option<Run>,
    pub result: Option<NestResult>,
    pub result_job: Option<NestJob>,
    /// Parts, stock or settings changed after the last nest.
    pub stale: bool,
    pub elapsed: f64,

    pub cam: Camera,
    pub drag_last: Option<[f32; 2]>,
    pub canvas: [f32; 2],
    pub fit_pending: bool,

    pub draft: Option<StockDraft>,
    /// Result sheets whose offcut was already saved.
    pub saved_offcuts: Vec<usize>,
    pub drop_over: bool,
    pub notices: Vec<Notice>,
    pub next_notice: u64,
    pub now: f64,
}

pub fn library_path() -> PathBuf {
    if let Ok(p) = std::env::var("NESTING_LIBRARY") {
        return PathBuf::from(p);
    }
    let base = if cfg!(windows) {
        std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    } else if cfg!(target_os = "macos") {
        std::env::var("HOME").map(|h| PathBuf::from(h).join("Library/Application Support")).unwrap_or_else(|_| PathBuf::from("."))
    } else {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|_| PathBuf::from("."))
    };
    base.join("Nesting").join("library.json")
}

pub fn parse_pos(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

pub fn parse_nonneg(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

pub fn fmt_mm(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 { format!("{:.0}", v) } else { format!("{:.1}", v) }
}

impl App {
    pub fn new() -> App {
        let library_path = library_path();
        let library = Library::load(&library_path).unwrap_or_default();
        let settings = NestSettings::default();
        let mut app = App {
            bus: Arc::new(Bus::default()),
            parts: Vec::new(),
            flats: HashMap::new(),
            qty_edits: HashMap::new(),
            next_part: 1,
            selected: None,
            importing: 0,
            use_stock: library.stock.iter().map(|s| s.id).collect(),
            library,
            library_path,
            stock_flats: HashMap::new(),
            spacing: TextEdit::new(fmt_mm(settings.spacing)),
            margin: TextEdit::new(fmt_mm(settings.edge_margin)),
            time: TextEdit::new(format!("{}", settings.time_limit_secs as u32)),
            settings,
            open_select: None,
            run: None,
            result: None,
            result_job: None,
            stale: false,
            elapsed: 0.0,
            cam: Camera::default(),
            drag_last: None,
            canvas: [0.0, 0.0],
            fit_pending: true,
            draft: None,
            saved_offcuts: Vec::new(),
            drop_over: false,
            notices: Vec::new(),
            next_notice: 1,
            now: 0.0,
        };
        app.refresh_stock_flats();
        // Files passed on the command line (Open with, or dropped on the exe) import at launch.
        let files: Vec<PathBuf> = std::env::args()
            .skip(1)
            .map(PathBuf::from)
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|e| e.to_str()).is_some_and(|e| crate::view::IMPORT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            })
            .collect();
        if !files.is_empty() {
            app.import_files(files);
        }
        app
    }

    pub fn refresh_stock_flats(&mut self) {
        self.stock_flats = self.library.stock.iter().map(|s| (s.id, Flat::of_stock(s))).collect();
    }

    pub fn stock(&self, id: StockId) -> Option<&Stock> {
        self.library.stock.iter().find(|s| s.id == id)
    }

    pub fn part(&self, id: PartId) -> Option<&Part> {
        self.parts.iter().find(|p| p.id == id)
    }

    pub fn notify(&mut self, title: impl Into<String>, detail: Option<String>, error: bool) {
        let id = self.next_notice;
        self.next_notice += 1;
        let until = self.now + if error { 8.0 } else { 4.0 };
        self.notices.push(Notice { id, title: title.into(), detail, error, until });
        if self.notices.len() > 3 {
            self.notices.remove(0);
        }
    }

    pub fn total_quantity(&self) -> u32 {
        self.parts.iter().map(|p| p.quantity).sum()
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    fn changed(&mut self) {
        if self.result.is_some() {
            self.stale = true;
        }
    }

    // ------------------------------------------------------------------ parts

    pub fn import_files(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            self.importing += 1;
            let bus = self.bus.clone();
            std::thread::spawn(move || {
                let result = nest_io::import_path(&path).map_err(|e| e.to_string());
                bus.send(Msg::Imported { path, result });
            });
        }
    }

    fn on_imported(&mut self, path: &Path, result: Result<nest_io::ImportReport, String>) {
        self.importing = self.importing.saturating_sub(1);
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        match result {
            Ok(report) => {
                if report.parts.is_empty() {
                    self.notify(format!("No closed shapes in {file}"), Some("Check that outlines are closed paths.".into()), true);
                    return;
                }
                let n = report.parts.len();
                for imported in report.parts {
                    let id = self.next_part;
                    self.next_part += 1;
                    let part = Part {
                        id,
                        name: imported.name,
                        source: Some(file.clone()),
                        geometry: imported.geometry,
                        quantity: 1,
                        rotation: None,
                        allow_mirror: false,
                    };
                    self.flats.insert(id, Flat::of_part(&part));
                    self.qty_edits.insert(id, TextEdit::new("1"));
                    self.parts.push(part);
                }
                if let Some(w) = report.warnings.first() {
                    let more = report.warnings.len() - 1;
                    let detail = if more > 0 { format!("{w} ({more} more)") } else { w.clone() };
                    self.notify(format!("Imported {n} parts from {file}"), Some(detail), false);
                }
                self.changed();
                if self.result.is_none() {
                    self.fit_pending = true;
                }
            }
            Err(e) => self.notify(format!("Couldn't import {file}"), Some(e), true),
        }
    }

    pub fn add_part(&mut self, name: &str, geometry: nest_core::PartGeometry, quantity: u32) {
        let id = self.next_part;
        self.next_part += 1;
        let part = Part { id, name: name.into(), source: None, geometry, quantity, rotation: None, allow_mirror: false };
        self.flats.insert(id, Flat::of_part(&part));
        self.qty_edits.insert(id, TextEdit::new(quantity.to_string()));
        self.parts.push(part);
    }

    pub fn remove_part(&mut self, id: PartId) {
        self.parts.retain(|p| p.id != id);
        self.flats.remove(&id);
        self.qty_edits.remove(&id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.changed();
    }

    pub fn clear_parts(&mut self) {
        self.parts.clear();
        self.flats.clear();
        self.qty_edits.clear();
        self.selected = None;
        self.result = None;
        self.result_job = None;
        self.stale = false;
        self.fit_pending = true;
    }

    pub fn qty_changed(&mut self, id: PartId) {
        let Some(edit) = self.qty_edits.get(&id) else { return };
        let q = edit.text.trim().parse::<u32>().ok();
        if let (Some(q), Some(part)) = (q, self.parts.iter_mut().find(|p| p.id == id)) {
            if part.quantity != q {
                part.quantity = q;
                self.changed();
            }
        }
    }

    // ------------------------------------------------------------------ stock

    pub fn toggle_stock(&mut self, id: StockId) {
        if let Some(i) = self.use_stock.iter().position(|s| *s == id) {
            self.use_stock.remove(i);
        } else {
            self.use_stock.push(id);
        }
        self.changed();
        if self.result.is_none() {
            self.fit_pending = true;
        }
    }

    pub fn save_draft(&mut self) {
        let Some(d) = self.draft.as_mut() else { return };
        let outline = match d.outline() {
            Ok(o) => o,
            Err(e) => {
                d.error = Some(e);
                return;
            }
        };
        let Some(quantity) = d.quantity.text.trim().parse::<u32>().ok().filter(|q| *q > 0) else {
            d.error = Some("Enter how many you have.".into());
            return;
        };
        let b = outline.bounding_box();
        let default_name = format!(
            "{} {} × {}",
            if d.kind == StockKind::Remnant { "Remnant" } else { "Sheet" },
            fmt_mm(b.width()),
            fmt_mm(b.height())
        );
        let name = if d.name.text.trim().is_empty() { default_name } else { d.name.text.trim().to_string() };
        let stock = Stock {
            id: d.editing.unwrap_or_else(|| self.library.next_id()),
            name,
            kind: d.kind,
            outline,
            defects: Vec::new(),
            quantity,
            material: d.material.text.trim().to_string(),
            thickness: parse_nonneg(&d.thickness.text).unwrap_or(0.0),
        };
        let id = stock.id;
        match self.library.stock.iter_mut().find(|s| s.id == id) {
            Some(existing) => *existing = stock,
            None => {
                self.library.stock.push(stock);
                self.use_stock.push(id);
            }
        }
        self.draft = None;
        self.persist_library();
        self.refresh_stock_flats();
        self.changed();
        if self.result.is_none() {
            self.fit_pending = true;
        }
    }

    pub fn edit_stock(&mut self, id: StockId) {
        let Some(s) = self.stock(id).cloned() else { return };
        let mut d = StockDraft::new(s.kind);
        d.editing = Some(id);
        d.name = TextEdit::new(s.name.clone());
        d.material = TextEdit::new(s.material.clone());
        d.thickness = TextEdit::new(if s.thickness > 0.0 { fmt_mm(s.thickness) } else { String::new() });
        d.quantity = TextEdit::new(s.quantity.to_string());
        let b = s.bbox();
        d.width = TextEdit::new(fmt_mm(b.width()));
        d.height = TextEdit::new(fmt_mm(b.height()));
        let ring = flatten_ring(&s.outline, 0.01);
        if ring.len() != 4 {
            d.mode = 1;
            d.points = ring;
            d.closed = true;
        }
        self.draft = Some(d);
    }

    /// The unused end of a result sheet, cut straight across past the last part: whichever of
    /// a vertical or horizontal cut leaves more material. None when what's left is too small
    /// to be worth keeping (under 100 mm on a side).
    pub fn offcut(&self, sheet_index: usize) -> Option<Offcut> {
        let result = self.result.as_ref()?;
        let job = self.result_job.as_ref()?;
        let sheet = result.sheets.get(sheet_index)?;
        let stock = job.stock.iter().find(|s| s.id == sheet.stock_id)?;
        let ring = flatten_ring(&stock.outline, 0.05);
        let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
        for p in &sheet.placements {
            let flat = self.flats.get(&p.part_id)?;
            let t = p.affine();
            for q in &flat.outer {
                let w = t * *q;
                max_x = max_x.max(w.x);
                max_y = max_y.max(w.y);
            }
        }
        if max_x == f64::MIN {
            return None;
        }
        let gap = job.settings.spacing.max(1.0) + 1.0;
        let right = clip_half(&ring, |p| p.x - (max_x + gap));
        let top = clip_half(&ring, |p| p.y - (max_y + gap));
        let area = |r: &[Point]| nest_core::geom::ring_area(r).abs();
        let (keep, cut) = if area(&right) >= area(&top) { (right, Cut::X(max_x + gap)) } else { (top, Cut::Y(max_y + gap)) };
        let path = nest_core::geom::polygon_path(&keep);
        let b = path.bounding_box();
        if keep.len() < 3 || b.width() < 100.0 || b.height() < 100.0 {
            return None;
        }
        Some(Offcut { outline: kurbo::Affine::translate((-b.x0, -b.y0)) * path, cut, stock_name: stock.name.clone(), material: stock.material.clone(), thickness: stock.thickness })
    }

    pub fn save_offcut(&mut self, sheet_index: usize) {
        let Some(o) = self.offcut(sheet_index) else { return };
        let b = o.outline.bounding_box();
        let id = self.library.next_id();
        self.library.stock.push(Stock {
            id,
            name: format!("Offcut {} × {}", fmt_mm(b.width()), fmt_mm(b.height())),
            kind: StockKind::Remnant,
            outline: o.outline,
            defects: Vec::new(),
            quantity: 1,
            material: o.material,
            thickness: o.thickness,
        });
        self.persist_library();
        self.refresh_stock_flats();
        self.saved_offcuts.push(sheet_index);
        self.notify(format!("Saved offcut {} × {} mm", fmt_mm(b.width()), fmt_mm(b.height())), Some(format!("Cut from {}.", o.stock_name)), false);
    }

    pub fn delete_stock(&mut self, id: StockId) {
        self.library.stock.retain(|s| s.id != id);
        self.use_stock.retain(|s| *s != id);
        self.persist_library();
        self.refresh_stock_flats();
        self.draft = None;
        self.changed();
    }

    fn persist_library(&mut self) {
        if let Err(e) = self.library.save(&self.library_path) {
            self.notify("Couldn't save the stock library", Some(e.to_string()), true);
        }
    }

    pub fn load_outline(&mut self, path: PathBuf) {
        let bus = self.bus.clone();
        if let Some(d) = self.draft.as_mut() {
            d.file_name = path.file_name().map(|f| f.to_string_lossy().into_owned());
            d.error = None;
        }
        std::thread::spawn(move || {
            let r = nest_io::import_outline(&path).map_err(|e| e.to_string());
            bus.send(Msg::Outline(r));
        });
    }

    // ------------------------------------------------------------------ settings

    pub fn settings_changed(&mut self) {
        if let Some(v) = parse_nonneg(&self.spacing.text) {
            self.settings.spacing = v;
        }
        if let Some(v) = parse_nonneg(&self.margin.text) {
            self.settings.edge_margin = v;
        }
        if let Some(v) = parse_pos(&self.time.text) {
            self.settings.time_limit_secs = v.min(3600.0);
        }
        self.changed();
    }

    pub fn set_rotation(&mut self, r: Rotation) {
        self.settings.default_rotation = r;
        self.changed();
    }

    // ------------------------------------------------------------------ nesting

    pub fn job(&self) -> NestJob {
        NestJob {
            parts: self.parts.iter().filter(|p| p.quantity > 0).cloned().collect(),
            stock: self.library.stock.iter().filter(|s| self.use_stock.contains(&s.id)).cloned().collect(),
            settings: self.settings.clone(),
        }
    }

    pub fn can_nest(&self) -> bool {
        !self.is_running() && self.total_quantity() > 0 && !self.use_stock.is_empty()
    }

    pub fn start_nest(&mut self) {
        if !self.can_nest() {
            return;
        }
        let job = self.job();
        let ctl = Arc::new(nest_engine::Control::default());
        self.run = Some(Run { ctl: ctl.clone(), started: Instant::now() });
        self.result_job = Some(job.clone());
        self.result = None;
        self.saved_offcuts.clear();
        self.stale = false;
        self.elapsed = 0.0;
        self.fit_pending = true;
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let progress_bus = bus.clone();
            let r = nest_engine::nest(&job, &ctl, move |res: &NestResult| progress_bus.send(Msg::Progress(res.clone())));
            bus.send(Msg::Done(r.map_err(|e| e.to_string())));
        });
    }

    pub fn stop_nest(&mut self) {
        if let Some(run) = &self.run {
            run.ctl.cancel.store(true, Ordering::Relaxed);
        }
    }

    // ------------------------------------------------------------------ export

    pub fn export(&mut self, path: PathBuf) {
        let (Some(result), Some(job)) = (self.result.clone(), self.result_job.clone()) else { return };
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let r = export_sheets(&job, &result, &path).map_err(|e| e.to_string());
            bus.send(Msg::Exported(r));
        });
    }

    // ------------------------------------------------------------------ board

    /// Sheets drawn on the board, left to right: the nest result, or the stock switched on.
    pub fn board(&self) -> Vec<BoardSheet> {
        let mut out = Vec::new();
        let mut x = 0.0;
        let gap = 60.0;
        let mut push = |stock: &Stock, placements: Vec<nest_core::Placement>, util: Option<f64>, out: &mut Vec<BoardSheet>| {
            let b = stock.bbox();
            out.push(BoardSheet { stock_id: stock.id, offset: Point::new(x - b.x0, -b.y0), placements, utilization: util });
            x += b.width() + gap;
        };
        match (&self.result, &self.result_job) {
            (Some(result), Some(job)) => {
                for sheet in &result.sheets {
                    if let Some(stock) = job.stock.iter().find(|s| s.id == sheet.stock_id) {
                        push(stock, sheet.placements.clone(), Some(sheet.utilization), &mut out);
                    }
                }
            }
            _ => {
                for s in self.library.stock.iter().filter(|s| self.use_stock.contains(&s.id)) {
                    push(s, Vec::new(), None, &mut out);
                }
            }
        }
        out
    }

    pub fn fit(&mut self) {
        let [w, h] = self.canvas;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let board = self.board();
        let mut bounds: Option<kurbo::Rect> = None;
        for s in &board {
            let src = self.result_job.as_ref().and_then(|j| j.stock.iter().find(|x| x.id == s.stock_id)).or(self.stock(s.stock_id));
            if let Some(stock) = src {
                let b = stock.bbox() + s.offset.to_vec2();
                bounds = Some(bounds.map_or(b, |r| r.union(b)));
            }
        }
        let Some(b) = bounds else {
            self.cam = Camera::default();
            return;
        };
        let pad = 48.0;
        let zoom = ((w - 2.0 * pad) / b.width() as f32).min((h - 2.0 * pad) / b.height() as f32).clamp(0.01, 50.0);
        let cx = (b.x0 + b.x1) as f32 * 0.5;
        let cy = (b.y0 + b.y1) as f32 * 0.5;
        self.cam = Camera { zoom, pan: [w * 0.5 - cx * zoom, h * 0.5 + cy * zoom] };
    }

    // ------------------------------------------------------------------ frame

    /// Applies background results. True when anything changed.
    pub fn poll(&mut self, now: f64) -> bool {
        self.now = now;
        let mut changed = false;
        for msg in self.bus.drain() {
            changed = true;
            match msg {
                Msg::Imported { path, result } => self.on_imported(&path, result),
                Msg::Progress(r) => {
                    if self.run.is_some() {
                        let first = self.result.is_none();
                        self.result = Some(r);
                        if first {
                            self.fit_pending = true;
                        }
                    }
                }
                Msg::Done(r) => {
                    self.elapsed = self.run.as_ref().map(|r| r.started.elapsed().as_secs_f64()).unwrap_or(0.0);
                    self.run = None;
                    match r {
                        Ok(r) => {
                            let first = self.result.is_none();
                            self.result = Some(r);
                            if first {
                                self.fit_pending = true;
                            }
                        }
                        Err(e) => self.notify("Nesting failed", Some(e), true),
                    }
                }
                Msg::Outline(r) => {
                    if let Some(d) = self.draft.as_mut() {
                        match r {
                            Ok(path) => {
                                let b = path.bounding_box();
                                d.width = TextEdit::new(fmt_mm(b.width()));
                                d.height = TextEdit::new(fmt_mm(b.height()));
                                d.file_outline = Some(path);
                                d.error = None;
                            }
                            Err(e) => d.error = Some(e),
                        }
                    }
                }
                Msg::Exported(r) => match r {
                    Ok(files) => {
                        let detail = files.first().and_then(|f| f.parent()).map(|d| d.display().to_string());
                        let n = files.len();
                        self.notify(if n == 1 { "Exported 1 sheet".to_string() } else { format!("Exported {n} sheets") }, detail, false);
                    }
                    Err(e) => self.notify("Export failed", Some(e), true),
                },
            }
        }
        let before = self.notices.len();
        self.notices.retain(|n| n.until > now);
        changed |= before != self.notices.len();
        if self.fit_pending && self.canvas[0] > 0.0 {
            self.fit_pending = false;
            self.fit();
            changed = true;
        }
        changed
    }
}

/// Writes one file per sheet. With several sheets, `path` gets `-1`, `-2`, … before the extension.
fn export_sheets(job: &NestJob, result: &NestResult, path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("dxf").to_ascii_lowercase();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("nest").to_string();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut written = Vec::new();
    for (i, sheet) in result.sheets.iter().enumerate() {
        let file = if result.sheets.len() == 1 { path.to_path_buf() } else { dir.join(format!("{stem}-{}.{ext}", i + 1)) };
        let body = match ext.as_str() {
            "svg" => nest_io::export_svg(job, sheet),
            _ => nest_io::export_dxf(job, sheet)?,
        };
        std::fs::write(&file, body)?;
        written.push(file);
    }
    Ok(written)
}

/// Where an offcut is cut from its sheet, in sheet coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cut {
    X(f64),
    Y(f64),
}

pub struct Offcut {
    pub outline: BezPath,
    pub cut: Cut,
    pub stock_name: String,
    pub material: String,
    pub thickness: f64,
}

/// Keeps the part of `ring` where `side(p) >= 0` (Sutherland–Hodgman against one line).
fn clip_half(ring: &[Point], side: impl Fn(Point) -> f64) -> Vec<Point> {
    let mut out = Vec::new();
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let (da, db) = (side(a), side(b));
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            out.push(a.lerp(b, t));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_right_half_of_square() {
        let sq = [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0), Point::new(0.0, 10.0)];
        let r = clip_half(&sq, |p| p.x - 4.0);
        assert!((nest_core::geom::ring_area(&r).abs() - 60.0).abs() < 1e-9);
    }
}
