//! A small desktop inside the application: draggable, resizable, snapping windows with thin title bars
//! (built-in views and the pluggable tools). Windows snap to each other and to the edges of the area,
//! dock to halves / quarters when released against an edge, and neighbours that sit flush against an
//! edge follow when it is resized (hold Alt to resize one window only, Ctrl to move without snapping).

use crate::app::*;
use crate::tools::{self, TOOL_COUNT};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Stroke, Ui};
use std::time::Instant;

/// Built-in views first, then one window per tool (see `tools::INFO`).
pub const BUILTIN: usize = 8;
pub const WIN_COUNT: usize = BUILTIN + TOOL_COUNT;
const SHORT: [&str; BUILTIN] = ["Spectrum", "Spectro", "Stereo", "3D", "Scope", "Measure", "Generator", "Cursors"];
const OPEN: [bool; BUILTIN] = [true, true, true, true, true, true, false, false];
/// Tiling priority of the built-in windows (visual ones first).
/// Smallest share a window gets, so utility windows stay usable.
const MIN_WEIGHT: f32 = 1.2;
const WEIGHT: [f32; BUILTIN] = [3.0, 3.0, 1.8, 2.2, 2.6, 1.0, 0.9, 0.7];

/// Height of a window's title bar (kept small, like a thin desktop window frame).
pub const TITLE_H: f32 = 17.0;
/// Space between tiled / snapped windows.
pub const GAP: f32 = 5.0;
/// Distance (px) at which window edges snap to each other.
const SNAP: f32 = 10.0;
const MIN_W: f32 = 200.0;
const MIN_H: f32 = 110.0;
const WIN_FILL: Color32 = Color32::from_rgb(22, 26, 33);

#[derive(Clone, Copy, PartialEq)]
pub enum DragKind {
    Move,
    Resize { l: bool, r: bool, t: bool, b: bool },
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// A neighbour whose edge sits against the edge being resized and moves with it.
#[derive(Clone)]
struct Link {
    j: usize,
    my: Edge,
    theirs: Edge,
    off: f32,
    orig: Rect,
}

#[derive(Clone)]
pub struct WinDrag {
    i: usize,
    kind: DragKind,
    start: Rect,
    ptr0: Pos2,
    links: Vec<Link>,
}

#[derive(Default)]
struct WinEv {
    close: bool,
    max: bool,
    collapse: bool,
    pop: bool,
    tile: bool,
    raise: bool,
    drag: Option<DragKind>,
}

pub fn win_short(i: usize) -> &'static str {
    if i < BUILTIN { SHORT[i] } else { tools::INFO[i - BUILTIN].short }
}

pub fn win_weight(i: usize) -> f32 {
    if i < BUILTIN { WEIGHT[i] } else { tools::INFO[i - BUILTIN].weight }
}

#[derive(Clone, Copy)]
pub struct WinState {
    pub open: bool,
    /// outer rect as last seen on screen
    pub rect: Option<Rect>,
    /// rect to return to after "maximize" / docking
    pub prev: Option<Rect>,
    /// shown as its title bar only
    pub collapsed: bool,
}

impl WinState {
    pub fn new(i: usize) -> Self {
        Self { open: i < BUILTIN && OPEN[i], rect: None, prev: None, collapsed: false }
    }
}

/// Gapless treemap: the weights are dealt into two groups of similar total (largest first, always to
/// the lighter group), so big and small windows end up mixed all over the area; each group gets a part of
/// the rect proportional to its weight, cut along the longer side. Covers `r` completely (minus `gap`).
fn split_rects(items: &[(usize, f32)], r: Rect, gap: f32, out: &mut Vec<(usize, Rect)>) {
    match items.len() {
        0 => {}
        1 => out.push((items[0].0, r)),
        _ => {
            let mut v = items.to_vec();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            let (mut a, mut b): (Vec<(usize, f32)>, Vec<(usize, f32)>) = (Vec::new(), Vec::new());
            let (mut wa, mut wb) = (0.0f32, 0.0f32);
            for it in v {
                if wa <= wb {
                    wa += it.1;
                    a.push(it);
                } else {
                    wb += it.1;
                    b.push(it);
                }
            }
            let f = wa / (wa + wb);
            let (ra, rb) = if r.width() >= r.height() {
                let w = (r.width() - gap) * f;
                (Rect::from_min_size(r.min, vec2(w, r.height())), Rect::from_min_max(pos2(r.min.x + w + gap, r.min.y), r.max))
            } else {
                let h = (r.height() - gap) * f;
                (Rect::from_min_size(r.min, vec2(r.width(), h)), Rect::from_min_max(pos2(r.min.x, r.min.y + h + gap), r.max))
            };
            split_rects(&a, ra, gap, out);
            split_rects(&b, rb, gap, out);
        }
    }
}

/// Windows that are laid out as a wide strip when tiling (the timeline reads left to right).
pub fn wants_wide(i: usize) -> bool {
    i >= BUILTIN && tools::INFO[i - BUILTIN].short == "Timeline"
}

/// The arrangement auto tiling produces. Wide windows (the timeline) get a full-width strip at the bottom,
/// wider than tall whenever the area allows it; the rest is a weighted treemap, so the heaviest window
/// (the routing graph) gets the most room. Gaps of `gap` between windows.
pub fn tile_rects(items: &[(usize, f32)], area: Rect, gap: f32) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    let inner = area.shrink(gap);
    let (wide, rest): (Vec<(usize, f32)>, Vec<(usize, f32)>) = items.iter().copied().partition(|(i, _)| wants_wide(*i));
    if wide.is_empty() || rest.is_empty() {
        split_rects(items, inner, gap, &mut out);
        return out;
    }
    let (ww, wr): (f32, f32) = (wide.iter().map(|w| w.1).sum(), rest.iter().map(|w| w.1).sum());
    // the strip takes its share of the height but stays between 28 % and 55 %, and never taller than wide
    let h = (inner.height() * ww / (ww + wr)).clamp(inner.height() * 0.28, inner.height() * 0.55).min(inner.width() / 1.6).max(40.0_f32.min(inner.height() * 0.5));
    let strip = Rect::from_min_max(pos2(inner.min.x, inner.max.y - h), inner.max);
    let upper = Rect::from_min_max(inner.min, pos2(inner.max.x, inner.max.y - h - gap));
    split_rects(&rest, upper, gap, &mut out);
    split_rects(&wide, strip, gap, &mut out);
    out
}

impl App {
    pub fn win_title(&self, i: usize) -> &'static str {
        match i {
            0 => "Spectrum",
            1 => {
                if self.rotated {
                    "Waterfall"
                } else {
                    "Spectrogram"
                }
            }
            2 => "Stereo field",
            3 => "3D spectrum",
            4 => "Oscilloscope",
            5 => "Measurements",
            6 => "Signal generator",
            7 => "Cursors & snapshots",
            _ => tools::INFO[i - BUILTIN].title,
        }
    }

    /// Gapless tiling weighted by importance: visual windows get the most room, utility windows get
    /// less but are spread out wherever they fit (no side column).
    pub fn tile_windows(&mut self, area: Rect) {
        let items: Vec<(usize, f32)> = (0..WIN_COUNT).filter(|&i| self.wins[i].open && !self.popped[i]).map(|i| (i, win_weight(i).max(MIN_WEIGHT))).collect();
        if items.is_empty() {
            return;
        }
        for (i, r) in tile_rects(&items, area, GAP) {
            if self.prof {
                eprintln!("tile {i} {}: {:?}", self.win_title(i), r);
            }
            self.wins[i].rect = Some(r);
            self.wins[i].prev = None;
            self.wins[i].collapsed = false;
        }
    }

    /// First layout after start: rescale the saved arrangement to the current area, or tile.
    fn apply_initial_layout(&mut self, area: Rect) {
        let missing = self.wins.iter().any(|w| w.open && w.rect.is_none());
        match self.saved_area {
            Some(sa) if !missing => {
                let (sx, sy) = (area.width() / sa.x, area.height() / sa.y);
                for w in self.wins.iter_mut() {
                    if let Some(r) = w.rect {
                        let min = area.min + vec2(r.min.x * sx, r.min.y * sy);
                        w.rect = Some(Rect::from_min_size(min, vec2(r.width() * sx, r.height() * sy)));
                    }
                }
            }
            _ => {
                for w in self.wins.iter_mut() {
                    w.rect = None;
                }
                self.tile_windows(area);
            }
        }
        self.layout_ready = true;
    }

    fn toggle_max(&mut self, i: usize, area: Rect) {
        let w = &mut self.wins[i];
        if let Some(p) = w.prev.take() {
            w.rect = Some(p);
        } else {
            w.prev = w.rect;
            w.rect = Some(area.shrink(GAP * 0.5));
            w.collapsed = false;
        }
    }

    pub fn current_mask(&self) -> u32 {
        (0..WIN_COUNT).fold(0u32, |m, i| m | ((self.wins[i].open as u32) << i))
    }

    /// Content with a minimum width must not push its window wider than the tile it was given
    /// (that made tiled windows overlap): overflow scrolls sideways instead.
    fn no_grow(&mut self, ui: &mut Ui, add: impl FnOnce(&mut Self, &mut Ui)) {
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| add(self, ui));
    }

    /// Contents of window `i` (shared by the floating window and the pop-out viewport).
    fn window_content(&mut self, ui: &mut Ui, i: usize) {
        match i {
            0 => self.panel_spectrum(ui),
            1 => {
                if self.rotated {
                    self.panel_waterfall(ui)
                } else {
                    self.panel_spectrogram(ui)
                }
            }
            2 => self.no_grow(ui, |app, ui| app.panel_stereo(ui)),
            3 => self.panel_3d(ui),
            4 => self.panel_scope(ui),
            5 => self.no_grow(ui, |app, ui| {
                egui::ScrollArea::vertical().show(ui, |ui| app.measurements_ui(ui));
            }),
            6 => self.no_grow(ui, |app, ui| app.generator_ui(ui)),
            7 => self.no_grow(ui, |app, ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    app.cursors_ui(ui);
                    ui.separator();
                    app.snapshots_ui(ui);
                });
            }),
            _ => self.no_grow(ui, |app, ui| app.tool_ui(ui, i - BUILTIN)),
        }
    }

    /// Rect a window currently occupies on screen (a collapsed window is only its title bar).
    fn shown_rect(&self, i: usize, area: Rect) -> Rect {
        let w = &self.wins[i];
        let casc = i as f32 * 24.0;
        let r = w.rect.unwrap_or_else(|| Rect::from_min_size(area.min + vec2(30.0 + casc, 30.0 + casc), vec2(560.0, 380.0)));
        if w.collapsed { Rect::from_min_size(r.min, vec2(r.width(), TITLE_H)) } else { r }
    }

    /// Keep a window inside the area and above the minimum size.
    fn clamp_rect(r: Rect, area: Rect) -> Rect {
        let size = vec2(r.width().clamp(MIN_W, area.width().max(MIN_W)), r.height().clamp(MIN_H, area.height().max(MIN_H)));
        let min = pos2(r.min.x.clamp(area.min.x, (area.max.x - size.x).max(area.min.x)), r.min.y.clamp(area.min.y, (area.max.y - size.y).max(area.min.y)));
        Rect::from_min_size(min, size)
    }

    /// Open, docked windows other than `i` (the things a window can snap to).
    fn snap_targets(&self, i: usize, area: Rect) -> Vec<Rect> {
        (0..WIN_COUNT).filter(|&j| j != i && self.wins[j].open && !self.popped[j]).map(|j| self.shown_rect(j, area)).collect()
    }

    /// Smallest correction (< SNAP) that moves `m` onto one of `targets`.
    fn best_delta(m: f32, targets: &[f32]) -> Option<f32> {
        let mut best: Option<f32> = None;
        for &t in targets {
            let d = t - m;
            if d.abs() < SNAP && best.map_or(true, |b| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
        best
    }

    /// Corrections for the low and the high edge of a window on one axis. `lo`/`hi` are the edge
    /// positions, `(o_lo, o_hi)` the extent on the other axis (only neighbours near it count).
    fn axis_snap(lo: f32, hi: f32, free: (bool, bool), other: (f32, f32), area: (f32, f32), nbrs: &[Rect], horizontal: bool) -> (Option<f32>, Option<f32>) {
        let mut lo_t = vec![area.0 + GAP];
        let mut hi_t = vec![area.1 - GAP];
        for o in nbrs {
            let (n_lo, n_hi, p_lo, p_hi) = if horizontal { (o.min.x, o.max.x, o.min.y, o.max.y) } else { (o.min.y, o.max.y, o.min.x, o.max.x) };
            if other.0 - 40.0 > p_hi || other.1 + 40.0 < p_lo {
                continue;
            }
            lo_t.push(n_hi + GAP);
            lo_t.push(n_lo);
            hi_t.push(n_lo - GAP);
            hi_t.push(n_hi);
        }
        (free.0.then(|| Self::best_delta(lo, &lo_t)).flatten(), free.1.then(|| Self::best_delta(hi, &hi_t)).flatten())
    }

    fn snap_move(&mut self, i: usize, raw: Rect, area: Rect) -> Rect {
        self.snap_guides.clear();
        let nb = self.snap_targets(i, area);
        let (xl, xh) = Self::axis_snap(raw.min.x, raw.max.x, (true, true), (raw.min.y, raw.max.y), (area.min.x, area.max.x), &nb, true);
        let (yl, yh) = Self::axis_snap(raw.min.y, raw.max.y, (true, true), (raw.min.x, raw.max.x), (area.min.y, area.max.y), &nb, false);
        let pick = |a: Option<f32>, b: Option<f32>| match (a, b) {
            (Some(x), Some(y)) => Some(if x.abs() <= y.abs() { x } else { y }),
            (x, y) => x.or(y),
        };
        let (dx, dy) = (pick(xl, xh), pick(yl, yh));
        let r = raw.translate(vec2(dx.unwrap_or(0.0), dy.unwrap_or(0.0)));
        if let Some(d) = dx {
            self.snap_guides.push((true, if xl == Some(d) { r.min.x } else { r.max.x }));
        }
        if let Some(d) = dy {
            self.snap_guides.push((false, if yl == Some(d) { r.min.y } else { r.max.y }));
        }
        r
    }

    /// Where a window docks when released against an edge of the area (like Windows' snap).
    fn dock_zone(area: Rect, p: Pos2) -> Option<Rect> {
        let m = 4.0;
        let (l, r, t, b) = (p.x <= area.min.x + m, p.x >= area.max.x - m, p.y <= area.min.y + m, p.y >= area.max.y - m);
        if !(l || r || t || b) {
            return None;
        }
        let a = area.shrink(GAP);
        let (c, h) = (a.center(), GAP * 0.5);
        let (x0, x1, y0, y1) = (a.min.x, a.max.x, a.min.y, a.max.y);
        let (left, right, top, bottom) = ((x0, c.x - h), (c.x + h, x1), (y0, c.y - h), (c.y + h, y1));
        let (xs, ys) = match (l, r, t, b) {
            (true, _, true, _) => (left, top),
            (_, true, true, _) => (right, top),
            (true, _, _, true) => (left, bottom),
            (_, true, _, true) => (right, bottom),
            (true, ..) => (left, (y0, y1)),
            (_, true, ..) => (right, (y0, y1)),
            (_, _, true, _) => ((x0, x1), (y0, y1)),
            _ => ((x0, x1), bottom),
        };
        Some(Rect::from_min_max(pos2(xs.0, ys.0), pos2(xs.1, ys.1)))
    }

    fn begin_drag(&mut self, ctx: &egui::Context, i: usize, kind: DragKind, area: Rect) {
        let Some(ptr) = ctx.input(|inp| inp.pointer.interact_pos()) else { return };
        let mut start = self.shown_rect(i, area);
        if let (DragKind::Move, Some(prev)) = (kind, self.wins[i].prev.take()) {
            // dragging a maximised / docked window pulls it back to its old size under the pointer
            let fx = ((ptr.x - start.min.x) / start.width().max(1.0)).clamp(0.0, 1.0);
            start = Rect::from_min_size(pos2(ptr.x - fx * prev.width(), start.min.y), prev.size());
            self.wins[i].rect = Some(start);
        }
        let mut links = Vec::new();
        if let DragKind::Resize { l, r, t, b } = kind {
            self.wins[i].prev = None;
            if self.snap_on && !ctx.input(|inp| inp.modifiers.alt) {
                for j in (0..WIN_COUNT).filter(|&j| j != i && self.wins[j].open && !self.popped[j] && !self.wins[j].collapsed) {
                    let o = self.shown_rect(j, area);
                    let ov_y = o.min.y < start.max.y - 16.0 && o.max.y > start.min.y + 16.0;
                    let ov_x = o.min.x < start.max.x - 16.0 && o.max.x > start.min.x + 16.0;
                    let near = |d: f32| d.abs() <= GAP + 2.0;
                    if r && ov_y && near(o.min.x - start.max.x) {
                        links.push(Link { j, my: Edge::Right, theirs: Edge::Left, off: o.min.x - start.max.x, orig: o });
                    }
                    if l && ov_y && near(o.max.x - start.min.x) {
                        links.push(Link { j, my: Edge::Left, theirs: Edge::Right, off: o.max.x - start.min.x, orig: o });
                    }
                    if b && ov_x && near(o.min.y - start.max.y) {
                        links.push(Link { j, my: Edge::Bottom, theirs: Edge::Top, off: o.min.y - start.max.y, orig: o });
                    }
                    if t && ov_x && near(o.max.y - start.min.y) {
                        links.push(Link { j, my: Edge::Top, theirs: Edge::Bottom, off: o.max.y - start.min.y, orig: o });
                    }
                }
            }
        }
        self.win_drag = Some(WinDrag { i, kind, start, ptr0: ptr, links });
        self.focus = i;
    }

    /// Per-frame handling of a window drag in progress (move or resize with snapping / linked edges).
    fn update_drag(&mut self, ctx: &egui::Context, area: Rect) {
        let Some(d) = self.win_drag.clone() else { return };
        let down = ctx.input(|inp| inp.pointer.primary_down());
        let Some(ptr) = ctx.input(|inp| inp.pointer.interact_pos()) else {
            self.finish_drag();
            return;
        };
        let free = !self.snap_on || ctx.input(|inp| inp.modifiers.ctrl);
        let delta = ptr - d.ptr0;
        let i = d.i;
        match d.kind {
            DragKind::Move => {
                let raw = Self::clamp_rect(d.start.translate(delta), area);
                let r = if free {
                    self.snap_guides.clear();
                    raw
                } else {
                    self.snap_move(i, raw, area)
                };
                let zone = if free { None } else { Self::dock_zone(area, ptr) };
                self.dock_preview = zone;
                if down {
                    self.set_rect(i, r);
                } else {
                    if let Some(z) = zone {
                        self.set_rect(i, z);
                        // remember the size to return to when it is dragged away again
                        let back = if d.start.width() < area.width() * 0.98 { d.start } else { Rect::from_min_size(z.min, vec2(560.0, 380.0)) };
                        self.wins[i].prev = Some(back);
                        self.wins[i].collapsed = false;
                    } else {
                        self.set_rect(i, r);
                    }
                    self.finish_drag();
                }
            }
            DragKind::Resize { l, r, t, b } => {
                let s = d.start;
                let (mut x0, mut x1, mut y0, mut y1) = (s.min.x, s.max.x, s.min.y, s.max.y);
                if l {
                    x0 += delta.x;
                }
                if r {
                    x1 += delta.x;
                }
                if t {
                    y0 += delta.y;
                }
                if b {
                    y1 += delta.y;
                }
                self.snap_guides.clear();
                if !free {
                    let nb: Vec<Rect> = self.snap_targets(i, area).into_iter().filter(|o| !d.links.iter().any(|k| k.orig == *o)).collect();
                    let (xl, xh) = Self::axis_snap(x0, x1, (l, r), (y0, y1), (area.min.x, area.max.x), &nb, true);
                    let (yl, yh) = Self::axis_snap(y0, y1, (t, b), (x0, x1), (area.min.y, area.max.y), &nb, false);
                    if let Some(v) = xl {
                        x0 += v;
                        self.snap_guides.push((true, x0));
                    }
                    if let Some(v) = xh {
                        x1 += v;
                        self.snap_guides.push((true, x1));
                    }
                    if let Some(v) = yl {
                        y0 += v;
                        self.snap_guides.push((false, y0));
                    }
                    if let Some(v) = yh {
                        y1 += v;
                        self.snap_guides.push((false, y1));
                    }
                }
                // limits: own minimum size, the area and the room the linked neighbours can give
                let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (area.min.x, area.max.x, area.min.y, area.max.y);
                for k in &d.links {
                    match k.my {
                        Edge::Right => hi_x = hi_x.min(k.orig.max.x - MIN_W - k.off),
                        Edge::Left => lo_x = lo_x.max(k.orig.min.x + MIN_W - k.off),
                        Edge::Bottom => hi_y = hi_y.min(k.orig.max.y - MIN_H - k.off),
                        Edge::Top => lo_y = lo_y.max(k.orig.min.y + MIN_H - k.off),
                    }
                }
                if l {
                    x0 = x0.clamp(lo_x, (x1 - MIN_W).max(lo_x));
                }
                if r {
                    x1 = x1.clamp((x0 + MIN_W).min(hi_x), hi_x);
                }
                if t {
                    y0 = y0.clamp(lo_y, (y1 - MIN_H).max(lo_y));
                }
                if b {
                    y1 = y1.clamp((y0 + MIN_H).min(hi_y), hi_y);
                }
                self.set_rect(i, Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)));
                for k in &d.links {
                    let mut o = k.orig;
                    match k.theirs {
                        Edge::Left => o.min.x = x1 + k.off,
                        Edge::Right => o.max.x = x0 + k.off,
                        Edge::Top => o.min.y = y1 + k.off,
                        Edge::Bottom => o.max.y = y0 + k.off,
                    }
                    self.wins[k.j].prev = None;
                    self.set_rect(k.j, o);
                }
                if !down {
                    self.finish_drag();
                }
            }
        }
        ctx.request_repaint();
    }

    fn finish_drag(&mut self) {
        self.win_drag = None;
        self.snap_guides.clear();
        self.dock_preview = None;
        // the arrangement is the user's now: do not re-tile because of the drag
        self.open_mask = self.current_mask();
    }

    fn set_rect(&mut self, i: usize, r: Rect) {
        let w = &mut self.wins[i];
        // a collapsed window keeps its full height
        let full_h = w.rect.map_or(r.height(), |o| o.height()).max(MIN_H);
        w.rect = Some(if w.collapsed { Rect::from_min_size(r.min, vec2(r.width(), full_h)) } else { r });
    }

    /// Draw all open windows inside `area`.
    pub fn show_windows(&mut self, ctx: &egui::Context, area: Rect) {
        self.area = Some(area);
        if self.settle > 0 {
            self.settle -= 1;
            if self.settle > 0 {
                return;
            }
            self.apply_initial_layout(area);
        }
        // the windows follow the area when it changes (side bar dragged, application window resized)
        if let Some(old) = self.prev_area.filter(|o| *o != area && o.width() > 50.0 && o.height() > 50.0) {
            if self.layout_ready && self.win_drag.is_none() {
                let (sx, sy) = (area.width() / old.width(), area.height() / old.height());
                let map = |r: Rect| Rect::from_min_size(area.min + vec2((r.min.x - old.min.x) * sx, (r.min.y - old.min.y) * sy), vec2(r.width() * sx, r.height() * sy));
                for w in self.wins.iter_mut() {
                    w.rect = w.rect.map(map);
                    w.prev = w.prev.map(map);
                }
            }
        }
        self.prev_area = Some(area);
        let mask = self.current_mask();
        if self.auto_tile && mask != self.open_mask && self.open_mask != 0 {
            self.need_tile = true;
        }
        self.open_mask = mask;
        if self.need_tile {
            self.tile_windows(area);
            self.need_tile = false;
        }
        if let Some(i) = self.max_req.take() {
            self.toggle_max(i, area);
        }
        self.update_drag(ctx, area);
        // a click anywhere inside a window raises it
        let pressed_layer = ctx.input(|inp| inp.pointer.any_pressed()).then(|| ctx.pointer_interact_pos().and_then(|p| ctx.layer_id_at(p))).flatten();
        let mut events: Vec<(usize, WinEv)> = Vec::new();
        for i in 0..WIN_COUNT {
            if !self.wins[i].open {
                continue;
            }
            let t0 = Instant::now();
            if self.popped[i] {
                self.show_popout(ctx, i);
                self.ms[i] += 0.1 * (t0.elapsed().as_secs_f32() * 1000.0 - self.ms[i]);
                continue;
            }
            if self.win_drag.as_ref().map_or(true, |d| d.i != i) {
                if let Some(r) = self.wins[i].rect {
                    let c = Self::clamp_rect(r, area);
                    if c != r {
                        self.wins[i].rect = Some(c);
                    }
                }
            }
            let r = self.shown_rect(i, area);
            let id = egui::Id::new(("visuscope_win", i));
            let focused = self.focus == i;
            let collapsed = self.wins[i].collapsed;
            let maximized = self.wins[i].prev.is_some();
            let title = self.win_title(i);
            let mut e = WinEv::default();
            let resp = egui::Area::new(id).order(egui::Order::Middle).fixed_pos(r.min).constrain(false).show(ctx, |ui| {
                let (frame_rect, _) = ui.allocate_exact_size(r.size(), egui::Sense::hover());
                ui.set_clip_rect(frame_rect);
                let p = ui.painter().clone();
                let edge = if focused { Color32::from_rgb(70, 100, 150) } else { Color32::from_rgb(50, 58, 72) };
                p.rect_filled(frame_rect, 5.0, WIN_FILL);
                let bar = Rect::from_min_size(frame_rect.min, vec2(frame_rect.width(), TITLE_H));
                let bottom_r = if collapsed { 5 } else { 0 };
                p.rect_filled(bar, egui::CornerRadius { nw: 5, ne: 5, sw: bottom_r, se: bottom_r }, if focused { Color32::from_rgb(38, 52, 76) } else { Color32::from_rgb(30, 36, 46) });
                // buttons, right to left: close, maximise, collapse
                let bw = 18.0;
                let mut x = bar.max.x - 2.0;
                let mut button = |ui: &mut Ui, kind: u8| -> bool {
                    let br = Rect::from_min_max(pos2(x - bw, bar.min.y + 1.0), pos2(x, bar.max.y - 1.0));
                    x -= bw + 1.0;
                    let rs = ui.interact(br, id.with(("btn", kind)), egui::Sense::click());
                    let hov = rs.hovered();
                    if hov {
                        p.rect_filled(br, 3.0, if kind == 0 { Color32::from_rgb(190, 60, 60) } else { Color32::from_rgb(70, 84, 108) });
                    }
                    let c = br.center();
                    let st = Stroke::new(1.3, if hov { Color32::WHITE } else { Color32::from_rgb(170, 180, 196) });
                    match kind {
                        0 => {
                            p.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], st);
                            p.line_segment([c + vec2(-3.0, 3.0), c + vec2(3.0, -3.0)], st);
                        }
                        1 => {
                            if maximized {
                                p.rect_stroke(Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(6.0, 5.0)), 0.0, st, egui::StrokeKind::Inside);
                                p.line_segment([c + vec2(-1.0, -2.5), c + vec2(3.5, -2.5)], st);
                                p.line_segment([c + vec2(3.5, -2.5), c + vec2(3.5, 2.0)], st);
                            } else {
                                p.rect_stroke(Rect::from_center_size(c, vec2(8.0, 7.0)), 0.0, st, egui::StrokeKind::Inside);
                            }
                        }
                        _ => {
                            if collapsed {
                                p.line_segment([c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0)], st);
                                p.line_segment([c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)], st);
                            } else {
                                p.line_segment([c + vec2(-3.5, 2.0), c + vec2(3.5, 2.0)], st);
                            }
                        }
                    }
                    let tip = match kind {
                        0 => "Close",
                        1 if maximized => "Restore",
                        1 => "Maximise",
                        _ if collapsed => "Expand",
                        _ => "Collapse to title bar",
                    };
                    rs.on_hover_text(tip).clicked()
                };
                e.close = button(ui, 0);
                e.max = button(ui, 1);
                e.collapse = button(ui, 2);
                let title_rect = Rect::from_min_max(bar.min, pos2(x + 1.0, bar.max.y));
                let tr = ui.interact(title_rect, id.with("title"), egui::Sense::click_and_drag());
                p.with_clip_rect(title_rect).text(bar.min + vec2(7.0, TITLE_H * 0.5), Align2::LEFT_CENTER, title, FontId::proportional(11.0), if focused { Color32::from_rgb(225, 232, 245) } else { Color32::from_rgb(160, 170, 186) });
                if tr.drag_started_by(egui::PointerButton::Primary) {
                    e.drag = Some(DragKind::Move);
                }
                if tr.double_clicked() {
                    e.max = true;
                }
                if tr.hovered() || tr.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                }
                tr.context_menu(|ui| {
                    if ui.button("Pop out to own window").clicked() {
                        e.pop = true;
                        ui.close();
                    }
                    if ui.button(if maximized { "Restore" } else { "Maximise" }).clicked() {
                        e.max = true;
                        ui.close();
                    }
                    if ui.button("Tile all windows").clicked() {
                        e.tile = true;
                        ui.close();
                    }
                });
                if !collapsed {
                    let content = Rect::from_min_max(bar.min + vec2(3.0, TITLE_H + 1.0), frame_rect.max - vec2(3.0, 3.0));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                        ui.shrink_clip_rect(content);
                        ui.set_min_size(content.size());
                        self.window_content(ui, i);
                    });
                    // resize zones last, so that they win over the content underneath
                    let (t, c) = (5.0, 11.0);
                    let f = frame_rect;
                    type Z = (Rect, (bool, bool, bool, bool), egui::CursorIcon);
                    let zones: [Z; 8] = [
                        (Rect::from_min_max(pos2(f.min.x + c, f.max.y - t), pos2(f.max.x - c, f.max.y)), (false, false, false, true), egui::CursorIcon::ResizeVertical),
                        (Rect::from_min_max(pos2(f.min.x, f.min.y + TITLE_H), pos2(f.min.x + t, f.max.y - c)), (true, false, false, false), egui::CursorIcon::ResizeHorizontal),
                        (Rect::from_min_max(pos2(f.max.x - t, f.min.y + TITLE_H), pos2(f.max.x, f.max.y - c)), (false, true, false, false), egui::CursorIcon::ResizeHorizontal),
                        (Rect::from_min_max(pos2(f.min.x + c, f.min.y), pos2(f.max.x - c, f.min.y + 3.0)), (false, false, true, false), egui::CursorIcon::ResizeVertical),
                        (Rect::from_min_size(pos2(f.min.x, f.max.y - c), vec2(c, c)), (true, false, false, true), egui::CursorIcon::ResizeNeSw),
                        (Rect::from_min_size(pos2(f.max.x - c, f.max.y - c), vec2(c, c)), (false, true, false, true), egui::CursorIcon::ResizeNwSe),
                        (Rect::from_min_size(f.min, vec2(c, 4.0)), (true, false, true, false), egui::CursorIcon::ResizeNwSe),
                        (Rect::from_min_size(pos2(f.max.x - c, f.min.y), vec2(c, 4.0)), (false, true, true, false), egui::CursorIcon::ResizeNeSw),
                    ];
                    for (k, (zr, (l, r_, t_, b), cur)) in zones.into_iter().enumerate() {
                        let rs = ui.interact(zr, id.with(("rs", k)), egui::Sense::drag());
                        if rs.hovered() || rs.dragged() {
                            ctx.set_cursor_icon(cur);
                        }
                        if rs.drag_started_by(egui::PointerButton::Primary) {
                            e.drag = Some(DragKind::Resize { l, r: r_, t: t_, b });
                        }
                    }
                }
                p.rect_stroke(frame_rect, 5.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
            });
            e.raise = pressed_layer == Some(resp.response.layer_id);
            events.push((i, e));
            self.ms[i] += 0.1 * (t0.elapsed().as_secs_f32() * 1000.0 - self.ms[i]);
        }
        for (i, e) in events {
            if e.raise {
                self.focus = i;
                ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, egui::Id::new(("visuscope_win", i))));
            }
            if let Some(k) = e.drag {
                if self.win_drag.is_none() {
                    self.begin_drag(ctx, i, k, area);
                }
            }
            if e.collapse {
                self.wins[i].collapsed = !self.wins[i].collapsed;
            }
            if e.max {
                self.max_req = Some(i);
            }
            if e.pop {
                self.popped[i] = true;
                self.need_tile |= self.auto_tile;
            }
            if e.tile {
                self.need_tile = true;
            }
            if e.close {
                self.wins[i].open = false;
            }
        }
        // snap guides and the dock preview while dragging
        if self.win_drag.is_some() {
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("snap_overlay")));
            for &(vertical, pos) in &self.snap_guides {
                let (a, b) = if vertical { (pos2(pos, area.min.y), pos2(pos, area.max.y)) } else { (pos2(area.min.x, pos), pos2(area.max.x, pos)) };
                p.line_segment([a, b], Stroke::new(1.0, Color32::from_rgba_unmultiplied(110, 170, 255, 150)));
            }
            if let Some(z) = self.dock_preview {
                p.rect_filled(z, 6.0, Color32::from_rgba_unmultiplied(90, 150, 255, 40));
                p.rect_stroke(z, 6.0, Stroke::new(1.5, Color32::from_rgba_unmultiplied(120, 175, 255, 170)), egui::StrokeKind::Inside);
            }
        }
    }

    /// The window as its own native window (drag it to another monitor); closing it docks it back.
    fn show_popout(&mut self, ctx: &egui::Context, i: usize) {
        let id = egui::ViewportId::from_hash_of(("visuscope_popout", i));
        let r = self.wins[i].rect.map(|r| r.size()).unwrap_or(vec2(720.0, 480.0));
        let builder = egui::ViewportBuilder::default().with_title(format!("VisuScope - {}", self.win_title(i))).with_inner_size(r.max(vec2(320.0, 220.0))).with_min_inner_size([240.0, 150.0]);
        let mut close = false;
        ctx.show_viewport_immediate(id, builder, |ui, _class| {
            if ui.ctx().input(|inp| inp.viewport().close_requested()) {
                close = true;
            }
            egui::CentralPanel::default().show(ui, |ui| self.window_content(ui, i));
        });
        if close {
            self.popped[i] = false;
            self.need_tile |= self.auto_tile;
        }
    }

    pub fn top_bar_ui(&mut self, ui: &mut Ui) {
        egui::Panel::top("topbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new("Windows").weak());
                for i in 0..BUILTIN {
                    let mut o = self.wins[i].open;
                    if ui.toggle_value(&mut o, win_short(i)).on_hover_text("Show / hide this window").changed() {
                        self.wins[i].open = o;
                    }
                }
                if let Some(k) = (0..TOOL_COUNT).find(|&k| tools::INFO[k].short == "Timeline") {
                    let i = BUILTIN + k;
                    let mut o = self.wins[i].open;
                    if ui.toggle_value(&mut o, egui::RichText::new("Timeline").strong()).on_hover_text(tools::INFO[k].tip).changed() {
                        self.wins[i].open = o;
                    }
                }
                if self.graph_on {
                    let i = BUILTIN + tools::ROUTING;
                    let mut o = self.wins[i].open;
                    if ui.toggle_value(&mut o, egui::RichText::new("Routing").strong()).on_hover_text(tools::INFO[tools::ROUTING].tip).changed() {
                        self.wins[i].open = o;
                    }
                }
                let open_tools = (BUILTIN..WIN_COUNT).filter(|&i| self.wins[i].open).count();
                let label = if open_tools > 0 { format!("Tools ({open_tools})") } else { "Tools".to_string() };
                ui.menu_button(label, |ui| {
                    for (g, (name, members)) in tools::GROUPS.iter().enumerate() {
                        if g > 0 {
                            ui.separator();
                        }
                        ui.label(egui::RichText::new(*name).small().weak());
                        for &k in members.iter() {
                            if k == tools::ROUTING {
                                // listed in the top bar while the graph is on
                                continue;
                            }
                            let i = BUILTIN + k;
                            let info = &tools::INFO[k];
                            let mut o = self.wins[i].open;
                            if ui.checkbox(&mut o, info.title).on_hover_text(info.tip).changed() {
                                self.wins[i].open = o;
                            }
                        }
                    }
                });
                self.layouts_menu(ui);
                ui.separator();
                if ui.button("Tile").on_hover_text("Fill the area without gaps; visual windows get the most space").clicked() {
                    self.need_tile = true;
                }
                ui.checkbox(&mut self.auto_tile, "Auto").on_hover_text("Re-tile automatically when a window is opened or closed");
                ui.checkbox(&mut self.snap_on, "Snap").on_hover_text("Windows snap to each other and dock against the edges.\nCtrl while dragging: free. Alt while resizing: do not move neighbours.");
                if ui.button("Reset layout").on_hover_text("Default windows, tiled").clicked() {
                    for i in 0..WIN_COUNT {
                        self.wins[i] = WinState::new(i);
                        self.popped[i] = false;
                    }
                    self.need_tile = true;
                }
                ui.separator();
                let lbl = if self.p.paused { "Resume" } else { "Freeze" };
                if ui.selectable_label(self.p.paused, lbl).on_hover_text("Freeze the analysis").clicked() {
                    self.p.paused = !self.p.paused;
                }
                if ui.button("PNG").on_hover_text("Save a screenshot").clicked() {
                    self.request_png(ui.ctx());
                }
                if ui.button("Options").on_hover_text("Frame rate, window behaviour, folders ...").clicked() {
                    self.opts_open = !self.opts_open;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("v{}", crate::VERSION)).small().weak()).on_hover_text(format!("VisuScope {}", crate::VERSION_FULL));
                });
            });
        });
    }

    pub fn status_bar_ui(&mut self, ui: &mut Ui) {
        let statuses = self.tool_statuses();
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                // every part has a fixed width, so that text that changes or flashes (alarms, recording time,
                // frame rates) never pushes the parts to its right around
                let cell = |ui: &mut Ui, w: f32, add: &mut dyn FnMut(&mut Ui)| {
                    ui.allocate_ui_with_layout(vec2(w, 16.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(w);
                        ui.set_max_width(w);
                        add(ui);
                    });
                };
                let xr = self.sh.xruns.load(std::sync::atomic::Ordering::Relaxed);
                let status = self.sh.status.lock().clone();
                cell(ui, 270.0, &mut |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(format!("{status}   xruns {xr}")).small()).truncate());
                });
                if self.sh.feeding_silence.load(std::sync::atomic::Ordering::Relaxed) {
                    ui.label(egui::RichText::new("no audio from device (silent loopback or stalled stream) - showing silence").small().color(egui::Color32::from_rgb(255, 190, 80)));
                }
                ui.separator();
                // the alarms and recording messages of the tools share one box of constant size
                cell(ui, 250.0, &mut |ui| {
                    for (text, col) in &statuses {
                        ui.add(egui::Label::new(egui::RichText::new(text).small().strong().color(*col)).truncate());
                    }
                });
                ui.separator();
                let perf = format!("UI {:.0} fps ({:.1} ms)   DSP {:.0} fps ({:.2} ms)", self.ui_fps, self.ms[WIN_COUNT], self.an.dsp_fps, self.an.dsp_ms);
                cell(ui, 290.0, &mut |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&perf).small().weak()).truncate());
                });
                if !self.note.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new(&self.note).small());
                }
            });
        });
    }

    pub fn request_png(&mut self, ctx: &egui::Context) {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.request_png_to(ctx, format!("visuscope_{ts}.png"));
    }

    pub fn request_png_to(&mut self, ctx: &egui::Context, path: String) {
        let note = self.shot_note.clone();
        ctx.request_screenshot(move |img| {
            let (w, h) = (img.width() as u32, img.height() as u32);
            let mut raw = Vec::with_capacity((w * h * 4) as usize);
            for p in &img.pixels {
                raw.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
            }
            *note.lock() = match image::save_buffer(&path, &raw, w, h, image::ColorType::Rgba8) {
                Ok(_) => format!("Saved {path}"),
                Err(e) => format!("PNG error: {e}"),
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0))
    }

    #[test]
    fn edges_snap_to_neighbours_with_a_gap() {
        // a window whose left edge is 6 px off "right edge of the neighbour + gap"
        let nb = [Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0))];
        let (lo, _) = App::axis_snap(400.0 + GAP + 6.0, 700.0, (true, true), (0.0, 300.0), (0.0, 1000.0), &nb, true);
        assert_eq!(lo, Some(-6.0));
        // far away: no snap
        let (lo, hi) = App::axis_snap(500.0, 800.0, (true, true), (0.0, 300.0), (0.0, 1000.0), &nb, true);
        assert!(lo.is_none() && hi.is_none());
        // neighbours that are not near vertically are ignored
        let far = [Rect::from_min_max(pos2(0.0, 500.0), pos2(400.0, 590.0))];
        let (lo, _) = App::axis_snap(405.0, 700.0, (true, true), (0.0, 100.0), (0.0, 1000.0), &far, true);
        assert!(lo.is_none());
        // the area edge counts too
        let (_, hi) = App::axis_snap(100.0, 1000.0 - GAP - 3.0, (true, true), (0.0, 100.0), (0.0, 1000.0), &[], true);
        assert_eq!(hi, Some(3.0));
    }

    #[test]
    fn docking_zones() {
        let a = area();
        assert!(App::dock_zone(a, pos2(500.0, 300.0)).is_none());
        let left = App::dock_zone(a, pos2(1.0, 300.0)).unwrap();
        assert!(left.width() < a.width() * 0.51 && left.height() > a.height() * 0.95 && left.min.x < 10.0);
        let right = App::dock_zone(a, pos2(999.0, 300.0)).unwrap();
        assert!(right.max.x > 990.0 && right.min.x > 400.0);
        let max = App::dock_zone(a, pos2(500.0, 1.0)).unwrap();
        assert!(max.width() > 980.0 && max.height() > 580.0);
        let quarter = App::dock_zone(a, pos2(1.0, 599.0)).unwrap();
        assert!(quarter.width() < 520.0 && quarter.height() < 300.0 && quarter.min.y > 290.0);
        // halves do not overlap
        assert!(left.max.x < right.min.x);
    }

    #[test]
    fn windows_stay_inside_the_area() {
        let r = App::clamp_rect(Rect::from_min_size(pos2(900.0, -50.0), vec2(400.0, 300.0)), area());
        assert!(area().contains_rect(r), "{r:?}");
        let tiny = App::clamp_rect(Rect::from_min_size(pos2(10.0, 10.0), vec2(20.0, 20.0)), area());
        assert!(tiny.width() >= MIN_W && tiny.height() >= MIN_H);
        // an area smaller than the minimum size must not panic
        let _ = App::clamp_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0)), Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 60.0)));
    }

    #[test]
    fn timeline_is_a_wide_strip_and_the_routing_window_gets_the_most_room() {
        let (tl, rt) = (BUILTIN + 14, BUILTIN + 15);
        assert!(wants_wide(tl) && !wants_wide(rt));
        for a in [Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0)), Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 1000.0)), Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 900.0))] {
            let items: Vec<(usize, f32)> = vec![(tl, win_weight(tl)), (rt, win_weight(rt)), (0, win_weight(0)), (4, win_weight(4)), (5, win_weight(5))];
            let out = tile_rects(&items, a, GAP);
            assert_eq!(out.len(), 5);
            let get = |i: usize| out.iter().find(|o| o.0 == i).unwrap().1;
            assert!(get(tl).width() > get(tl).height(), "timeline {:?} in {:?}", get(tl), a);
            let area_of = |r: Rect| r.width() * r.height();
            assert!(out.iter().filter(|o| o.0 != rt && o.0 != tl).all(|o| area_of(o.1) < area_of(get(rt)) * 1.01), "routing is the largest of the rest");
            for (n, (_, ra)) in out.iter().enumerate() {
                assert!(a.contains_rect(*ra));
                for (_, rb) in out.iter().skip(n + 1) {
                    assert!(!ra.intersects(*rb));
                }
            }
        }
    }

    #[test]
    fn treemap_covers_the_area_without_overlap() {
        let items: Vec<(usize, f32)> = (0..9).map(|i| (i, 1.0 + i as f32 * 0.3)).collect();
        let mut out = Vec::new();
        split_rects(&items, area().shrink(GAP), GAP, &mut out);
        assert_eq!(out.len(), 9);
        for (a, (_, ra)) in out.iter().enumerate() {
            assert!(area().contains_rect(*ra));
            for (_, rb) in out.iter().skip(a + 1) {
                assert!(!ra.intersects(*rb));
            }
        }
    }
}
