//! Hand edits on the board. On a nest result: select parts (click, Shift-click, or drag a box
//! on a sheet), drag them to move, the round handle to rotate, the square handles to resize.
//! Sheets and remnants rotate too, so the nest runs the way the material lies.

use kurbo::{Affine, Point, Rect, Shape as _, Vec2};
use nest_core::geom::{point_in_ring, ring_area};
use nest_core::{NestJob, NestResult, Part, PartGeometry, PartId, Placement, Stock, StockId};
use rune::{Cursor, Phase, PointerButton, TextEdit, Tx};

use crate::state::{App, Flat, fmt_mm, parse_pos};

/// Handle size and hit radius, in points.
pub const GRIP: f32 = 8.0;
const GRIP_HIT: f32 = 9.0;
/// How far the rotate handle sits past the frame's top edge, in points.
pub const ROT_GAP: f32 = 22.0;
/// Pointer travel before a press becomes a drag, in points.
const SLOP: f32 = 3.0;
const HISTORY: usize = 100;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum BoardSel {
    #[default]
    None,
    /// (sheet, placement) pairs on the result.
    Parts(Vec<(usize, usize)>),
    /// A sheet on the board, by its board index.
    Sheet(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grip {
    Rotate,
    /// A resize handle: -1, 0 or 1 along the part's own x and y.
    Scale(i8, i8),
    SheetRotate,
}

#[derive(Clone)]
pub enum Op {
    Pan { last: [f32; 2] },
    /// A press on a sheet's empty area: a selection box once it moves, a sheet click if it doesn't.
    Marquee { sheet: usize, from: Point, to: Point, base: Vec<(usize, usize)>, moved: bool },
    /// A press on the empty board: pans once it moves, clears the selection if it doesn't.
    Background { last: [f32; 2], moved: bool },
    Move { from: Point, start: Vec<Placement>, moved: bool, toggle: Option<(usize, usize)> },
    Rotate { center: Point, from: f64, start: Vec<Placement> },
    Scale { at: (usize, usize), gx: i8, gy: i8, geom: PartGeometry, pl: Placement, b: Rect },
    SheetRotate { sheet: usize, center: Point, from: f64 },
}

/// The selection's frame on the board, in world mm.
pub enum Frame {
    /// One part: its own bounding box, carried by its placement.
    Part { local: Rect, to_world: Affine },
    /// Several parts: the box around them.
    Group(Rect),
    Sheet(Rect),
}

#[derive(Clone)]
pub struct Snap {
    result: Option<NestResult>,
    job: Option<NestJob>,
    parts: Vec<Part>,
    stock: Vec<Stock>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Field {
    X,
    Y,
    W,
    H,
    Angle,
}

/// The fields in the selection bar. Typed values apply on Enter or when the field loses focus.
pub struct Bar {
    pub x: TextEdit,
    pub y: TextEdit,
    pub w: TextEdit,
    pub h: TextEdit,
    pub angle: TextEdit,
    pub lock: bool,
    pub pending: Option<Field>,
}

impl Default for Bar {
    fn default() -> Self {
        Bar { x: TextEdit::new(""), y: TextEdit::new(""), w: TextEdit::new(""), h: TextEdit::new(""), angle: TextEdit::new(""), lock: true, pending: None }
    }
}

impl Bar {
    pub fn field(&mut self, f: Field) -> &mut TextEdit {
        match f {
            Field::X => &mut self.x,
            Field::Y => &mut self.y,
            Field::W => &mut self.w,
            Field::H => &mut self.h,
            Field::Angle => &mut self.angle,
        }
    }
}

fn norm_deg(d: f64) -> f64 {
    let r = d.rem_euclid(360.0);
    if (r - 360.0).abs() < 1e-6 { 0.0 } else { r }
}

/// `pl` turned by `deg` about `c` (sheet coordinates).
fn rotated(pl: &Placement, c: Point, deg: f64) -> Placement {
    let t = Point::new(pl.translation.0, pl.translation.1);
    let r = Affine::rotate_about(deg.to_radians(), c) * t;
    Placement { rotation_deg: norm_deg(pl.rotation_deg + deg), translation: (r.x, r.y), ..pl.clone() }
}

fn bounds(pts: impl IntoIterator<Item = Point>) -> Option<Rect> {
    let mut it = pts.into_iter();
    let first = it.next()?;
    Some(it.fold(Rect::from_points(first, first), |r, p| r.union_pt(p)))
}

pub fn flat_bounds(f: &Flat) -> Rect {
    bounds(f.outer.iter().copied()).unwrap_or_default()
}

fn seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let l2 = ab.hypot2();
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).hypot()
}

fn near_ring(p: Point, ring: &[Point], tol: f64) -> bool {
    let n = ring.len();
    (0..n).any(|i| seg_dist(p, ring[i], ring[(i + 1) % n]) <= tol)
}

/// Whether local point `p` is on the part: inside its outline and outside its holes, or within
/// `tol` of any edge (thin strokes are hard to hit otherwise).
fn on_part(f: &Flat, p: Point, tol: f64) -> bool {
    let b = flat_bounds(f).inflate(tol, tol);
    if !b.contains(p) {
        return false;
    }
    let inside = point_in_ring(p, &f.outer) && !f.holes.iter().any(|h| point_in_ring(p, h));
    inside || near_ring(p, &f.outer, tol) || f.holes.iter().any(|h| near_ring(p, h, tol))
}

fn scale_geom(g: &PartGeometry, a: Affine) -> PartGeometry {
    PartGeometry {
        outer: a * g.outer.clone(),
        holes: g.holes.iter().map(|h| a * h.clone()).collect(),
        marks: g.marks.iter().map(|m| a * m.clone()).collect(),
    }
}

fn flat_area(f: &Flat) -> f64 {
    ring_area(&f.outer).abs() - f.holes.iter().map(|h| ring_area(h).abs()).sum::<f64>()
}

impl App {
    pub fn can_edit_layout(&self) -> bool {
        self.result.is_some() && self.run.is_none()
    }

    fn sheet_tf(&self, sheet: usize) -> Option<Affine> {
        self.board().get(sheet).map(|b| Affine::translate(b.offset.to_vec2()))
    }

    fn placement(&self, (si, pi): (usize, usize)) -> Option<&Placement> {
        self.result.as_ref()?.sheets.get(si)?.placements.get(pi)
    }

    fn placement_mut(&mut self, (si, pi): (usize, usize)) -> Option<&mut Placement> {
        self.result.as_mut()?.sheets.get_mut(si)?.placements.get_mut(pi)
    }

    pub fn selected_parts(&self) -> &[(usize, usize)] {
        match &self.sel {
            BoardSel::Parts(v) => v,
            _ => &[],
        }
    }

    pub fn is_selected(&self, at: (usize, usize)) -> bool {
        self.selected_parts().contains(&at)
    }

    pub fn set_sel(&mut self, sel: BoardSel) {
        self.apply_bar();
        self.sel = sel;
        for f in [Field::X, Field::Y, Field::W, Field::H, Field::Angle] {
            self.bar.field(f).focused = false;
        }
        if let BoardSel::Parts(v) = &self.sel {
            if let [at] = v.as_slice() {
                self.selected = self.placement(*at).map(|p| p.part_id);
            }
        }
        self.sync_bar();
    }

    /// The topmost part under world point `w`.
    pub fn hit_part(&self, w: Point) -> Option<(usize, usize)> {
        let res = self.result.as_ref()?;
        let tol = (4.0 / self.cam.zoom) as f64;
        let board = self.board();
        for (si, bs) in board.iter().enumerate().rev() {
            let Some(sheet) = res.sheets.get(si) else { continue };
            let t = Affine::translate(bs.offset.to_vec2());
            for (pi, pl) in sheet.placements.iter().enumerate().rev() {
                let Some(f) = self.flats.get(&pl.part_id) else { continue };
                let local = (t * pl.affine()).inverse() * w;
                if on_part(f, local, tol) {
                    return Some((si, pi));
                }
            }
        }
        None
    }

    /// The board sheet whose stock box holds world point `w`.
    pub fn hit_sheet(&self, w: Point) -> Option<usize> {
        let board = self.board();
        board.iter().enumerate().rev().find_map(|(i, bs)| {
            let stock = self.board_stock(bs.stock_id)?;
            (stock.bbox() + bs.offset.to_vec2()).contains(w).then_some(i)
        })
    }

    pub fn board_stock(&self, id: StockId) -> Option<&Stock> {
        self.result_job.as_ref().and_then(|j| j.stock.iter().find(|x| x.id == id)).or(self.stock(id))
    }

    /// World points of a placed part's outline.
    fn world_outline(&self, at: (usize, usize)) -> Vec<Point> {
        let (Some(pl), Some(t)) = (self.placement(at), self.sheet_tf(at.0)) else { return Vec::new() };
        let Some(f) = self.flats.get(&pl.part_id) else { return Vec::new() };
        let a = t * pl.affine();
        f.outer.iter().map(|p| a * *p).collect()
    }

    pub fn frame(&self) -> Option<Frame> {
        match &self.sel {
            BoardSel::None => None,
            BoardSel::Sheet(i) => {
                let bs = self.board().into_iter().nth(*i)?;
                let stock = self.board_stock(bs.stock_id)?;
                Some(Frame::Sheet(stock.bbox() + bs.offset.to_vec2()))
            }
            BoardSel::Parts(v) if v.len() == 1 => {
                let pl = self.placement(v[0])?;
                let f = self.flats.get(&pl.part_id)?;
                Some(Frame::Part { local: flat_bounds(f), to_world: self.sheet_tf(v[0].0)? * pl.affine() })
            }
            BoardSel::Parts(v) => bounds(v.iter().flat_map(|at| self.world_outline(*at))).map(Frame::Group),
        }
    }

    /// Screen positions of the frame's corners, in order around it.
    pub fn frame_corners(&self, f: &Frame) -> [[f32; 2]; 4] {
        let r = match f {
            Frame::Part { local, .. } => *local,
            Frame::Group(r) | Frame::Sheet(r) => *r,
        };
        let pts = [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x1, r.y1), Point::new(r.x0, r.y1)];
        let a = match f {
            Frame::Part { to_world, .. } => *to_world,
            _ => Affine::IDENTITY,
        };
        pts.map(|p| self.cam.to_screen(a * p))
    }

    /// Every handle of the current selection and where it sits on screen.
    pub fn grips(&self) -> Vec<(Grip, [f32; 2])> {
        let Some(f) = self.frame() else { return Vec::new() };
        let c = self.frame_corners(&f);
        let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let center = mid(c[0], c[2]);
        let top = mid(c[2], c[3]);
        let (dx, dy) = (top[0] - center[0], top[1] - center[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let rot = [top[0] + dx / len * ROT_GAP, top[1] + dy / len * ROT_GAP];
        let mut out = Vec::new();
        match f {
            Frame::Sheet(_) => {
                if self.run.is_none() {
                    out.push((Grip::SheetRotate, rot));
                }
            }
            Frame::Group(_) => out.push((Grip::Rotate, rot)),
            Frame::Part { .. } => {
                out.push((Grip::Rotate, rot));
                // Corners, then edge midpoints when the part is big enough on screen for them.
                out.extend([(Grip::Scale(-1, -1), c[0]), (Grip::Scale(1, -1), c[1]), (Grip::Scale(1, 1), c[2]), (Grip::Scale(-1, 1), c[3])]);
                let side = |a: [f32; 2], b: [f32; 2]| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
                if side(c[0], c[1]) > 48.0 {
                    out.extend([(Grip::Scale(0, -1), mid(c[0], c[1])), (Grip::Scale(0, 1), mid(c[2], c[3]))]);
                }
                if side(c[1], c[2]) > 48.0 {
                    out.extend([(Grip::Scale(1, 0), mid(c[1], c[2])), (Grip::Scale(-1, 0), mid(c[3], c[0]))]);
                }
            }
        }
        out
    }

    fn grip_at(&self, x: f32, y: f32) -> Option<Grip> {
        self.grips().into_iter().find(|(_, p)| (p[0] - x).abs() <= GRIP_HIT && (p[1] - y).abs() <= GRIP_HIT).map(|(g, _)| g)
    }

    pub fn board_cursor(&self) -> Cursor {
        match &self.op {
            Some(Op::Pan { .. } | Op::Background { moved: true, .. }) => return Cursor::Grabbing,
            Some(Op::Move { moved: true, .. }) => return Cursor::Move,
            Some(Op::Rotate { .. } | Op::SheetRotate { .. }) => return Cursor::Grabbing,
            Some(Op::Scale { .. }) => return Cursor::Crosshair,
            _ => {}
        }
        match self.hover {
            Some(Grip::Scale(..)) => Cursor::Crosshair,
            Some(Grip::Rotate | Grip::SheetRotate) => Cursor::Grab,
            None if self.hover_part => Cursor::Move,
            None => Cursor::Default,
        }
    }

    pub fn board_pointer(&mut self, x: f32, y: f32) {
        if self.op.is_some() {
            return;
        }
        self.hover = self.grip_at(x, y);
        self.hover_part = self.hover.is_none() && self.can_edit_layout() && self.hit_part(self.cam.to_world(x, y)).is_some();
    }

    // ------------------------------------------------------------------ history

    fn snap(&self) -> Snap {
        Snap { result: self.result.clone(), job: self.result_job.clone(), parts: self.parts.clone(), stock: self.library.stock.clone() }
    }

    /// Records the state before an edit, for undo.
    pub fn checkpoint_board(&mut self) {
        let s = self.snap();
        self.history.push(s);
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        self.future.clear();
    }

    pub fn undo_board(&mut self) {
        if let Some(s) = self.history.pop() {
            let now = self.snap();
            self.future.push(now);
            self.restore(s);
        }
    }

    pub fn redo_board(&mut self) {
        if let Some(s) = self.future.pop() {
            let now = self.snap();
            self.history.push(now);
            self.restore(s);
        }
    }

    fn restore(&mut self, s: Snap) {
        let stock_changed = s.stock != self.library.stock;
        self.result = s.result;
        self.result_job = s.job;
        self.parts = s.parts;
        self.flats = self.parts.iter().map(|p| (p.id, Flat::of_part(p))).collect();
        if stock_changed {
            self.library.stock = s.stock;
            self.after_stock_change();
        }
        let n = self.result.as_ref().map_or(0, |r| r.sheets.len());
        let keep = match &self.sel {
            BoardSel::Parts(v) => {
                let v: Vec<_> = v.iter().copied().filter(|at| self.placement(*at).is_some()).collect();
                if v.is_empty() { BoardSel::None } else { BoardSel::Parts(v) }
            }
            BoardSel::Sheet(i) if *i < n.max(self.board().len()) => BoardSel::Sheet(*i),
            _ => BoardSel::None,
        };
        self.sel = keep;
        self.after_layout_edit();
    }

    /// Refreshes everything that depends on placements: utilization, conflicts, the bar.
    pub fn after_layout_edit(&mut self) {
        if let (Some(res), Some(job)) = (self.result.as_mut(), self.result_job.as_ref()) {
            for sh in &mut res.sheets {
                let Some(stock) = job.stock.iter().find(|s| s.id == sh.stock_id) else { continue };
                let usable = Flat::of_stock(stock);
                let area = flat_area(&usable).max(1e-9);
                let placed: f64 = sh.placements.iter().filter_map(|p| self.flats.get(&p.part_id)).map(flat_area).sum();
                sh.utilization = placed / area;
            }
        }
        self.refresh_conflicts();
        self.sync_bar();
    }

    pub fn refresh_conflicts(&mut self) {
        self.conflicts = match (&self.result, &self.result_job) {
            (Some(r), Some(j)) if self.run.is_none() => nest_engine::validate::conflicts(j, r),
            _ => Vec::new(),
        };
    }

    pub fn conflict_count(&self) -> usize {
        self.conflicts.iter().map(Vec::len).sum()
    }

    pub fn has_conflict(&self, (si, pi): (usize, usize)) -> bool {
        self.conflicts.get(si).is_some_and(|v| v.contains(&pi))
    }

    // ------------------------------------------------------------------ edits

    /// Moves the selected parts by `d` mm.
    pub fn nudge_parts(&mut self, d: Vec2) {
        let sel = self.selected_parts().to_vec();
        if sel.is_empty() {
            return;
        }
        self.checkpoint_board();
        for at in sel {
            if let Some(p) = self.placement_mut(at) {
                p.translation.0 += d.x;
                p.translation.1 += d.y;
            }
        }
        self.after_layout_edit();
    }

    /// The world box around the selected parts.
    fn sel_bounds(&self) -> Option<Rect> {
        bounds(self.selected_parts().iter().flat_map(|at| self.world_outline(*at)))
    }

    /// Turns the selected parts by `deg` (counterclockwise) about the middle of the selection.
    pub fn rotate_parts(&mut self, deg: f64) {
        let sel = self.selected_parts().to_vec();
        let Some(b) = self.sel_bounds() else { return };
        self.checkpoint_board();
        let c = b.center();
        for at in sel {
            let Some(t) = self.sheet_tf(at.0) else { continue };
            let local_c = t.inverse() * c;
            if let Some(p) = self.placement_mut(at) {
                *p = rotated(p, local_c, deg);
            }
        }
        self.after_layout_edit();
    }

    /// Scales part `id` (every copy) by `sx`, `sy` about its own bounding-box corner. The copy
    /// at `keep`, if given, stays put at local point `anchor`.
    fn scale_part(&mut self, id: PartId, from: &PartGeometry, sx: f64, sy: f64, keep: Option<((usize, usize), Placement, Point)>) {
        let b = from.outer.bounding_box();
        let a = Affine::translate(b.origin().to_vec2()) * Affine::scale_non_uniform(sx, sy) * Affine::translate(-b.origin().to_vec2());
        let geom = scale_geom(from, a);
        for p in self.parts.iter_mut().filter(|p| p.id == id) {
            p.geometry = geom.clone();
        }
        if let Some(job) = self.result_job.as_mut() {
            for p in job.parts.iter_mut().filter(|p| p.id == id) {
                p.geometry = geom.clone();
            }
        }
        let part = self.parts.iter().find(|p| p.id == id).or_else(|| self.result_job.as_ref().and_then(|j| j.parts.iter().find(|p| p.id == id)));
        if let Some(part) = part {
            let f = Flat::of_part(part);
            self.flats.insert(id, f);
        }
        if let Some((at, pl, anchor)) = keep {
            let moved = a * anchor;
            let shift = pl.affine() * anchor - pl.affine() * moved;
            if let Some(p) = self.placement_mut(at) {
                p.translation = (pl.translation.0 + shift.x, pl.translation.1 + shift.y);
            }
        }
        self.changed_parts();
    }

    fn changed_parts(&mut self) {
        // Part sizes feed the next nest; this layout stays as it is.
        if self.result.is_some() {
            self.stale = true;
        }
    }

    /// Sets the size of the selected part (or the part picked in the list), every copy of it.
    fn resize_selected(&mut self, w: Option<f64>, h: Option<f64>) {
        let (id, at) = match self.selected_parts() {
            [at] => match self.placement(*at) {
                Some(p) => (p.part_id, Some(*at)),
                None => return,
            },
            [] => match self.selected {
                Some(id) => (id, None),
                None => return,
            },
            _ => return,
        };
        let Some(part) = self.part(id).or_else(|| self.result_job.as_ref().and_then(|j| j.parts.iter().find(|p| p.id == id))) else { return };
        let from = part.geometry.clone();
        let b = from.outer.bounding_box();
        let (bw, bh) = (b.width().max(1e-9), b.height().max(1e-9));
        let (mut sx, mut sy) = (w.map_or(1.0, |w| w / bw), h.map_or(1.0, |h| h / bh));
        if self.bar.lock {
            if w.is_some() {
                sy = sx;
            } else {
                sx = sy;
            }
        }
        if (sx - 1.0).abs() < 1e-9 && (sy - 1.0).abs() < 1e-9 {
            return;
        }
        self.checkpoint_board();
        let keep = at.and_then(|at| self.placement(at).cloned().map(|pl| (at, pl, b.center())));
        self.scale_part(id, &from, sx, sy, keep);
        self.after_layout_edit();
    }

    /// Turns stock `id` by `deg` and puts it back at the origin. On a result, the parts on that
    /// stock turn with it, so the layout stays valid.
    pub fn rotate_stock(&mut self, id: StockId, deg: f64) {
        let Some(stock) = self.board_stock(id) else { return };
        let r = Affine::rotate(deg.to_radians());
        let b = (r * stock.outline.clone()).bounding_box();
        let t = Affine::translate(-b.origin().to_vec2()) * r;
        let turn = |s: &mut Stock| {
            s.outline = t * s.outline.clone();
            s.defects = s.defects.iter().map(|d| t * d.clone()).collect();
        };
        if let Some(s) = self.library.stock.iter_mut().find(|s| s.id == id) {
            turn(s);
        }
        if let Some(job) = self.result_job.as_mut() {
            if let Some(s) = job.stock.iter_mut().find(|s| s.id == id) {
                turn(s);
            }
        }
        if let Some(res) = self.result.as_mut() {
            for sh in res.sheets.iter_mut().filter(|sh| sh.stock_id == id) {
                for p in &mut sh.placements {
                    let q = t * Point::new(p.translation.0, p.translation.1);
                    p.translation = (q.x, q.y);
                    p.rotation_deg = norm_deg(p.rotation_deg + deg);
                }
            }
        }
        self.persist_library();
        self.refresh_stock_flats();
        self.after_layout_edit();
    }

    pub fn rotate_selected_sheet(&mut self, deg: f64) {
        let BoardSel::Sheet(i) = self.sel else { return };
        let Some(id) = self.board().get(i).map(|b| b.stock_id) else { return };
        self.checkpoint_board();
        self.rotate_stock(id, deg);
    }

    /// R and Shift+R: a quarter turn of whatever is selected.
    pub fn quarter_turn(&mut self, ccw: bool) {
        let deg = if ccw { 90.0 } else { -90.0 };
        match self.sel {
            BoardSel::Parts(_) => self.rotate_parts(deg),
            BoardSel::Sheet(_) if self.run.is_none() => self.rotate_selected_sheet(deg),
            _ => {}
        }
    }

    pub fn select_all_parts(&mut self) {
        let Some(res) = &self.result else { return };
        let all: Vec<_> = res.sheets.iter().enumerate().flat_map(|(si, s)| (0..s.placements.len()).map(move |pi| (si, pi))).collect();
        self.set_sel(if all.is_empty() { BoardSel::None } else { BoardSel::Parts(all) });
    }

    // ------------------------------------------------------------------ the bar

    /// Writes the selection's position, size and angle into every field not being typed in.
    pub fn sync_bar(&mut self) {
        let mut vals: [Option<f64>; 5] = [None; 5];
        let one = match self.selected_parts() {
            [at] => Some(*at),
            _ => None,
        };
        if let (Some(b), Some(si)) = (self.sel_bounds(), self.selected_parts().first().map(|a| a.0)) {
            if let Some(t) = self.sheet_tf(si) {
                let o = t.inverse() * b.origin();
                vals[0] = Some(o.x);
                vals[1] = Some(o.y);
            }
        }
        let size_of = |id: PartId| self.flats.get(&id).map(flat_bounds);
        if let Some(at) = one {
            if let Some(pl) = self.placement(at) {
                if let Some(b) = size_of(pl.part_id) {
                    vals[2] = Some(b.width());
                    vals[3] = Some(b.height());
                }
                vals[4] = Some(norm_deg(pl.rotation_deg));
            }
        } else if self.sel == BoardSel::None {
            if let Some(b) = self.selected.and_then(size_of) {
                vals[2] = Some(b.width());
                vals[3] = Some(b.height());
            }
        }
        for (f, v) in [Field::X, Field::Y, Field::W, Field::H, Field::Angle].into_iter().zip(vals) {
            if self.bar.pending == Some(f) {
                continue;
            }
            let e = self.bar.field(f);
            if !e.focused {
                e.replace_text(v.map(|v| if f == Field::Angle { format!("{}", (v * 10.0).round() / 10.0) } else { fmt_mm(v) }).unwrap_or_default());
            }
        }
    }

    pub fn apply_bar(&mut self) {
        let Some(f) = self.bar.pending.take() else { return };
        let num = |t: &TextEdit| t.text.trim().trim_end_matches('°').replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite());
        match f {
            Field::X | Field::Y => {
                let (Some(b), Some(si)) = (self.sel_bounds(), self.selected_parts().first().map(|a| a.0)) else { return };
                let Some(t) = self.sheet_tf(si) else { return };
                let o = t.inverse() * b.origin();
                let v = if f == Field::X { num(&self.bar.x) } else { num(&self.bar.y) };
                if let Some(v) = v {
                    let d = if f == Field::X { Vec2::new(v - o.x, 0.0) } else { Vec2::new(0.0, v - o.y) };
                    if d.hypot() > 1e-9 {
                        self.nudge_parts(d);
                    }
                }
            }
            Field::W => {
                if let Some(v) = parse_pos(&self.bar.w.text) {
                    self.resize_selected(Some(v), None);
                }
            }
            Field::H => {
                if let Some(v) = parse_pos(&self.bar.h.text) {
                    self.resize_selected(None, Some(v));
                }
            }
            Field::Angle => {
                let cur = match self.selected_parts() {
                    [at] => self.placement(*at).map(|p| p.rotation_deg),
                    _ => None,
                };
                if let (Some(cur), Some(v)) = (cur, num(&self.bar.angle)) {
                    let d = norm_deg(v - cur);
                    if d.abs() > 1e-9 {
                        self.rotate_parts(d);
                    }
                }
            }
        }
        self.sync_bar();
    }

    /// Applies a typed value once its field has lost focus.
    pub fn apply_bar_if_left(&mut self) -> bool {
        let Some(f) = self.bar.pending else { return false };
        if self.bar.field(f).focused {
            return false;
        }
        self.apply_bar();
        true
    }

    // ------------------------------------------------------------------ pointer

    pub fn board_drag(&mut self, phase: Phase, x: f32, y: f32, travel: f32, tx: &Tx) {
        let w = self.cam.to_world(x, y);
        match phase {
            Phase::Down => self.press(x, y, w, tx),
            Phase::Move | Phase::Up => {
                if let Some(op) = self.op.take() {
                    let op = self.drag_to(op, x, y, w, travel, tx);
                    if phase == Phase::Up {
                        self.release(op);
                        self.hover = self.grip_at(x, y);
                    } else {
                        self.op = Some(op);
                    }
                }
            }
        }
    }

    fn press(&mut self, x: f32, y: f32, w: Point, tx: &Tx) {
        self.apply_bar();
        if tx.button() == PointerButton::Middle {
            self.op = Some(Op::Pan { last: [x, y] });
            return;
        }
        let shift = tx.mods().shift;
        let edit = self.can_edit_layout();
        if let Some(g) = self.grip_at(x, y) {
            match (g, &self.sel) {
                (Grip::SheetRotate, BoardSel::Sheet(i)) => {
                    let i = *i;
                    if let Some(Frame::Sheet(r)) = self.frame() {
                        let c = r.center();
                        self.op = Some(Op::SheetRotate { sheet: i, center: c, from: (w - c).atan2() });
                        self.spin = Some((i, 0.0));
                        return;
                    }
                }
                (Grip::Rotate, BoardSel::Parts(v)) if edit => {
                    let v = v.clone();
                    if let Some(b) = self.sel_bounds() {
                        self.checkpoint_board();
                        let c = b.center();
                        let start = v.iter().filter_map(|at| self.placement(*at).cloned()).collect();
                        self.op = Some(Op::Rotate { center: c, from: (w - c).atan2(), start });
                        return;
                    }
                }
                (Grip::Scale(gx, gy), BoardSel::Parts(v)) if edit && v.len() == 1 => {
                    let at = v[0];
                    if let Some(pl) = self.placement(at).cloned() {
                        let geom = self.part(pl.part_id).or_else(|| self.result_job.as_ref().and_then(|j| j.parts.iter().find(|p| p.id == pl.part_id))).map(|p| p.geometry.clone());
                        if let (Some(geom), Some(f)) = (geom, self.flats.get(&pl.part_id)) {
                            let b = flat_bounds(f);
                            self.checkpoint_board();
                            self.op = Some(Op::Scale { at, gx, gy, geom, pl, b });
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        if edit {
            if let Some(at) = self.hit_part(w) {
                let mut toggle = None;
                if shift {
                    if self.is_selected(at) {
                        toggle = Some(at);
                    } else {
                        let mut v = self.selected_parts().to_vec();
                        v.push(at);
                        self.set_sel(BoardSel::Parts(v));
                    }
                } else if !self.is_selected(at) {
                    self.set_sel(BoardSel::Parts(vec![at]));
                }
                self.checkpoint_board();
                let start = self.selected_parts().iter().filter_map(|a| self.placement(*a).cloned()).collect();
                self.op = Some(Op::Move { from: w, start, moved: false, toggle });
                return;
            }
        }
        if let Some(si) = self.hit_sheet(w) {
            let base = if shift { self.selected_parts().to_vec() } else { Vec::new() };
            self.op = Some(Op::Marquee { sheet: si, from: w, to: w, base, moved: false });
            return;
        }
        self.op = Some(Op::Background { last: [x, y], moved: false });
    }

    fn drag_to(&mut self, op: Op, x: f32, y: f32, w: Point, travel: f32, tx: &Tx) -> Op {
        let snap15 = tx.mods().shift;
        match op {
            Op::Pan { last } => {
                self.cam.pan[0] += x - last[0];
                self.cam.pan[1] += y - last[1];
                Op::Pan { last: [x, y] }
            }
            Op::Background { last, moved } => {
                let moved = moved || travel > SLOP;
                if moved {
                    self.cam.pan[0] += x - last[0];
                    self.cam.pan[1] += y - last[1];
                }
                Op::Background { last: [x, y], moved }
            }
            Op::Marquee { sheet, from, base, moved, .. } => {
                let moved = (moved || travel > SLOP) && self.can_edit_layout();
                if moved {
                    let r = Rect::from_points(from, w);
                    let mut v = base.clone();
                    if let Some(res) = &self.result {
                        for (si, sh) in res.sheets.iter().enumerate() {
                            for pi in 0..sh.placements.len() {
                                let inside = bounds(self.world_outline((si, pi))).is_some_and(|b| r.contains(b.origin()) && r.contains(Point::new(b.x1, b.y1)));
                                if inside && !v.contains(&(si, pi)) {
                                    v.push((si, pi));
                                }
                            }
                        }
                    }
                    self.sel = if v.is_empty() { BoardSel::None } else { BoardSel::Parts(v) };
                }
                Op::Marquee { sheet, from, to: w, base, moved }
            }
            Op::Move { from, start, moved, toggle } => {
                let moved = moved || travel > SLOP;
                if moved {
                    let d = w - from;
                    let sel = self.selected_parts().to_vec();
                    for (at, s) in sel.iter().zip(&start) {
                        if let Some(p) = self.placement_mut(*at) {
                            p.translation = (s.translation.0 + d.x, s.translation.1 + d.y);
                        }
                    }
                }
                Op::Move { from, start, moved, toggle }
            }
            Op::Rotate { center, from, start } => {
                let mut deg = ((w - center).atan2() - from).to_degrees();
                if snap15 {
                    deg = (deg / 15.0).round() * 15.0;
                }
                let sel = self.selected_parts().to_vec();
                for (at, s) in sel.iter().zip(&start) {
                    let Some(t) = self.sheet_tf(at.0) else { continue };
                    let c = t.inverse() * center;
                    if let Some(p) = self.placement_mut(*at) {
                        *p = rotated(s, c, deg);
                    }
                }
                self.sync_bar();
                Op::Rotate { center, from, start }
            }
            Op::Scale { at, gx, gy, geom, pl, b } => {
                let Some(t) = self.sheet_tf(at.0) else { return Op::Scale { at, gx, gy, geom, pl, b } };
                let p = (t * pl.affine()).inverse() * w;
                let ax = if gx > 0 { b.x0 } else if gx < 0 { b.x1 } else { b.center().x };
                let ay = if gy > 0 { b.y0 } else if gy < 0 { b.y1 } else { b.center().y };
                let anchor = Point::new(ax, ay);
                let (bw, bh) = (b.width().max(1e-9), b.height().max(1e-9));
                let min = 1.0;
                let fx = ((p.x - ax) * gx as f64).max(min) / bw;
                let fy = ((p.y - ay) * gy as f64).max(min) / bh;
                let (sx, sy) = match (gx != 0, gy != 0, self.bar.lock) {
                    (true, true, true) => {
                        let corner = Vec2::new(bw * gx as f64, bh * gy as f64);
                        let s = ((p - anchor).dot(corner) / corner.hypot2()).max(min / bw.min(bh));
                        (s, s)
                    }
                    (true, true, false) => (fx, fy),
                    (true, false, lock) => (fx, if lock { fx } else { 1.0 }),
                    (false, true, lock) => (if lock { fy } else { 1.0 }, fy),
                    (false, false, _) => (1.0, 1.0),
                };
                // Keep the anchor in the part's frame before scaling: scale_part scales about
                // the geometry's own corner, so map the anchor through the same change.
                self.scale_part(pl.part_id, &geom, sx, sy, Some((at, pl.clone(), anchor)));
                self.sync_bar();
                Op::Scale { at, gx, gy, geom, pl, b }
            }
            Op::SheetRotate { sheet, center, from } => {
                let mut deg = ((w - center).atan2() - from).to_degrees();
                if snap15 {
                    deg = (deg / 15.0).round() * 15.0;
                }
                self.spin = Some((sheet, deg));
                Op::SheetRotate { sheet, center, from }
            }
        }
    }

    fn release(&mut self, op: Op) {
        match op {
            Op::Pan { .. } => {}
            Op::Background { moved, .. } => {
                if !moved {
                    self.set_sel(BoardSel::None);
                    self.selected = None;
                }
            }
            Op::Marquee { sheet, moved, .. } => {
                if !moved {
                    self.set_sel(BoardSel::Sheet(sheet));
                } else {
                    let sel = self.sel.clone();
                    self.set_sel(sel);
                }
            }
            Op::Move { moved, toggle, .. } => {
                if !moved {
                    // A click: nothing moved, so drop the checkpoint.
                    self.history.pop();
                    if let Some(at) = toggle {
                        let v: Vec<_> = self.selected_parts().iter().copied().filter(|a| *a != at).collect();
                        self.set_sel(if v.is_empty() { BoardSel::None } else { BoardSel::Parts(v) });
                    }
                    return;
                }
                self.drop_on_sheets();
                self.after_layout_edit();
            }
            Op::Rotate { .. } | Op::Scale { .. } => self.after_layout_edit(),
            Op::SheetRotate { sheet, .. } => {
                let deg = self.spin.take().map_or(0.0, |s| s.1);
                if deg.abs() > 0.05 {
                    self.sel = BoardSel::Sheet(sheet);
                    self.rotate_selected_sheet(deg);
                }
            }
        }
    }

    /// Parts dropped over another sheet move onto it.
    fn drop_on_sheets(&mut self) {
        let sel = self.selected_parts().to_vec();
        let board = self.board();
        let mut moves = Vec::new();
        for at in &sel {
            let Some(c) = bounds(self.world_outline(*at)).map(|b| b.center()) else { continue };
            match self.hit_sheet(c) {
                Some(to) if to != at.0 && to < self.result.as_ref().map_or(0, |r| r.sheets.len()) => moves.push((*at, to)),
                _ => {}
            }
        }
        if moves.is_empty() {
            return;
        }
        let Some(res) = self.result.as_mut() else { return };
        // Take the moving placements out, highest index first so the rest keep theirs.
        let mut taken = Vec::new();
        let mut order = moves.clone();
        order.sort_by(|a, b| b.0.cmp(&a.0));
        for ((si, pi), to) in order {
            let mut p = res.sheets[si].placements.remove(pi);
            let d = board[si].offset - board[to].offset;
            p.translation = (p.translation.0 + d.x, p.translation.1 + d.y);
            taken.push((p, to));
        }
        // Indices of what stayed shift down past each removal.
        let removed: Vec<(usize, usize)> = moves.iter().map(|m| m.0).collect();
        let mut new_sel: Vec<(usize, usize)> = sel
            .iter()
            .filter(|at| !removed.contains(at))
            .map(|&(si, pi)| (si, pi - removed.iter().filter(|r| r.0 == si && r.1 < pi).count()))
            .collect();
        for (p, to) in taken {
            res.sheets[to].placements.push(p);
            new_sel.push((to, res.sheets[to].placements.len() - 1));
        }
        self.sel = BoardSel::Parts(new_sel);
    }
}
