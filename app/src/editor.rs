//! The stock editor: a sheet or remnant outline edited on the main canvas. Drag corners and
//! edges, add corners from edge midpoints, type exact edge lengths and corner positions, and
//! mark damaged areas the nester must avoid.

use kurbo::{Point, Shape as _, Vec2};
use rune::{Color, Cursor, Cx, El, LineCap, Phase, Shape, TextEdit, Tx, canvas, col, row, spacer, text};
use rune_kit::{Button, ButtonVariant, IconButton, IconButtonVariant, Kit, SegmentedControl, TextInput};
use rune_theme_opencode::icons as oc;

use crate::state::{App, Camera, fmt_mm, parse_nonneg, parse_pos};
use nest_core::geom::{flatten_ring, polygon_path, ring_area};
use nest_core::{Stock, StockId, StockKind};

const HANDLE: f32 = 9.0;
const HIT_VERTEX: f32 = 9.0;
const HIT_MID: f32 = 8.0;
const HIT_EDGE: f32 = 6.0;
const ALIGN_PT: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sel {
    None,
    Vertex(usize),
    Edge(usize),
    Damage(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Vertex(usize),
    Mid(usize),
    Edge(usize),
    Damage(usize),
    Empty,
}

/// A typed value waiting for Enter or for the field to lose focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pending {
    Edge(usize),
    Vertex(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tool {
    Shape,
    Damage,
}

#[derive(Clone, Debug)]
enum DragOp {
    Vertex(usize),
    Edge { i: usize, a: Point, b: Point, from: Point },
    Damage { i: usize, start: Vec<Point>, from: Point },
    NewDamage { from: Point },
    Pan { last: [f32; 2] },
}

#[derive(Clone, PartialEq)]
struct Snapshot {
    pts: Vec<Point>,
    damage: Vec<Vec<Point>>,
}

pub struct Editor {
    pub editing: Option<StockId>,
    pub kind: StockKind,
    pub name: TextEdit,
    pub material: TextEdit,
    pub thickness: TextEdit,
    pub quantity: TextEdit,
    pub rect_w: TextEdit,
    pub rect_h: TextEdit,
    pub vx: TextEdit,
    pub vy: TextEdit,
    pub edges: Vec<TextEdit>,
    /// Outline corners in mm, y-up, counter-clockwise.
    pub pts: Vec<Point>,
    pub damage: Vec<Vec<Point>>,
    pub sel: Sel,
    pub hover: Hit,
    pub tool: Tool,
    pub cursor: Option<Point>,
    pub cam: Camera,
    pub fit_pending: bool,
    pub error: Option<String>,
    pub pending: Option<Pending>,
    drag: Option<DragOp>,
    /// Rectangle being drawn with the damage tool.
    pub new_damage: Option<(Point, Point)>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

fn rect_pts(w: f64, h: f64) -> Vec<Point> {
    vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)]
}

/// Removes repeated and collinear corners.
fn tidy(pts: &mut Vec<Point>) {
    let mut i = 0;
    while pts.len() > 3 && i < pts.len() {
        let n = pts.len();
        let a = pts[(i + n - 1) % n];
        let b = pts[i];
        let c = pts[(i + 1) % n];
        let cross = (b - a).cross(c - b);
        if b.distance(a) < 1e-6 || (cross.abs() < 1e-6 * (b - a).hypot().max(1.0) * (c - b).hypot().max(1.0) && (b - a).dot(c - b) > 0.0) {
            pts.remove(i);
        } else {
            i += 1;
        }
    }
}

fn segments_cross(a: Point, b: Point, c: Point, d: Point) -> bool {
    let o = |p: Point, q: Point, r: Point| (q - p).cross(r - p);
    let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) && d1 != 0.0 && d2 != 0.0 && d3 != 0.0 && d4 != 0.0
}

fn self_intersects(pts: &[Point]) -> bool {
    let n = pts.len();
    for i in 0..n {
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            if segments_cross(pts[i], pts[(i + 1) % n], pts[j], pts[(j + 1) % n]) {
                return true;
            }
        }
    }
    false
}

/// Edge letters: A–Z, then AA, AB, …
pub fn edge_name(i: usize) -> String {
    let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if i < 26 { (letters[i] as char).to_string() } else { format!("{}{}", letters[i / 26 - 1] as char, letters[i % 26] as char) }
}

impl Editor {
    pub fn new(kind: StockKind) -> Editor {
        let (w, h) = if kind == StockKind::Sheet { (1200.0, 900.0) } else { (600.0, 400.0) };
        let mut e = Editor {
            editing: None,
            kind,
            name: TextEdit::new(""),
            material: TextEdit::new(""),
            thickness: TextEdit::new(""),
            quantity: TextEdit::new("1"),
            rect_w: TextEdit::new(fmt_mm(w)),
            rect_h: TextEdit::new(fmt_mm(h)),
            vx: TextEdit::new(""),
            vy: TextEdit::new(""),
            edges: Vec::new(),
            pts: rect_pts(w, h),
            damage: Vec::new(),
            sel: Sel::None,
            hover: Hit::Empty,
            tool: Tool::Shape,
            cursor: None,
            cam: Camera::default(),
            fit_pending: true,
            error: None,
            pending: None,
            drag: None,
            new_damage: None,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        e.sync_fields();
        e
    }

    pub fn from_stock(s: &Stock) -> Editor {
        let mut e = Editor::new(s.kind);
        e.editing = Some(s.id);
        e.name.replace_text(s.name.clone());
        e.material.replace_text(s.material.clone());
        e.thickness.replace_text(if s.thickness > 0.0 { fmt_mm(s.thickness) } else { String::new() });
        e.quantity.replace_text(s.quantity.to_string());
        e.set_outline_from_path(&s.outline);
        e.damage = s.defects.iter().map(|d| simplify(&flatten_ring(d, 0.2), 0.2)).filter(|d| d.len() > 2).collect();
        e.sync_fields();
        e
    }

    pub fn set_outline_from_path(&mut self, path: &kurbo::BezPath) {
        let mut pts = simplify(&flatten_ring(path, 0.2), 0.2);
        if ring_area(&pts) < 0.0 {
            pts.reverse();
        }
        tidy(&mut pts);
        if pts.len() >= 3 {
            self.pts = pts;
        }
        let b = polygon_path(&self.pts).bounding_box();
        self.rect_w.replace_text(fmt_mm(b.width()));
        self.rect_h.replace_text(fmt_mm(b.height()));
        self.sel = Sel::None;
        self.fit_pending = true;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { pts: self.pts.clone(), damage: self.damage.clone() }
    }

    /// Records the current shape so the next change can be undone.
    pub fn checkpoint(&mut self) {
        let s = self.snapshot();
        if self.undo.last() != Some(&s) {
            self.undo.push(s);
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        self.undo.iter().any(|s| *s != self.snapshot())
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        let now = self.snapshot();
        while let Some(s) = self.undo.pop() {
            if s != now {
                self.redo.push(now);
                self.restore(s);
                return;
            }
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.pts = s.pts;
        self.damage = s.damage;
        self.sel = Sel::None;
        self.sync_fields();
    }

    pub fn bbox(&self) -> kurbo::Rect {
        polygon_path(&self.pts).bounding_box()
    }

    pub fn area(&self) -> f64 {
        ring_area(&self.pts).abs()
    }

    /// Grid step in mm: the smallest round number at least 8 pt on screen.
    pub fn snap_step(&self) -> f64 {
        let min_mm = 8.0 / self.cam.zoom as f64;
        [0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0].into_iter().find(|s| *s >= min_mm).unwrap_or(500.0)
    }

    fn snap(&self, v: f64) -> f64 {
        let s = self.snap_step();
        (v / s).round() * s
    }

    /// Writes the geometry into every field the user isn't typing in.
    pub fn sync_fields(&mut self) {
        let n = self.pts.len();
        if self.edges.len() != n {
            self.edges = (0..n).map(|_| TextEdit::new("")).collect();
        }
        for i in 0..n {
            let len = self.pts[i].distance(self.pts[(i + 1) % n]);
            if !self.edges[i].focused {
                self.edges[i].replace_text(fmt_mm(len));
            }
        }
        if let Sel::Vertex(i) = self.sel {
            if let Some(p) = self.pts.get(i).copied() {
                if !self.vx.focused {
                    self.vx.replace_text(fmt_mm(p.x));
                }
                if !self.vy.focused {
                    self.vy.replace_text(fmt_mm(p.y));
                }
            }
        }
    }

    pub fn select(&mut self, sel: Sel) {
        self.apply_pending();
        self.sel = sel;
        self.vx.focused = false;
        self.vy.focused = false;
        self.sync_fields();
    }

    /// Sets edge `i` to `len` mm by moving its end corner along the edge. When the next edge
    /// runs square to this one, it moves along too, so square shapes stay square.
    pub fn set_edge_length(&mut self, i: usize, len: f64) {
        let n = self.pts.len();
        if i >= n || len <= 0.0 {
            return;
        }
        let a = self.pts[i];
        let j = (i + 1) % n;
        let b = self.pts[j];
        let d = b - a;
        if d.hypot() < 1e-9 {
            return;
        }
        let dir = d / d.hypot();
        let delta = a + dir * len - b;
        let k = (j + 1) % n;
        let next = self.pts[k] - b;
        let square = next.hypot() > 1e-9 && (dir.dot(next / next.hypot())).abs() < 0.02;
        self.pts[j] = b + delta;
        if square && k != i {
            self.pts[k] += delta;
        }
        self.sync_fields();
    }

    pub fn set_vertex(&mut self, i: usize, p: Point) {
        if let Some(v) = self.pts.get_mut(i) {
            *v = p;
        }
        self.sync_fields();
    }

    /// Applies a typed edge length or corner position.
    pub fn apply_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        match p {
            Pending::Edge(i) => {
                if let Some(v) = self.edges.get(i).and_then(|t| parse_pos(&t.text)) {
                    self.checkpoint();
                    self.set_edge_length(i, v);
                }
            }
            Pending::Vertex(i) => {
                let num = |t: &TextEdit| t.text.trim().replace(',', ".").parse::<f64>().ok();
                if let (Some(x), Some(y)) = (num(&self.vx), num(&self.vy)) {
                    self.checkpoint();
                    self.set_vertex(i, Point::new(x, y));
                }
            }
        }
        self.sync_fields();
    }

    /// Applies a pending value once its field has lost focus.
    pub fn apply_if_left(&mut self) -> bool {
        let left = match self.pending {
            Some(Pending::Edge(i)) => self.edges.get(i).is_none_or(|t| !t.focused),
            Some(Pending::Vertex(_)) => !self.vx.focused && !self.vy.focused,
            None => false,
        };
        if left {
            self.apply_pending();
        }
        left
    }

    pub fn delete_selection(&mut self) {
        match self.sel {
            Sel::Vertex(i) if self.pts.len() > 3 => {
                self.checkpoint();
                self.pts.remove(i);
                self.sel = Sel::None;
            }
            Sel::Damage(i) if i < self.damage.len() => {
                self.checkpoint();
                self.damage.remove(i);
                self.sel = Sel::None;
            }
            _ => return,
        }
        self.sync_fields();
    }

    pub fn nudge(&mut self, dx: f64, dy: f64) {
        let d = Vec2::new(dx, dy);
        match self.sel {
            Sel::Vertex(i) if i < self.pts.len() => {
                self.checkpoint();
                self.pts[i] += d;
            }
            Sel::Edge(i) if i < self.pts.len() => {
                self.checkpoint();
                let j = (i + 1) % self.pts.len();
                self.pts[i] += d;
                self.pts[j] += d;
            }
            Sel::Damage(i) if i < self.damage.len() => {
                self.checkpoint();
                for p in &mut self.damage[i] {
                    *p += d;
                }
            }
            _ => return,
        }
        self.sync_fields();
    }

    pub fn reset_rect(&mut self) {
        let (Some(w), Some(h)) = (parse_pos(&self.rect_w.text), parse_pos(&self.rect_h.text)) else {
            self.error = Some("Enter a width and height in mm.".into());
            return;
        };
        self.checkpoint();
        self.pts = rect_pts(w, h);
        self.damage.retain(|d| d.iter().all(|p| p.x <= w && p.y <= h));
        self.sel = Sel::None;
        self.error = None;
        self.fit_pending = true;
        self.sync_fields();
    }

    // ---------------------------------------------------------------- canvas input

    fn hit(&self, x: f32, y: f32) -> Hit {
        let cam = self.cam;
        let at = [x, y];
        let dist = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let scr: Vec<[f32; 2]> = self.pts.iter().map(|p| cam.to_screen(*p)).collect();
        if self.tool == Tool::Shape {
            for (i, s) in scr.iter().enumerate() {
                if dist(*s, at) <= HIT_VERTEX {
                    return Hit::Vertex(i);
                }
            }
            let n = scr.len();
            for i in 0..n {
                let (a, b) = (scr[i], scr[(i + 1) % n]);
                let m = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                if dist(a, b) > 40.0 && dist(m, at) <= HIT_MID {
                    return Hit::Mid(i);
                }
            }
            for i in 0..n {
                let (a, b) = (scr[i], scr[(i + 1) % n]);
                let ab = [b[0] - a[0], b[1] - a[1]];
                let len2 = ab[0] * ab[0] + ab[1] * ab[1];
                if len2 < 1e-6 {
                    continue;
                }
                let t = (((at[0] - a[0]) * ab[0] + (at[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0);
                let q = [a[0] + ab[0] * t, a[1] + ab[1] * t];
                if dist(q, at) <= HIT_EDGE {
                    return Hit::Edge(i);
                }
            }
        }
        let w = cam.to_world(x, y);
        for (i, d) in self.damage.iter().enumerate().rev() {
            if nest_core::geom::point_in_ring(w, d) {
                return Hit::Damage(i);
            }
        }
        Hit::Empty
    }

    /// Snaps a dragged corner to the grid, then lines it up with its neighbours when close.
    fn place_vertex(&self, i: usize, w: Point) -> Point {
        let mut p = Point::new(self.snap(w.x), self.snap(w.y));
        let n = self.pts.len();
        let tol = (ALIGN_PT / self.cam.zoom) as f64;
        for j in [(i + n - 1) % n, (i + 1) % n] {
            let q = self.pts[j];
            if (w.x - q.x).abs() < tol {
                p.x = q.x;
            }
            if (w.y - q.y).abs() < tol {
                p.y = q.y;
            }
        }
        p
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.cursor = Some(self.cam.to_world(x, y));
        if self.drag.is_none() {
            self.hover = self.hit(x, y);
        }
    }

    pub fn drag(&mut self, phase: Phase, x: f32, y: f32) {
        let w = self.cam.to_world(x, y);
        self.cursor = Some(w);
        match phase {
            Phase::Down => {
                let hit = self.hit(x, y);
                self.drag = Some(match (self.tool, hit) {
                    (_, Hit::Vertex(i)) => {
                        self.checkpoint();
                        self.select(Sel::Vertex(i));
                        DragOp::Vertex(i)
                    }
                    (_, Hit::Mid(i)) => {
                        self.checkpoint();
                        let n = self.pts.len();
                        let m = self.pts[i].midpoint(self.pts[(i + 1) % n]);
                        self.pts.insert(i + 1, m);
                        self.select(Sel::Vertex(i + 1));
                        DragOp::Vertex(i + 1)
                    }
                    (_, Hit::Edge(i)) => {
                        self.checkpoint();
                        self.select(Sel::Edge(i));
                        let n = self.pts.len();
                        DragOp::Edge { i, a: self.pts[i], b: self.pts[(i + 1) % n], from: w }
                    }
                    (_, Hit::Damage(i)) => {
                        self.checkpoint();
                        self.select(Sel::Damage(i));
                        DragOp::Damage { i, start: self.damage[i].clone(), from: w }
                    }
                    (Tool::Damage, Hit::Empty) => {
                        let p = Point::new(self.snap(w.x), self.snap(w.y));
                        self.new_damage = Some((p, p));
                        DragOp::NewDamage { from: p }
                    }
                    (Tool::Shape, Hit::Empty) => {
                        self.select(Sel::None);
                        DragOp::Pan { last: [x, y] }
                    }
                });
            }
            Phase::Up | Phase::Move => {
                let Some(op) = self.drag.clone() else { return };
                match op {
                    DragOp::Vertex(i) => {
                        let p = self.place_vertex(i, w);
                        self.pts[i] = p;
                    }
                    DragOp::Edge { i, a, b, from } => {
                        let d = b - a;
                        let len = d.hypot().max(1e-9);
                        let normal = Vec2::new(-d.y, d.x) / len;
                        let along = self.snap((w - from).dot(normal));
                        let n = self.pts.len();
                        self.pts[i] = a + normal * along;
                        self.pts[(i + 1) % n] = b + normal * along;
                    }
                    DragOp::Damage { i, start, from } => {
                        let d = Vec2::new(self.snap(w.x - from.x), self.snap(w.y - from.y));
                        self.damage[i] = start.iter().map(|p| *p + d).collect();
                    }
                    DragOp::NewDamage { from } => {
                        self.new_damage = Some((from, Point::new(self.snap(w.x), self.snap(w.y))));
                    }
                    DragOp::Pan { last } => {
                        self.cam.pan[0] += x - last[0];
                        self.cam.pan[1] += y - last[1];
                        self.drag = Some(DragOp::Pan { last: [x, y] });
                    }
                }
                if phase == Phase::Up {
                    if let Some((a, b)) = self.new_damage.take() {
                        let r = kurbo::Rect::from_points(a, b);
                        if r.width() >= 1.0 && r.height() >= 1.0 {
                            self.checkpoint();
                            self.damage.push(rect_pts(r.width(), r.height()).into_iter().map(|p| p + Vec2::new(r.x0, r.y0)).collect());
                            self.sel = Sel::Damage(self.damage.len() - 1);
                        }
                    }
                    if matches!(self.drag, Some(DragOp::Vertex(_)) | Some(DragOp::Edge { .. })) {
                        let keep = self.sel;
                        let before = self.pts.len();
                        tidy(&mut self.pts);
                        if self.pts.len() != before {
                            self.sel = Sel::None;
                        } else {
                            self.sel = keep;
                        }
                    }
                    self.drag = None;
                    self.hover = self.hit(x, y);
                }
                self.sync_fields();
            }
        }
    }

    pub fn cursor_icon(&self) -> Cursor {
        if let Some(op) = &self.drag {
            return match op {
                DragOp::NewDamage { .. } => Cursor::Crosshair,
                _ => Cursor::Grabbing,
            };
        }
        match (self.tool, self.hover) {
            (_, Hit::Vertex(_) | Hit::Edge(_) | Hit::Damage(_)) => Cursor::Grab,
            (_, Hit::Mid(_)) => Cursor::Pointer,
            (Tool::Damage, Hit::Empty) => Cursor::Crosshair,
            _ => Cursor::Default,
        }
    }

    pub fn fit(&mut self, size: [f32; 2]) {
        let [w, h] = size;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let b = self.bbox();
        let pad = 72.0;
        let zoom = ((w - 2.0 * pad) / b.width().max(1.0) as f32).min((h - 2.0 * pad) / b.height().max(1.0) as f32).clamp(0.01, 50.0);
        let cx = (b.x0 + b.x1) as f32 * 0.5;
        let cy = (b.y0 + b.y1) as f32 * 0.5;
        self.cam = Camera { zoom, pan: [w * 0.5 - cx * zoom, h * 0.5 + cy * zoom] };
    }

    /// The stock this editor describes, or what needs fixing first.
    pub fn build(&self, id: StockId) -> Result<Stock, String> {
        if self.pts.len() < 3 || self.area() < 1.0 {
            return Err("The outline needs at least 3 corners.".into());
        }
        if self_intersects(&self.pts) {
            return Err("The outline crosses itself. Move a corner to fix it.".into());
        }
        let Some(quantity) = self.quantity.text.trim().parse::<u32>().ok().filter(|q| *q > 0) else {
            return Err("Enter how many you have.".into());
        };
        let b = self.bbox();
        let shift = Vec2::new(-b.x0, -b.y0);
        let outline = polygon_path(&self.pts.iter().map(|p| *p + shift).collect::<Vec<_>>());
        let defects = self.damage.iter().map(|d| polygon_path(&d.iter().map(|p| *p + shift).collect::<Vec<_>>())).collect();
        let kind_name = if self.kind == StockKind::Remnant { "Remnant" } else { "Sheet" };
        let name = match self.name.text.trim() {
            "" => format!("{kind_name} {} × {}", fmt_mm(b.width()), fmt_mm(b.height())),
            n => n.to_string(),
        };
        Ok(Stock {
            id,
            name,
            kind: self.kind,
            outline,
            defects,
            quantity,
            material: self.material.text.trim().to_string(),
            thickness: parse_nonneg(&self.thickness.text).unwrap_or(0.0),
        })
    }
}

/// Douglas–Peucker on a closed ring.
fn simplify(ring: &[Point], tol: f64) -> Vec<Point> {
    if ring.len() < 5 {
        return ring.to_vec();
    }
    fn dp(pts: &[Point], tol: f64, out: &mut Vec<Point>) {
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        let ab = b - a;
        let len = ab.hypot();
        let (mut best, mut idx) = (0.0, 0);
        for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
            let d = if len < 1e-12 { p.distance(a) } else { ab.cross(*p - a).abs() / len };
            if d > best {
                best = d;
                idx = i;
            }
        }
        if best > tol {
            dp(&pts[..=idx], tol, out);
            out.pop();
            dp(&pts[idx..], tol, out);
        } else {
            out.push(a);
            out.push(b);
        }
    }
    // Split at the point farthest from the first so both halves are open polylines.
    let far = (1..ring.len()).max_by(|&i, &j| ring[0].distance(ring[i]).total_cmp(&ring[0].distance(ring[j]))).unwrap_or(1);
    let mut first: Vec<Point> = ring[..=far].to_vec();
    let mut second: Vec<Point> = ring[far..].to_vec();
    second.push(ring[0]);
    let mut out = Vec::new();
    dp(&first, tol, &mut out);
    out.pop();
    let mut tail = Vec::new();
    dp(&second, tol, &mut tail);
    tail.pop();
    out.extend(tail);
    first.clear();
    second.clear();
    out
}

// -------------------------------------------------------------------- app actions

impl App {
    pub fn open_editor(&mut self, id: Option<StockId>) {
        self.editor = Some(match id.and_then(|id| self.stock(id)) {
            Some(s) => Editor::from_stock(s),
            None => Editor::new(if self.library.stock.is_empty() { StockKind::Sheet } else { StockKind::Remnant }),
        });
    }

    pub fn save_editor(&mut self) {
        let Some(ed) = self.editor.as_mut() else { return };
        let id = ed.editing.unwrap_or_else(|| self.library.next_id());
        let stock = match ed.build(id) {
            Ok(s) => s,
            Err(e) => {
                ed.error = Some(e);
                return;
            }
        };
        match self.library.stock.iter_mut().find(|s| s.id == id) {
            Some(existing) => *existing = stock,
            None => {
                self.library.stock.push(stock);
                self.use_stock.push(id);
            }
        }
        self.editor = None;
        self.after_stock_change();
    }

    pub fn delete_edited_stock(&mut self) {
        if let Some(id) = self.editor.as_ref().and_then(|e| e.editing) {
            self.delete_stock(id);
        }
        self.editor = None;
    }

    pub fn editor_import(&mut self, path: std::path::PathBuf) {
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let r = nest_io::import_outline(&path).map_err(|e| e.to_string());
            bus.send(crate::state::Msg::Outline(r));
        });
    }
}

// -------------------------------------------------------------------- view

fn ed(s: &mut App) -> &mut Editor {
    s.editor.as_mut().expect("editor is open")
}

fn pill(k: &Kit, label: String, selected: bool, x: f32, y: f32) -> El {
    let (bg, fg) = if selected { (k.c.border_focus, Color::WHITE) } else { (k.c.surface_raised, k.c.text_base) };
    text(label)
        .font(k.ty.small_medium.features("'tnum' 1"))
        .color(fg)
        .single_line()
        .px(5.0)
        .py(1.0)
        .radius(4.0)
        .fill(bg)
        .border(1.0, if selected { bg } else { k.c.border_base })
        .absolute(x, y)
}

pub fn canvas_view(s: &App, e: &Editor, k: &Kit, _cx: &mut Cx<App>) -> El {
    let c = crate::board::colors(k);
    let cam = e.cam;
    let [cw, ch] = s.canvas;
    let accent = k.c.border_focus;
    let mut shape = Shape::new();

    // Grid: minor lines at the snap step when they're far enough apart, major every 10 steps.
    let step = e.snap_step().max(1.0);
    let minor = if step * (cam.zoom as f64) >= 10.0 { step } else { step * 10.0 };
    let major = minor * 10.0;
    let tl = cam.to_world(0.0, 0.0);
    let br = cam.to_world(cw, ch);
    let grid_minor = k.alpha(k.c.text_base, 0.05);
    let grid_major = k.alpha(k.c.text_base, 0.10);
    let mut x = (tl.x / minor).floor() * minor;
    while x <= br.x {
        let col = if (x / major).round() * major == x { grid_major } else { grid_minor };
        let sx = cam.to_screen(Point::new(x, 0.0))[0];
        shape = shape.line([sx, 0.0], [sx, ch], 1.0, LineCap::Butt, col);
        x += minor;
    }
    let mut y = (br.y / minor).floor() * minor;
    while y <= tl.y {
        let col = if (y / major).round() * major == y { grid_major } else { grid_minor };
        let sy = cam.to_screen(Point::new(0.0, y))[1];
        shape = shape.line([0.0, sy], [cw, sy], 1.0, LineCap::Butt, col);
        y += minor;
    }

    let scr: Vec<[f32; 2]> = e.pts.iter().map(|p| cam.to_screen(*p)).collect();
    let n = scr.len();
    if n >= 3 {
        shape = shape.fill(scr.clone(), c.sheet).stroke_with(scr.clone(), true, 1.5, LineCap::Round, k.c.text_base);
    }
    for (i, d) in e.damage.iter().enumerate() {
        let ds: Vec<[f32; 2]> = d.iter().map(|p| cam.to_screen(*p)).collect();
        let sel = e.sel == Sel::Damage(i);
        shape = shape
            .fill(ds.clone(), c.defect)
            .stroke_with(ds, true, if sel { 2.0 } else { 1.0 }, LineCap::Round, if sel { accent } else { k.c.danger.fg });
    }
    if let Some((a, b)) = e.new_damage {
        let r = kurbo::Rect::from_points(a, b);
        let pts = [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x1, r.y1), Point::new(r.x0, r.y1)];
        let ds: Vec<[f32; 2]> = pts.iter().map(|p| cam.to_screen(*p)).collect();
        shape = shape.fill(ds.clone(), c.defect).stroke_with(ds, true, 1.0, LineCap::Round, k.c.danger.fg);
    }

    // Selected or hovered edge.
    let edge_hl = match (e.sel, e.hover) {
        (Sel::Edge(i), _) => Some((i, accent, 3.0)),
        (_, Hit::Edge(i)) => Some((i, k.alpha(accent, 0.6), 3.0)),
        _ => None,
    };
    if let Some((i, col, w)) = edge_hl {
        if i < n {
            shape = shape.line(scr[i], scr[(i + 1) % n], w, LineCap::Round, col);
        }
    }

    // Midpoint handles for adding corners.
    if e.tool == Tool::Shape {
        for i in 0..n {
            let (a, b) = (scr[i], scr[(i + 1) % n]);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            if len <= 40.0 {
                continue;
            }
            let m = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
            let hot = e.hover == Hit::Mid(i);
            let r = if hot { 7.0 } else { 5.5 };
            shape = shape.fill(circle(m, r), if hot { accent } else { c.sheet }).stroke_with(circle(m, r), true, 1.0, LineCap::Round, if hot { accent } else { k.c.text_muted });
            let ink = if hot { Color::WHITE } else { k.c.text_muted };
            shape = shape.line([m[0] - 3.0, m[1]], [m[0] + 3.0, m[1]], 1.2, LineCap::Round, ink).line([m[0], m[1] - 3.0], [m[0], m[1] + 3.0], 1.2, LineCap::Round, ink);
        }
    }

    // Corner handles.
    for (i, p) in scr.iter().enumerate() {
        let sel = e.sel == Sel::Vertex(i);
        let hot = e.hover == Hit::Vertex(i);
        let h = if sel || hot { HANDLE + 2.0 } else { HANDLE } * 0.5;
        let sq = vec![[p[0] - h, p[1] - h], [p[0] + h, p[1] - h], [p[0] + h, p[1] + h], [p[0] - h, p[1] + h]];
        shape = shape.fill(sq.clone(), if sel { accent } else { c.sheet }).stroke_with(sq, true, 1.2, LineCap::Butt, if sel || hot { accent } else { k.c.text_base });
    }

    // Edge length labels outside each edge.
    let ccw = ring_area(&e.pts) >= 0.0;
    let mut labels = Vec::new();
    for i in 0..n {
        let a = e.pts[i];
        let b = e.pts[(i + 1) % n];
        let d = b - a;
        let len = d.hypot();
        let (sa, sb) = (scr[i], scr[(i + 1) % n]);
        if ((sb[0] - sa[0]).powi(2) + (sb[1] - sa[1]).powi(2)).sqrt() < 28.0 {
            continue;
        }
        let out = if ccw { Vec2::new(d.y, -d.x) } else { Vec2::new(-d.y, d.x) } / len.max(1e-9);
        let m = cam.to_screen(a.midpoint(b));
        let label = format!("{}  {}", edge_name(i), fmt_mm(len));
        let (pw, ph) = (8.0 + label.chars().count() as f32 * 6.2, 18.0);
        // Sit the pill just outside the edge: push it out by half its own size along the normal.
        let (nx, ny) = (out.x as f32, -(out.y as f32));
        let gap = 8.0;
        let cx = m[0] + nx * (gap + pw * 0.5 * nx.abs());
        let cy = m[1] + ny * (gap + ph * 0.5 * ny.abs());
        labels.push(pill(k, label, e.sel == Sel::Edge(i), cx - pw * 0.5, cy - ph * 0.5));
    }

    canvas(shape)
        .key("editor-canvas")
        .w_full()
        .h_full()
        .fill(c.board)
        .cursor(e.cursor_icon())
        .on_layout(|s: &mut App, r: [f32; 4]| s.canvas = [r[2], r[3]])
        .on_pointer_move_local(|s: &mut App, p, _tx| ed(s).pointer_move(p.x, p.y))
        .on_hover_change(|s: &mut App, inside, _tx| {
            if !inside {
                let e = ed(s);
                e.cursor = None;
                e.hover = Hit::Empty;
            }
        })
        .on_drag(|s: &mut App, d, _tx| ed(s).drag(d.phase, d.x, d.y))
        .on_wheel(|s: &mut App, w, _tx| {
            let e = ed(s);
            if w.precise && !(w.mods.cmd || w.mods.ctrl) {
                e.cam.pan[0] += w.dx;
                e.cam.pan[1] += w.dy;
            } else {
                e.cam.zoom_about(w.x, w.y, (1.0 + w.dy * 0.01).clamp(0.5, 2.0));
            }
            true
        })
        .on_magnify(|s: &mut App, m, _tx| {
            ed(s).cam.zoom_about(m.x, m.y, 1.0 + m.delta);
            true
        })
        .children(labels)
}

fn circle(c: [f32; 2], r: f32) -> Vec<[f32; 2]> {
    (0..16).map(|i| {
        let t = i as f32 / 16.0 * std::f32::consts::TAU;
        [c[0] + r * t.cos(), c[1] + r * t.sin()]
    }).collect()
}

fn section(k: &Kit, title: &str) -> El {
    row().items_center().h(32.0).px(12.0).mt(4.0).child(text(title).font(k.ty.ui_medium).color(k.c.text_strong))
}

fn labeled(k: &Kit, label: &str, control: El) -> El {
    row().items_center().h(34.0).px(12.0).gap(8.0).child(text(label).font(k.ty.ui).color(k.c.text_base)).child(spacer()).child(control)
}

fn mm_input(k: &Kit, key: String, edit: &TextEdit, width: f32, suffix: &str, get: impl for<'a> Fn(&'a mut App) -> &'a mut TextEdit + 'static, change: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    let mut r = row().items_center().gap(6.0).child(
        TextInput::new(edit)
            .width(Some(width))
            .height(26.0)
            .text_style(k.ty.ui_compact.features("'tnum' 1"))
            .on_edit(get)
            .on_change(change)
            .on_submit(|s: &mut App, _tx| {
                if let Some(e) = s.editor.as_mut() {
                    e.apply_pending();
                }
            })
            .build(k)
            .key(key),
    );
    if !suffix.is_empty() {
        r = r.child(text(suffix.to_string()).font(k.ty.ui).color(k.c.text_muted).w(22.0));
    }
    r
}

fn text_field(k: &Kit, key: &str, edit: &TextEdit, placeholder: &str, get: fn(&mut App) -> &mut TextEdit) -> El {
    TextInput::new(edit)
        .width(Some(176.0))
        .height(26.0)
        .placeholder(placeholder)
        .text_style(k.ty.ui_compact)
        .on_edit(get)
        .on_change(|s: &mut App, _tx| ed(s).error = None)
        .build(k)
        .key(key.to_string())
}

pub fn panel(e: &Editor, k: &Kit) -> El {
    let kind = SegmentedControl::new(&["Sheet", "Remnant"], Some(if e.kind == StockKind::Remnant { 1 } else { 0 }))
        .width(Some(176.0))
        .on_pick(|s: &mut App, i, _tx| ed(s).kind = if i == 1 { StockKind::Remnant } else { StockKind::Sheet })
        .build(k);
    let b = e.bbox();
    let auto_name = format!("{} {} × {}", if e.kind == StockKind::Remnant { "Remnant" } else { "Sheet" }, fmt_mm(b.width()), fmt_mm(b.height()));

    let mut body = col()
        .w_full()
        .pt(6.0)
        .pb(12.0)
        .child(section(k, "Details"))
        .child(labeled(k, "Type", kind))
        .child(labeled(k, "Name", text_field(k, "ed-name", &e.name, &auto_name, |s| &mut ed(s).name)))
        .child(labeled(k, "Material", text_field(k, "ed-material", &e.material, "", |s| &mut ed(s).material)))
        .child(labeled(k, "Thickness", mm_input(k, "ed-thick".into(), &e.thickness, 64.0, "mm", |s| &mut ed(s).thickness, |_s: &mut App, _tx| {})))
        .child(labeled(k, "Quantity", mm_input(k, "ed-qty".into(), &e.quantity, 64.0, "", |s| &mut ed(s).quantity, |s: &mut App, _tx| ed(s).error = None)));

    // Rectangle reset and import.
    let rect = row()
        .items_center()
        .gap(6.0)
        .px(12.0)
        .h(36.0)
        .child(mm_input(k, "ed-rw".into(), &e.rect_w, 64.0, "", |s| &mut ed(s).rect_w, |_s: &mut App, _tx| {}))
        .child(text("×").font(k.ty.ui).color(k.c.text_muted))
        .child(mm_input(k, "ed-rh".into(), &e.rect_h, 64.0, "mm", |s| &mut ed(s).rect_h, |_s: &mut App, _tx| {}))
        .child(spacer())
        .child(Button::new("Set").variant(ButtonVariant::Neutral).control_size(rune_kit::ControlSize::Regular).on_press(|s: &mut App| ed(s).reset_rect()).build(k));
    let import = Button::new("Import outline")
        .variant(ButtonVariant::Ghost)
        .control_size(rune_kit::ControlSize::Regular)
        .on_press_tx(|_s: &mut App, tx: &mut Tx| {
            let opts = rune::FileDialogOptions { title: "Import outline".into(), ..Default::default() }.filter("Drawings", &["dxf", "svg", "ai", "pdf"]);
            tx.open_file(opts, |s: &mut App, r, _tx| {
                if let Ok(Some(path)) = r {
                    s.editor_import(path);
                }
            });
        })
        .build(k);
    body = body
        .child(row().items_center().h(32.0).px(12.0).mt(4.0).child(text("Rectangle").font(k.ty.ui_medium).color(k.c.text_strong)).child(spacer()).child(import))
        .child(rect);

    // Edges with typed lengths.
    body = body.child(section(k, "Edges"));
    let n = e.pts.len();
    for i in 0..n {
        let sel = e.sel == Sel::Edge(i);
        let field = mm_input(
            k,
            format!("ed-edge-{i}"),
            &e.edges[i],
            84.0,
            "mm",
            move |s| &mut ed(s).edges[i],
            move |s: &mut App, _tx| {
                let e = ed(s);
                e.sel = Sel::Edge(i);
                e.pending = Some(Pending::Edge(i));
            },
        );
        body = body.child(
            row()
                .key(format!("ed-edge-row-{i}"))
                .items_center()
                .h(34.0)
                .mx(6.0)
                .px(6.0)
                .radius(k.radius.md)
                .fill(if sel { k.c.surface_hover } else { Color::TRANSPARENT })
                .hover(k.c.hover)
                .on_press(move |s: &mut App| ed(s).select(Sel::Edge(i)))
                .child(text(edge_name(i)).font(k.ty.ui_medium).color(if sel { k.c.text_strong } else { k.c.text_base }).w(28.0))
                .child(spacer())
                .child(field),
        );
    }

    // Selected corner.
    if let Sel::Vertex(i) = e.sel {
        let apply = move |s: &mut App, _tx: &mut Tx| ed(s).pending = Some(Pending::Vertex(i));
        body = body
            .child(row().items_center().h(32.0).px(12.0).mt(4.0).child(text("Corner").font(k.ty.ui_medium).color(k.c.text_strong)).child(spacer()).child(
                Button::new("Delete")
                    .variant(ButtonVariant::Ghost)
                    .control_size(rune_kit::ControlSize::Regular)
                    .disabled(n <= 3)
                    .on_press(|s: &mut App| ed(s).delete_selection())
                    .build(k),
            ))
            .child(labeled(k, "X", mm_input(k, "ed-vx".into(), &e.vx, 84.0, "mm", |s| &mut ed(s).vx, apply)))
            .child(labeled(k, "Y", mm_input(k, "ed-vy".into(), &e.vy, 84.0, "mm", |s| &mut ed(s).vy, apply)));
    }

    // Damaged areas.
    let marking = e.tool == Tool::Damage;
    body = body.child(
        row().items_center().h(32.0).px(12.0).mt(4.0).child(text("Damaged areas").font(k.ty.ui_medium).color(k.c.text_strong)).child(spacer()).child(
            Button::new(if marking { "Done" } else { "Mark area" })
                .variant(if marking { ButtonVariant::Neutral } else { ButtonVariant::Ghost })
                .control_size(rune_kit::ControlSize::Regular)
                .on_press(|s: &mut App| {
                    let e = ed(s);
                    e.tool = if e.tool == Tool::Damage { Tool::Shape } else { Tool::Damage };
                    e.hover = Hit::Empty;
                })
                .build(k),
        ),
    );
    if e.damage.is_empty() {
        body = body.child(text(if marking { "Drag on the canvas to mark an area." } else { "None" }).font(k.ty.ui).color(k.c.text_faint).px(12.0).py(4.0));
    }
    for (i, d) in e.damage.iter().enumerate() {
        let r = polygon_path(d).bounding_box();
        let sel = e.sel == Sel::Damage(i);
        body = body.child(
            row()
                .key(format!("ed-dmg-{i}"))
                .items_center()
                .h(32.0)
                .mx(6.0)
                .px(6.0)
                .radius(k.radius.md)
                .fill(if sel { k.c.surface_hover } else { Color::TRANSPARENT })
                .hover(k.c.hover)
                .on_press(move |s: &mut App| ed(s).select(Sel::Damage(i)))
                .child(text(format!("{} × {} mm", fmt_mm(r.width()), fmt_mm(r.height()))).font(k.ty.ui.features("'tnum' 1")).color(k.c.text_base))
                .child(spacer())
                .child(
                    IconButton::new(&oc::CLOSE)
                        .variant(IconButtonVariant::GhostMuted)
                        .label("Remove")
                        .on_press(move |s: &mut App| {
                            let e = ed(s);
                            e.sel = Sel::Damage(i);
                            e.delete_selection();
                        })
                        .build(k),
                ),
        );
    }

    if e.editing.is_some() {
        body = body.child(
            row().px(12.0).mt(16.0).child(
                Button::new("Delete this stock").variant(ButtonVariant::Danger).control_size(rune_kit::ControlSize::Regular).on_press(|s: &mut App| s.delete_edited_stock()).build(k),
            ),
        );
    }

    let scroller = col().key("editor-scroll").w_full().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec { stick_bottom: false }).child(body);
    col()
        .key("editor-panel")
        .w(crate::sidebar::WIDTH)
        .h_full()
        .min_h(0.0)
        .fill(k.c.bg_base)
        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_weak)
        .child(k.scroll_bar(scroller))
}

pub fn toolbar_items(e: &Editor, k: &Kit) -> (El, El) {
    let title = match (e.editing.is_some(), e.kind) {
        (true, StockKind::Remnant) => "Edit remnant",
        (true, StockKind::Sheet) => "Edit sheet",
        (false, StockKind::Remnant) => "New remnant",
        (false, StockKind::Sheet) => "New sheet",
    };
    let left = row()
        .items_center()
        .gap(4.0)
        .child(text(title).font(k.ty.ui_medium).color(k.c.text_strong).mr(8.0))
        .child(IconButton::new(&crate::icons::UNDO).variant(IconButtonVariant::GhostMuted).label("Undo").help("Undo").disabled(!e.can_undo()).on_press(|s: &mut App| ed(s).undo()).build(k))
        .child(
            IconButton::new(&crate::icons::REDO)
                .variant(IconButtonVariant::GhostMuted)
                .label("Redo")
                .help("Redo")
                .disabled(!e.can_redo())
                .on_press(|s: &mut App| ed(s).redo())
                .build(k),
        );
    let mut right = row().items_center().gap(8.0);
    if let Some(err) = &e.error {
        right = right.child(text(err.clone()).font(k.ty.ui).color(k.c.danger.fg).single_line());
    }
    right = right
        .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.editor = None).build(k))
        .child(Button::new("Save").variant(ButtonVariant::Submit).on_press(|s: &mut App| s.save_editor()).build(k));
    (left, right)
}

pub fn status(e: &Editor, k: &Kit) -> El {
    let num = k.ty.ui.features("'tnum' 1");
    let b = e.bbox();
    let mut r = row()
        .items_center()
        .gap(16.0)
        .child(text(format!("{} × {} mm", fmt_mm(b.width()), fmt_mm(b.height()))).font(num).color(k.c.text_base))
        .child(text(format!("{:.3} m²", e.area() / 1e6)).font(num).color(k.c.text_muted))
        .child(text(format!("Snap {} mm", fmt_mm(e.snap_step()))).font(num).color(k.c.text_muted));
    if let Some(p) = e.cursor {
        r = r.child(text(format!("x {}  y {}", fmt_mm(p.x), fmt_mm(p.y))).font(num).color(k.c.text_muted));
    }
    r
}


