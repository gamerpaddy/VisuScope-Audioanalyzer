//! The node editor window ("Routing"): drag from an output dot to an input dot to connect, drag
//! an input away to move or drop the connection, Space opens the node search, right-click a wire to
//! insert a node into it. Everything the engine runs comes from the graph edited here.

use super::dsp::{db_to_lin, EqUnit};
use super::engine::{bus, Engine, Probe};
use super::model::*;
use crate::tools::daw::capture::{short_label, DevEx};
use crate::tools::{Ctx, SettingsIn, SettingsOut, Tool};
use eframe::egui::{self, epaint::CubicBezierShape, pos2, vec2, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, UiBuilder};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

mod nodes;
mod canvas;

const HDR: f32 = 22.0;
const ROW: f32 = 22.0;
const PORT_R: f32 = 5.5;
/// Colours of the equaliser bands (dot in the graph, stripe in the list).
const EQ_PALETTE: [Color32; 8] = [
    Color32::from_rgb(255, 120, 120),
    Color32::from_rgb(255, 190, 90),
    Color32::from_rgb(240, 235, 100),
    Color32::from_rgb(120, 225, 130),
    Color32::from_rgb(100, 215, 225),
    Color32::from_rgb(120, 160, 255),
    Color32::from_rgb(190, 130, 255),
    Color32::from_rgb(255, 130, 200),
];
const EQ_MAX: usize = super::dsp::EQ_MAX_BANDS;
const BODY_BG: Color32 = Color32::from_rgb(27, 32, 42);
const CANVAS_BG: Color32 = Color32::from_rgb(16, 19, 25);

fn node_w(k: Kind) -> f32 {
    match k {
        Kind::Eq => 340.0,
        Kind::Mixer => 230.0,
        Kind::Input | Kind::Output | Kind::Comp | Kind::Gate => 240.0,
        _ => 210.0,
    }
}

/// Position of an input port in canvas coordinates.
fn in_pos(n: &Node, port: usize) -> Pos2 {
    if matches!(n.kind, Kind::Mixer | Kind::Merge) || (n.kind == Kind::Output && n.inputs() == 2) {
        // a mixer has the "Inputs" and "Master" rows above its inputs
        let above = if n.kind == Kind::Mixer { 2.0 } else { 0.0 };
        pos2(n.x, n.y + HDR + 11.0 + (above + port as f32) * ROW + ROW * 0.5)
    } else {
        pos2(n.x, n.y + HDR * 0.5)
    }
}

fn out_pos(n: &Node, port: usize) -> Pos2 {
    if matches!(n.kind, Kind::Split | Kind::Dual) {
        pos2(n.x + node_w(n.kind), n.y + HDR + 11.0 + port as f32 * ROW + ROW * 0.5)
    } else {
        pos2(n.x + node_w(n.kind), n.y + HDR * 0.5)
    }
}

/// A number with a small up and down arrow (instead of a slider). Returns true when it changed.
fn spinner(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug, v: &mut usize, lo: usize, hi: usize, z: f32) -> bool {
    let mut changed = false;
    let mut f = *v as f32;
    if ui.add(egui::DragValue::new(&mut f).range(lo as f32..=hi as f32).speed(0.05).max_decimals(0)).changed() {
        *v = (f.round() as usize).clamp(lo, hi);
        changed = true;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(16.0 * z, 16.0 * z), Sense::hover());
    let (up_r, dn_r) = (Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y)), Rect::from_min_max(pos2(rect.min.x, rect.center().y), rect.max));
    let id = ui.scope_id().with(salt);
    for (k, (r, up)) in [(up_r, true), (dn_r, false)].into_iter().enumerate() {
        let resp = ui.interact(r, id.with(k), Sense::click());
        let enabled = if up { *v < hi } else { *v > lo };
        let col = if !enabled { Color32::from_gray(70) } else if resp.hovered() { Color32::WHITE } else { Color32::from_gray(170) };
        let (c, h, w) = (r.center(), 2.8 * z, 3.8 * z);
        let tri = if up { vec![c + vec2(0.0, -h), c + vec2(w, h), c + vec2(-w, h)] } else { vec![c + vec2(0.0, h), c + vec2(w, -h), c + vec2(-w, -h)] };
        ui.painter().add(Shape::convex_polygon(tri, col, Stroke::NONE));
        if resp.clicked() && enabled {
            *v = if up { *v + 1 } else { *v - 1 };
            changed = true;
        }
    }
    changed
}

/// The drawn phase curve of a phase node: drag to draw (like a frequency response), double-click for flat.
fn phase_curve(ui: &mut Ui, n: &mut Node, z: f32) -> bool {
    use crate::graph::fx::{curve_encode, curve_freq, curve_parse, CURVE_N};
    let mut v = curve_parse(&n.s);
    let (r, resp) = ui.allocate_exact_size(vec2((ui.available_width() - 4.0).max(120.0), 100.0 * z), Sense::click_and_drag());
    let p = ui.painter_at(r);
    p.rect_filled(r, 2.0, Color32::from_gray(18));
    let xo = |i: f32| r.left() + r.width() * i / (CURVE_N - 1) as f32;
    let yo = |d: f32| r.center().y - d / 360.0 * r.height();
    for (d, lab) in [(180.0, "180"), (90.0, "90"), (0.0, "0"), (-90.0, "-90"), (-180.0, "-180")] {
        let c = if d == 0.0 { Color32::from_gray(80) } else { Color32::from_gray(38) };
        p.line_segment([pos2(r.left(), yo(d)), pos2(r.right(), yo(d))], Stroke::new(1.0, c));
        p.text(pos2(r.left() + 2.0, yo(d) + if d > 100.0 { 6.0 } else { -6.0 } * z), Align2::LEFT_CENTER, lab, egui::FontId::proportional(8.5 * z), Color32::from_gray(110));
    }
    for f in [100.0f32, 1000.0, 10000.0] {
        let i = (f / 20.0).ln() / 1000f32.ln() * (CURVE_N - 1) as f32;
        p.line_segment([pos2(xo(i), r.top()), pos2(xo(i), r.bottom())], Stroke::new(1.0, Color32::from_gray(38)));
        p.text(pos2(xo(i) + 2.0, r.bottom() - 6.0 * z), Align2::LEFT_CENTER, if f >= 1000.0 { format!("{} k", f / 1000.0) } else { format!("{f}") }, egui::FontId::proportional(8.5 * z), Color32::from_gray(110));
    }
    let idx = |pos: Pos2| ((pos.x - r.left()) / r.width() * (CURVE_N - 1) as f32).clamp(0.0, (CURVE_N - 1) as f32);
    let deg = |pos: Pos2| {
        let d = ((r.center().y - pos.y) / r.height() * 360.0).clamp(-180.0, 180.0);
        if d.abs() < 4.0 { 0.0 } else { d }
    };
    let mut ch = false;
    if resp.double_clicked() {
        v = vec![0.0; CURVE_N];
        ch = true;
    } else if (resp.dragged_by(egui::PointerButton::Primary) || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos() {
            let prev = pos - ui.input(|i| i.pointer.delta());
            let (a, b) = (idx(prev), idx(pos));
            let (da, db) = (deg(prev), deg(pos));
            let (lo, hi) = (a.min(b).round() as usize, a.max(b).round() as usize);
            for k in lo..=hi {
                let t = if (b - a).abs() < 1e-3 { 1.0 } else { (k as f32 - a) / (b - a) };
                v[k] = (da + (db - da) * t.clamp(0.0, 1.0)).clamp(-180.0, 180.0);
            }
            ch = true;
        }
    let pts: Vec<Pos2> = v.iter().enumerate().map(|(i, d)| pos2(xo(i as f32), yo(*d))).collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.6, Color32::from_rgb(255, 170, 70))));
    if let Some(pos) = resp.hover_pos() {
        let i = idx(pos).round() as usize;
        resp.clone().on_hover_text(format!("{:.0} Hz: {:.0} deg\nDrag to draw, double-click for flat (off, no delay)", curve_freq(i), v[i]));
    }
    if ch {
        n.s = curve_encode(&v);
    }
    ch
}

/// Scale the fonts and the spacing of a style (the zoom of the node bodies).
fn scale_style(st: &mut egui::Style, z: f32) {
    if (z - 1.0).abs() < 1e-3 {
        return;
    }
    for f in st.text_styles.values_mut() {
        f.size *= z;
    }
    let sp = &mut st.spacing;
    sp.item_spacing *= z;
    sp.button_padding *= z;
    sp.interact_size *= z;
    sp.slider_width *= z;
    sp.slider_rail_height *= z;
    sp.icon_width *= z;
    sp.icon_width_inner *= z;
    sp.icon_spacing *= z;
    sp.combo_width *= z;
    sp.text_edit_width *= z;
    sp.indent *= z;
    sp.combo_height *= z;
}

/// One half of a round port dot.
fn half_dot(p: &egui::Painter, c: Pos2, r: f32, left: bool, col: Color32) {
    let pts: Vec<Pos2> = (0..=10)
        .map(|i| {
            let a = std::f32::consts::FRAC_PI_2 + (i as f32 / 10.0) * std::f32::consts::PI * if left { 1.0 } else { -1.0 };
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn wire_pts(a: Pos2, b: Pos2) -> [Pos2; 4] {
    let dx = ((b.x - a.x).abs() * 0.5).max(50.0);
    [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b]
}

/// Distance from a point to the curve of a wire: to many short pieces of it, as many as its length needs (a long wire has gaps
/// of a hundred pixels between 21 points, and a click between them found nothing).
fn wire_distance(pts: &[Pos2; 4], p: Pos2) -> f32 {
    let len: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    let n = ((len / 6.0) as usize).clamp(24, 800);
    let mut best = f32::MAX;
    let mut prev = pts[0];
    for k in 1..=n {
        let cur = bezier_at(pts, k as f32 / n as f32);
        let (ab, ap) = (cur - prev, p - prev);
        let t = if ab.length_sq() > 1e-9 { (ap.dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
        best = best.min((prev + ab * t).distance(p));
        prev = cur;
    }
    best
}

/// The port nearest to `p` within `radius` (not the first one within it: the rows of a mixer are close together).
enum ModReq {
    Set(Modulation),
    Clear(NodeId, usize),
}

/// The menus of the parameters cannot reach the graph: what they may choose from, what is set now, and what they ask for.
static MOD_SRC: parking_lot::Mutex<Vec<(NodeId, String)>> = parking_lot::Mutex::new(Vec::new());
static MOD_CUR: parking_lot::Mutex<Vec<Modulation>> = parking_lot::Mutex::new(Vec::new());
/// Where the parameter rows of the nodes are on the screen (the left edge of the node and the middle of the row): the end of the
/// dashed line of a parameter that follows a signal.
static MOD_ROWS: parking_lot::Mutex<Vec<((NodeId, usize), Pos2)>> = parking_lot::Mutex::new(Vec::new());
static MOD_REQ: parking_lot::Mutex<Vec<ModReq>> = parking_lot::Mutex::new(Vec::new());

/// The entries of the right-click menu of a parameter that let it follow a signal of the graph.
/// The value a followed parameter has right now (for the display), and whether it is followed at all.
fn followed_value(node: NodeId, i: usize, base: f32) -> (f32, bool) {
    if !MOD_CUR.lock().iter().any(|m| m.node == node && m.param == i) {
        return (base, false);
    }
    let live = super::engine::MOD_LIVE.lock().iter().find(|m| m.0 == node && m.1 == i).map(|m| m.2).filter(|v| v.is_finite());
    (live.unwrap_or(base), true)
}

/// While a wire is dragged from an output dot: the control ports of the parameters light up.
static MOD_DRAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The range of a value that can be controlled from a signal: (lowest, highest, logarithmic, unit).
fn param_range(n: &Node, i: usize) -> Option<(f32, f32, bool, &'static str)> {
    if n.kind == Kind::Eq {
        return match i % 4 {
            1 => Some((20.0, 20000.0, true, " Hz")),
            2 => Some((-24.0, 24.0, false, " dB")),
            3 => Some((0.1, 10.0, true, "")),
            _ => None,
        };
    }
    if n.kind == Kind::Mixer && (2..2 + MIX_MAX).contains(&i) {
        return Some((-60.0, 12.0, false, " dB"));
    }
    // a setting with two entries (on / off) is a switch: a signal above a threshold turns it on
    if n.kind.info().params.get(i).is_some_and(|d| d.choices.len() == 2) {
        return Some((0.0, 1.0, false, "switch"));
    }
    n.kind.info().params.get(i).filter(|d| d.choices.is_empty()).map(|d| (d.min, d.max, d.log, d.unit))
}

/// A control input of a value: a small dot (filled when a signal controls it). Drag a wire from the output of any node onto it;
/// right-click it for the range and to disconnect. `at` is where it sits on the screen.
fn param_port(ui: &Ui, z: f32, at: Pos2, node: NodeId, i: usize, range: (f32, f32, bool, &'static str)) {
    let followed = MOD_CUR.lock().iter().any(|m| m.node == node && m.param == i);
    let p = ui.painter().clone().with_clip_rect(Rect::EVERYTHING);
    let orange = Color32::from_rgb(255, 170, 70);
    if MOD_DRAG.load(std::sync::atomic::Ordering::Relaxed) {
        p.circle_stroke(at, 7.0 * z, Stroke::new(1.5, Color32::from_rgb(255, 214, 80)));
    }
    if followed {
        p.circle(at, 4.5 * z, orange, Stroke::new(1.0, Color32::from_rgb(40, 30, 20)));
    } else {
        p.circle(at, 3.2 * z, Color32::from_rgb(24, 28, 36), Stroke::new(1.0, Color32::from_rgb(120, 130, 150)));
    }
    {
        let mut rows = MOD_ROWS.lock();
        rows.retain(|r| r.0 != (node, i));
        rows.push(((node, i), at));
    }
    let r = ui.interact(Rect::from_center_size(at, vec2(12.0, 12.0) * z), ui.scope_id().with(("ctlport", node, i)), Sense::click());
    r.clone().on_hover_text(if followed { "Controlled by a signal (right-click: range, disconnect)" } else { "Control input: drag a wire here from the output of a node" });
    crate::slider::context_menu(&r, |ui| mod_menu_for(ui, node, i, range));
}

/// The menu of a control port: where it follows, the ranges, disconnect.
fn mod_menu_for(ui: &mut Ui, node: NodeId, i: usize, range: (f32, f32, bool, &'static str)) {
    let (min, max, log, unit) = range;
    let switch = unit == "switch";
    let cur = MOD_CUR.lock().iter().find(|m| m.node == node && m.param == i).copied();
    let Some(mut m) = cur else {
        ui.label(RichText::new(crate::lang::tr("Not controlled. Drag a wire from the output dot of a node onto this dot: a generator, a timeline out node, a low-pass ...")).small().weak());
        return;
    };
    let name = MOD_SRC.lock().iter().find(|s| s.0 == m.src).map(|s| s.1.clone()).unwrap_or_else(|| "?".into());
    ui.label(RichText::new(format!("Controlled by: {name}")).small().monospace());
    let mut ch = false;
    if switch {
        // the second entry while the signal is above the threshold; the hysteresis keeps it from flickering around it
        let (mut thr, mut hys) = (0.5 * (m.in_lo + m.in_hi), (m.in_hi - m.in_lo).abs());
        ui.label(RichText::new(crate::lang::tr("The second entry is chosen while the signal (full scale is 1) is above the threshold, the first while it is below")).small().weak());
        ui.horizontal(|ui| {
            ui.label(crate::lang::tr("Threshold"));
            ch |= ui.add(egui::DragValue::new(&mut thr).range(-4.0..=4.0).speed(0.005).max_decimals(3)).changed();
        });
        ui.horizontal(|ui| {
            ui.label(crate::lang::tr("Hysteresis"));
            ch |= ui.add(egui::DragValue::new(&mut hys).range(0.0..=4.0).speed(0.005).max_decimals(3)).on_hover_text(crate::lang::tr("Total width: it switches on above threshold + half of it and off below threshold - half of it. 0 is a plain threshold.")).changed();
        });
        let mut inv = m.out_lo > m.out_hi;
        ch |= ui.checkbox(&mut inv, crate::lang::tr("Inverted (the first entry above the threshold)")).changed();
        if ch {
            m.in_lo = thr - 0.5 * hys;
            m.in_hi = thr + 0.5 * hys;
            (m.out_lo, m.out_hi) = if inv { (1.0, 0.0) } else { (0.0, 1.0) };
            MOD_REQ.lock().push(ModReq::Set(m));
        }
        if crate::mapping::btn(ui, "Disconnect").clicked() {
            MOD_REQ.lock().push(ModReq::Clear(node, i));
            ui.close();
        }
        return;
    }
    ui.label(RichText::new(crate::lang::tr("Signal level (full scale is 1) -> value, outside the range it stops")).small().weak());
    ui.horizontal(|ui| {
        ui.label(crate::lang::tr("Signal at the lowest"));
        ch |= ui.add(egui::DragValue::new(&mut m.in_lo).range(-4.0..=4.0).speed(0.01).max_decimals(3)).changed();
    });
    ui.horizontal(|ui| {
        ui.label(crate::lang::tr("Signal at the highest"));
        ch |= ui.add(egui::DragValue::new(&mut m.in_hi).range(-4.0..=4.0).speed(0.01).max_decimals(3)).changed();
    });
    ui.horizontal(|ui| {
        ui.label(crate::lang::tr("Value at the lowest"));
        ch |= ui.add(egui::DragValue::new(&mut m.out_lo).range(min..=max).speed((max - min) * 0.002).suffix(unit)).changed();
    });
    ui.horizontal(|ui| {
        ui.label(crate::lang::tr("Value at the highest"));
        ch |= ui.add(egui::DragValue::new(&mut m.out_hi).range(min..=max).speed((max - min) * 0.002).suffix(unit)).changed();
    });
    if log {
        ch |= ui.checkbox(&mut m.log, crate::lang::tr("Logarithmic")).changed();
    }
    ui.label(RichText::new(crate::lang::tr("To smooth the signal first, put a Control low-pass node between.")).small().weak());
    if ch {
        MOD_REQ.lock().push(ModReq::Set(m));
    }
    if crate::mapping::btn(ui, "Disconnect").clicked() {
        MOD_REQ.lock().push(ModReq::Clear(node, i));
        ui.close();
    }
}

fn pick_port(cands: &[(NodeId, u8, Pos2)], p: Pos2, radius: f32) -> Option<(NodeId, u8)> {
    cands.iter().map(|(id, k, c)| (c.distance(p), *id, *k)).filter(|(d, _, _)| *d < radius).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, id, k)| (id, k))
}

fn bezier_at(p: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    pos2(a * p[0].x + b * p[1].x + c * p[2].x + d * p[3].x, a * p[0].y + b * p[1].y + c * p[2].y + d * p[3].y)
}

#[derive(Clone, Copy)]
enum WireDrag {
    /// dragging from an output dot (node, output)
    FromOut(NodeId, u8),
    /// dragging from an input dot (connect it to an output)
    FromIn(NodeId, u8),
}

/// What the search window will do with the node that is picked.
#[derive(Clone, Copy)]
enum Link {
    None,
    /// connect the new node's input to this output (node, output)
    FromOut(NodeId, u8),
    /// connect the new node's output to this input
    ToIn(NodeId, u8),
    /// put the node into this connection (to, port)
    Insert(NodeId, u8),
}

struct Search {
    /// where the node will be put (canvas coordinates)
    at: Pos2,
    text: String,
    sel: usize,
    link: Link,
    focused: bool,
}

#[derive(Clone)]
struct Entry {
    label: String,
    group: &'static str,
    keywords: String,
    kind: Kind,
    /// device id or track id to preset
    s: String,
    /// preset parameter 0 (generator number)
    p0: Option<f32>,
}

/// Both patches are the same down to the last bit of every value, so `encode` gives the same text for them (the converse does not
/// hold: a value that differs below the digits of the text is "different" here, which only costs an `encode`).
fn same_graph(a: &Graph, b: &Graph) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.edges == b.edges
        && a.mods == b.mods
        && a.nodes.iter().zip(&b.nodes).all(|(x, y)| {
            x.id == y.id
                && x.kind == y.kind
                && x.x.to_bits() == y.x.to_bits()
                && x.y.to_bits() == y.y.to_bits()
                && x.bypass == y.bypass
                && x.s == y.s
                && x.p.len() == y.p.len()
                && x.p.iter().zip(&y.p).all(|(u, v)| u.to_bits() == v.to_bits())
        })
}

/// Undo / redo of the patch: every settled change (no mouse button down) of the graph becomes one step, so a drag of a node or of a
/// slider is one step, not hundreds.
#[derive(Default)]
struct History {
    undo: Vec<Graph>,
    redo: Vec<Graph>,
    cur: Option<(String, Graph)>,
}

impl History {
    const MAX: usize = 200;

    /// Called every frame with the graph as it is now; `settled` = no drag in progress.
    fn track(&mut self, g: &Graph, settled: bool) {
        match &self.cur {
            None => self.cur = Some((g.encode(), g.clone())),
            Some((text, prev)) if settled => {
                // (nothing changed, which is nearly every frame: no need to write the whole patch as text to find out)
                if same_graph(prev, g) {
                    return;
                }
                let now = g.encode();
                if now != *text {
                    self.undo.push(prev.clone());
                    if self.undo.len() > Self::MAX {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                    self.cur = Some((now, g.clone()));
                }
            }
            _ => {}
        }
    }

    /// The graph before the last step (the current one becomes redo-able).
    fn back(&mut self, g: &Graph) -> Option<Graph> {
        let prev = self.undo.pop()?;
        // a change that is not settled yet belongs to the step that is undone
        self.redo.push(self.cur.as_ref().map_or_else(|| g.clone(), |c| c.1.clone()));
        self.cur = Some((prev.encode(), prev.clone()));
        Some(prev)
    }

    fn forward(&mut self, g: &Graph) -> Option<Graph> {
        let next = self.redo.pop()?;
        self.undo.push(self.cur.as_ref().map_or_else(|| g.clone(), |c| c.1.clone()));
        self.cur = Some((next.encode(), next.clone()));
        Some(next)
    }
}

pub struct Routing {
    eng: Option<Arc<Engine>>,
    g: Graph,
    pan: Vec2,
    sel: HashSet<NodeId>,
    sel_wire: Option<(NodeId, u8)>,
    wire: Option<WireDrag>,
    search: Option<Search>,
    dirty: bool,
    last_save: Instant,
    rate_pref: u32,
    fit_next: bool,
    note: String,
    /// last node drag: rubber band origin when selecting
    band: Option<Pos2>,
    /// name typed for saving, name of the routing that was loaded or saved last, delete waiting for a second click
    save_name: String,
    cur_name: String,
    del_arm: Option<String>,
    /// equaliser nodes whose graph is in drawing mode, and the band that a drag on empty space is moving
    eq_draw: HashSet<NodeId>,
    eq_new: Option<(NodeId, usize)>,
    /// zoom of the canvas (wheel)
    zoom: f32,
    /// size of every node in model units as drawn last frame, and a request to move overlapping nodes apart
    sizes: HashMap<NodeId, (f32, f32)>,
    tidy_pending: bool,
    /// file dialog for the impulse response of a convolution node
    ir_dlg: Option<(NodeId, crate::tools::dialogs::Pending)>,
    /// A/B: the active slot and both versions of the patch
    ab: Option<(usize, [Graph; 2])>,
    hist: History,
    search_hook: bool,
    /// commands from the input mappings (probe, A / B, undo, redo), taken when the window is drawn
    cmds: Vec<String>,
    /// listening probe: on, its volume (dB), whether the normal outputs go quiet, what it hovers over now and whether that is the body of a node
    probe_on: bool,
    /// counts the opened search windows (each gets its own area, so that it does not keep the size of the one before)
    search_n: u32,
    /// where the probe plays (see `Bus::probe_dev`)
    probe_dev: String,
    probe_db: f32,
    probe_solo: bool,
    probe_hover: Option<Probe>,
    probe_body: bool,
    /// settings and mutes that input bindings ask for (applied in `update`)
    bound_set: HashMap<(NodeId, usize), f32>,
    bound_mute: Vec<(NodeId, usize, Option<bool>)>,
    /// on / off settings of nodes that an input switches (None: toggle)
    bound_tog: Vec<(NodeId, usize, Option<bool>)>,
    /// devices for the first input / output node (true: input), from a request of the application
    bound_dev: Vec<(bool, String)>,
    /// DATS set-up requests: (input device, output device)
    bound_dats: Vec<(String, String)>,
}

type Vec2 = egui::Vec2;

/// A small patch for screenshots and tests (`VISUSCOPE_GRAPH=demo`): two generators and an input mixed,
/// equalised and compressed, then to the analysers and an output.
fn demo_graph(input: &str) -> Graph {
    let mut g = Graph::default();
    let inp = g.add(Kind::Input, 30.0, 20.0);
    g.node_mut(inp).unwrap().s = input.to_string();
    let g1 = g.add(Kind::Generator, 30.0, 250.0);
    let g2 = g.add(Kind::Generator, 30.0, 360.0);
    g.node_mut(g2).unwrap().p[0] = 2.0;
    let gain = g.add(Kind::Distortion, 290.0, 20.0);
    {
        let p = &mut g.node_mut(gain).unwrap().p;
        p[1] = DIST_HARMONIC as f32;
        p[4] = 0.0;
    }
    let mx = g.add(Kind::Mixer, 290.0, 190.0);
    let eq = g.add(Kind::Eq, 540.0, 60.0);
    let sp = g.add(Kind::Split, 920.0, 60.0);
    let mg = g.add(Kind::Merge, 1150.0, 60.0);
    let cp = g.add(Kind::Comp, 1150.0, 200.0);
    let an = g.add(Kind::Analyzer, 1430.0, 60.0);
    let out = g.add(Kind::Output, 1430.0, 170.0);
    let tl = g.add(Kind::TimelineRec, 1430.0, 310.0);
    for (a, b, p) in [(inp, gain, 0), (gain, mx, 2), (g1, mx, 0), (g2, mx, 1), (mx, eq, 0), (eq, sp, 0), (mg, cp, 0), (cp, an, 0), (cp, out, 0), (cp, tl, 0)] {
        let _ = g.connect(a, b, p);
    }
    let _ = g.connect_from(sp, 0, mg, 0);
    let _ = g.connect_from(sp, 1, mg, 1);
    {
        let p = &mut g.node_mut(eq).unwrap().p;
        p[2] = 4.0;
        // a notch and two more bands
        p.extend([5.0, 3200.0, -18.0, 6.0, 0.0, 6500.0, -5.0, 1.2, 0.0, 400.0, 3.5, 0.8]);
    }
    g
}

/// Folder of the named routings the user saved.
fn routings_dir() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("VISUSCOPE_ROUTINGS_DIR") {
        return p.into();
    }
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("routings")
}

/// A name that is safe to use as a file name (empty when nothing usable is left).
fn clean_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')')).take(80).collect();
    let n = n.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    // Windows treats these names as devices (CON.txt is the console): make them ordinary names
    let stem = n.split('.').next().unwrap_or("").to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem[3..].chars().all(|c| c.is_ascii_digit()));
    if reserved { format!("{n}_") } else { n }
}

fn list_routings() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(routings_dir())
        .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.path().file_stem().filter(|_| e.path().extension().is_some_and(|x| x == "txt")).map(|s| s.to_string_lossy().to_string())).collect())
        .unwrap_or_default();
    v.sort_by_key(|s| s.to_lowercase());
    v
}

fn save_routing(name: &str, g: &Graph) -> Result<(), String> {
    let n = clean_name(name);
    if n.is_empty() {
        return Err("give the routing a name".into());
    }
    std::fs::create_dir_all(routings_dir()).map_err(|e| e.to_string())?;
    // (a saved routing is user data: a crash in the middle of the write must not leave half a file behind)
    crate::fsutil::atomic_write(routings_dir().join(format!("{n}.txt")), g.encode()).map_err(|e| e.to_string())
}

fn load_routing(name: &str) -> Result<Graph, String> {
    let text = std::fs::read_to_string(routings_dir().join(format!("{}.txt", clean_name(name)))).map_err(|e| e.to_string())?;
    Graph::decode(&text).ok_or_else(|| "not a routing file".to_string())
}

fn file() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("VISUSCOPE_GRAPH_FILE") {
        return p.into();
    }
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("graph.txt")
}

impl Routing {
    pub fn new() -> Self {
        Self { eng: None, g: Graph::default(), pan: vec2(20.0, 20.0), sel: HashSet::new(), sel_wire: None, wire: None, search: None, dirty: false, last_save: Instant::now(), rate_pref: 0, fit_next: true, note: String::new(), band: None, save_name: String::new(), cur_name: String::new(), del_arm: None, eq_draw: HashSet::new(), eq_new: None, sizes: HashMap::new(), tidy_pending: true, ir_dlg: None, ab: None, hist: History::default(), search_hook: false, cmds: Vec::new(), probe_on: false, search_n: 0, probe_dev: String::new(), probe_db: -6.0, probe_solo: true, probe_hover: None, probe_body: false, bound_set: HashMap::new(), bound_mute: Vec::new(), bound_tog: Vec::new(), bound_dev: Vec::new(), bound_dats: Vec::new(), zoom: std::env::var("VISUSCOPE_ZOOM").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0) }
    }

    /// Values and mutes that input bindings set (a knob moves many times a frame: only the last value counts).
    fn apply_bound(&mut self) {
        let sets = std::mem::take(&mut self.bound_set);
        let mutes = std::mem::take(&mut self.bound_mute);
        let togs = std::mem::take(&mut self.bound_tog);
        let devs = std::mem::take(&mut self.bound_dev);
        let dats = std::mem::take(&mut self.bound_dats);
        let mut changed = false;
        for ((id, p), v) in sets {
            if let Some(n) = self.g.node_mut(id) {
                if let Some(d) = n.kind.info().params.get(p).filter(|_| p < n.p.len()) {
                    let v = v.clamp(d.min, d.max);
                    if n.p[p] != v {
                        n.p[p] = v;
                        changed = true;
                    }
                } else if n.kind == Kind::Mixer && (2..2 + MIX_MAX).contains(&p) && p < n.p.len() {
                    // the gains of the mixer inputs
                    let v = v.clamp(-60.0, 12.0);
                    if n.p[p] != v {
                        n.p[p] = v;
                        changed = true;
                    }
                }
            }
        }
        for (i, o) in dats {
            self.note = self.g.setup_dats(&i, &o);
            self.fit_next = true;
            changed = true;
        }
        for (input, dev) in devs {
            let kind = if input { Kind::Input } else { Kind::Output };
            if let Some(id) = self.g.nodes.iter().filter(|n| n.kind == kind).map(|n| n.id).min()
                && let Some(n) = self.g.node_mut(id) {
                    n.s = dev;
                    changed = true;
                }
        }
        for (id, p, state) in togs {
            if let Some(n) = self.g.node_mut(id)
                && n.kind.info().params.get(p).is_some_and(|d| d.choices.len() == 2) && p < n.p.len() {
                    let cur = n.p[p] >= 0.5;
                    n.p[p] = if state.unwrap_or(!cur) { 1.0 } else { 0.0 };
                    changed = true;
                }
        }
        for (id, k, state) in mutes {
            if let Some(n) = self.g.node_mut(id)
                && n.kind == Kind::Mixer && k < MIX_MAX && 2 + MIX_MAX + k < n.p.len() {
                    let cur = n.p[2 + MIX_MAX + k] >= 0.5;
                    n.p[2 + MIX_MAX + k] = if state.unwrap_or(!cur) { 1.0 } else { 0.0 };
                    changed = true;
                }
        }
        if changed {
            self.push();
        }
    }

    fn push(&mut self) {
        if let Some(e) = &self.eng {
            e.set_graph(self.g.clone());
        }
        *bus().graph_text.lock() = self.g.encode();
        self.dirty = true;
    }

    fn make_engine(&mut self, cx: &Ctx) {
        let e = Engine::new(cx.sh.clone(), cx.gens.clone());
        *e.default_in.lock() = cx.default_in.clone();
        e.rate_pref.store(self.rate_pref, std::sync::atomic::Ordering::Relaxed);
        self.g = std::fs::read_to_string(file()).ok().and_then(|t| Graph::decode(&t)).filter(|g| !g.nodes.is_empty()).unwrap_or_else(|| Graph::starter(&cx.default_in, None));
        if std::env::var("VISUSCOPE_GRAPH").as_deref() == Ok("demo") {
            self.g = demo_graph(&cx.default_in);
        }
        if std::env::var("VISUSCOPE_GRAPH").as_deref() == Ok("all") {
            // every kind of node once (with VISUSCOPE_NODECHECK=1: the overflow sweep before a release)
            let mut g = Graph::default();
            for (i, k) in Kind::ALL.iter().enumerate() {
                g.add(*k, 40.0 + (i % 6) as f32 * 330.0, 40.0 + (i / 6) as f32 * 360.0);
            }
            self.g = g;
        } else {
            self.tidy_pending = true;
        }
        e.set_graph(self.g.clone());
        *bus().graph_text.lock() = self.g.encode();
        self.eng = Some(e);
        self.fit_next = true;
    }

    fn save_file(&mut self) {
        if let Some(d) = file().parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = crate::fsutil::atomic_write(file(), self.g.encode());
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Rectangle of a node in model units (a guess for nodes that have not been drawn yet).
    fn node_rect(&self, n: &Node) -> Rect {
        let (w, h) = self.sizes.get(&n.id).copied().unwrap_or((node_w(n.kind), 150.0));
        Rect::from_min_size(pos2(n.x, n.y), vec2(w, h))
    }

    /// The first free place for a node of this kind at or below `at`: a new node must not land on another one.
    fn free_place(&self, at: Pos2, kind: Kind) -> Pos2 {
        let (w, h) = (node_w(kind), 170.0);
        let mut p = at;
        for step in 0..40 {
            let r = Rect::from_min_size(p, vec2(w, h)).expand(8.0);
            if !self.g.nodes.iter().any(|o| self.node_rect(o).intersects(r)) {
                return p;
            }
            // down first, then a column to the right
            p.y += 40.0;
            if step % 8 == 7 {
                p = pos2(p.x + w + 30.0, at.y);
            }
        }
        at
    }

    /// Move nodes that overlap apart (the one that is lower or more to the right gives way).
    fn tidy(&mut self) {
        for _ in 0..30 {
            let mut order: Vec<usize> = (0..self.g.nodes.len()).collect();
            order.sort_by(|&a, &b| (self.g.nodes[a].y, self.g.nodes[a].x).partial_cmp(&(self.g.nodes[b].y, self.g.nodes[b].x)).unwrap_or(std::cmp::Ordering::Equal));
            let mut moved = false;
            for (k, &i) in order.iter().enumerate() {
                for &j in &order[..k] {
                    let (ri, rj) = (self.node_rect(&self.g.nodes[i]), self.node_rect(&self.g.nodes[j]));
                    if !ri.expand(6.0).intersects(rj) {
                        continue;
                    }
                    let (dy, dx) = (rj.bottom() + 18.0 - ri.top(), rj.right() + 18.0 - ri.left());
                    let n = &mut self.g.nodes[i];
                    if dy <= dx { n.y += dy } else { n.x += dx }
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        self.dirty = true;
    }

    fn free_spot(&self) -> Pos2 {
        let right = self.g.nodes.iter().map(|n| n.x + node_w(n.kind)).fold(0.0, f32::max);
        pos2(right + 60.0, 60.0)
    }

    fn entries(&self, cx: &Ctx) -> Vec<Entry> {
        let e = |label: String, kind: Kind, s: String, p0: Option<f32>| {
            let info = kind.info();
            Entry { keywords: format!("{} {} {}", label, info.name, info.keywords).to_lowercase(), label: crate::lang::tr_s(&label), group: info.group, kind, s, p0 }
        };
        let mut v = Vec::new();
        for k in Kind::ALL {
            if k == Kind::Analyzer && self.g.nodes.iter().any(|n| n.kind == Kind::Analyzer) {
                continue;
            }
            v.push(e(k.name().to_string(), k, String::new(), None));
        }
        // devices, generators and tracks are chosen in the nodes themselves, not listed here
        let _ = cx;
        v
    }

    /// Bands from the EQ design window go into the selected equaliser node, else the first one; without any a new one is
    /// put in front of the first output.
    fn take_eq_bands(&mut self, mut bands: Vec<[f32; 3]>, preamp: f32) {
        bands.sort_by(|a, b| b[1].abs().total_cmp(&a[1].abs()));
        bands.truncate(EQ_MAX);
        bands.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if bands.is_empty() {
            return;
        }
        let existing = self.sel.iter().copied().find(|id| self.g.node(*id).is_some_and(|n| n.kind == Kind::Eq)).or_else(|| self.g.nodes.iter().find(|n| n.kind == Kind::Eq).map(|n| n.id));
        let id = match existing {
            Some(id) => id,
            None => {
                let out = self.g.nodes.iter().find(|n| n.kind == Kind::Output && self.g.edge_into(n.id, 0).is_some()).map(|n| n.id);
                let at = out.and_then(|o| self.g.node(o)).map(|n| pos2(n.x - 300.0, n.y)).unwrap_or_else(|| self.free_spot());
                let en = Entry { label: String::new(), group: "Effect", keywords: String::new(), kind: Kind::Eq, s: String::new(), p0: None };
                self.add_entry(&en, at, out.map_or(Link::None, |o| Link::Insert(o, 0)));
                match self.sel.iter().next().copied() {
                    Some(id) => id,
                    None => return,
                }
            }
        };
        if let Some(n) = self.g.node_mut(id) {
            n.p = bands.iter().flat_map(|b| [0.0, b[0].clamp(20.0, 20000.0), b[1].clamp(-24.0, 24.0), b[2].clamp(0.1, 10.0)]).collect();
            n.bypass = false;
        }
        self.sel.clear();
        self.sel.insert(id);
        self.tidy_pending = true;
        self.push();
        self.note = format!("EQ design loaded into an equaliser node ({} bands){}", bands.len(), if preamp < -0.05 { format!("; lower the level by {:.1} dB (Gain node) to keep the headroom", -preamp) } else { String::new() });
    }

    /// The crossover of the Speaker window as rows of nodes: every way starts at what feeds the first output and ends open.
    fn take_crossover(&mut self, chains: Vec<Vec<(u8, [f32; 3])>>) {
        let src = self.g.nodes.iter().find(|n| n.kind == Kind::Output && self.g.edge_into(n.id, 0).is_some()).and_then(|o| self.g.edge_into(o.id, 0).copied()).map(|e| (e.from, e.from_port));
        let base = self.free_spot();
        self.sel.clear();
        for (row, ch) in chains.iter().enumerate() {
            let mut prev = src;
            for (col, (k, vals)) in ch.iter().enumerate() {
                let kind = match k {
                    0 => Kind::Filter,
                    1 => Kind::Gain,
                    _ => Kind::Delay,
                };
                let id = self.g.add(kind, base.x + col as f32 * 240.0, base.y + row as f32 * 200.0);
                if let Some(n) = self.g.node_mut(id) {
                    for (i, v) in vals.iter().enumerate().take(n.p.len()) {
                        n.p[i] = *v;
                    }
                }
                if let Some((a, ap)) = prev {
                    let _ = self.g.connect_from(a, ap, id, 0);
                }
                prev = Some((id, 0));
                self.sel.insert(id);
            }
        }
        self.tidy_pending = true;
        self.push();
        self.note = format!("Crossover: {} way(s) added. Connect the end of each row to the output of its driver.", chains.len());
    }

    fn add_entry(&mut self, en: &Entry, at: Pos2, link: Link) {
        let at = self.free_place(at, en.kind);
        let id = self.g.add(en.kind, at.x, at.y);
        if let Some(n) = self.g.node_mut(id) {
            n.s = en.s.clone();
            if let Some(p0) = en.p0 {
                n.p[0] = p0;
            }
        }
        let info = en.kind.info();
        match link {
            Link::FromOut(a, ap) if info.inputs > 0 => {
                if let Err(m) = self.g.connect_from(a, ap, id, 0) {
                    self.note = m.into();
                }
            }
            Link::ToIn(b, port) if info.outputs > 0 => {
                if let Err(m) = self.g.connect(id, b, port) {
                    self.note = m.into();
                }
            }
            Link::Insert(to, port) => {
                if let Some(old) = self.g.edge_into(to, port).copied()
                    && info.inputs > 0 && info.outputs > 0 {
                        let _ = self.g.connect_from(old.from, old.from_port, id, 0);
                        let _ = self.g.connect(id, to, port);
                    }
            }
            _ => {}
        }
        self.sel.clear();
        self.sel.insert(id);
        self.push();
    }

    // ------------------------------------------------------------------ search window

    fn search_ui(&mut self, ui: &mut Ui, cx: &Ctx, canvas: Rect) {
        let Some(mut s) = self.search.take() else { return };
        if !s.focused {
            self.search_n = self.search_n.wrapping_add(1);
        }
        let entries = self.entries(cx);
        let words: Vec<String> = s.text.to_lowercase().split_whitespace().map(|w| w.to_string()).collect();
        let want_in = matches!(s.link, Link::FromOut(..) | Link::Insert(..));
        let want_out = matches!(s.link, Link::ToIn(..) | Link::Insert(..));
        let mut hits: Vec<(i32, &Entry)> = entries
            .iter()
            .filter(|e| {
                let i = e.kind.info();
                (!want_in || i.inputs > 0) && (!want_out || i.outputs > 0) && words.iter().all(|w| e.keywords.contains(w.as_str()) || e.group.to_lowercase().contains(w.as_str()))
            })
            .map(|e| {
                let l = e.label.to_lowercase();
                let score = words.iter().map(|w| if l.starts_with(w.as_str()) { 0 } else if l.contains(w.as_str()) { 1 } else { 2 }).sum::<i32>();
                (score, e)
            })
            .collect();
        hits.sort_by_key(|h| h.0);
        s.sel = s.sel.min(hits.len().saturating_sub(1));
        let (down, up, enter, esc) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        if down {
            s.sel = (s.sel + 1).min(hits.len().saturating_sub(1));
        }
        if up {
            s.sel = s.sel.saturating_sub(1);
        }
        let mut chosen: Option<Entry> = None;
        let screen = canvas.min + self.pan + s.at.to_vec2() * self.zoom;
        let browse_now = s.text.trim().is_empty();
        let win_w = if browse_now { (canvas.width() - 24.0).clamp(300.0, 880.0) } else { 280.0 };
        let win_h = if browse_now { 300.0 } else { 120.0 };
        let pos = pos2(screen.x.clamp(canvas.left() + 4.0, (canvas.right() - win_w - 20.0).max(canvas.left() + 4.0)), screen.y.clamp(canvas.top() + 4.0, (canvas.bottom() - win_h - 50.0).max(canvas.top() + 4.0)));
        let mut close = esc;
        // own area per mode, so the size remembered from the other mode never carries over
        let area = egui::Area::new(egui::Id::unique(("routing_search", self.search_n, browse_now))).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(win_w);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                if browse_now {
                    ui.set_min_height(win_h);
                }
                let te = ui.add(egui::TextEdit::singleline(&mut s.text).hint_text(crate::lang::tr("Search nodes: eq, input, compressor ...")).desired_width(240.0));
                if !s.focused {
                    te.request_focus();
                    s.focused = true;
                }
                if te.changed() {
                    s.sel = 0;
                }
                let browse = s.text.trim().is_empty();
                egui::ScrollArea::both().max_height(300.0).auto_shrink([!browse, true]).show(ui, |ui| {
                    if browse {
                        // nothing typed: the groups side by side, each with its nodes one below the other
                        let mut groups: Vec<&str> = Vec::new();
                        for (_, e) in hits.iter() {
                            if !groups.contains(&e.group) {
                                groups.push(e.group);
                            }
                        }
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            for g in &groups {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(*g).strong().color(Color32::from_rgb(150, 170, 200)));
                                    for (k, (_, e)) in hits.iter().enumerate().filter(|(_, (_, e))| e.group == *g) {
                                        let r = ui.selectable_label(k == s.sel, &e.label);
                                        if k == s.sel && (down || up) {
                                            r.scroll_to_me(None);
                                        }
                                        if r.clicked() {
                                            chosen = Some((*e).clone());
                                        }
                                    }
                                });
                            }
                        });
                    } else {
                        for (k, (_, e)) in hits.iter().enumerate().take(60) {
                            let r = ui.selectable_label(k == s.sel, &e.label);
                            if k == s.sel && (down || up) {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                chosen = Some((*e).clone());
                            }
                        }
                    }
                    if hits.is_empty() {
                        ui.label(RichText::new(crate::lang::tr("no match")).weak());
                    }
                });
            });
        });
        if enter
            && let Some((_, e)) = hits.get(s.sel) {
                chosen = Some((*e).clone());
            }
        // a click outside closes it
        if ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() {
            close = true;
        }
        if let Some(e) = chosen {
            let (at, link) = (s.at, s.link);
            self.add_entry(&e, at, link);
        } else if !close {
            self.search = Some(s);
        }
    }

    // ------------------------------------------------------------------ node bodies

    fn undo_graph(&mut self) {
        let g = self.g.clone();
        if let Some(prev) = self.hist.back(&g) {
            self.restore(prev);
        }
    }

    fn redo_graph(&mut self) {
        let g = self.g.clone();
        if let Some(next) = self.hist.forward(&g) {
            self.restore(next);
        }
    }

    fn restore(&mut self, g: Graph) {
        self.g = g;
        self.sel.clear();
        self.sel_wire = None;
        self.wire = None;
        self.push();
    }

    /// The signal under the pointer for the listening probe: an input or output dot, else the body of a node (its output, or its input when it has none).
    fn probe_target(&self, p: Pos2, sp: &impl Fn(Pos2) -> Pos2, z: f32) -> Option<(Probe, bool)> {
        let mut best: Option<(f32, Probe)> = None;
        for n in &self.g.nodes {
            for port in 0..n.inputs() {
                let d = sp(in_pos(n, port)).distance(p);
                if d <= 10.0 * z && best.is_none_or(|b| d < b.0) {
                    best = Some((d, Probe { node: n.id, input: true, port: port as u8 }));
                }
            }
            for port in 0..n.outputs() {
                let d = sp(out_pos(n, port)).distance(p);
                if d <= 10.0 * z && best.is_none_or(|b| d < b.0) {
                    best = Some((d, Probe { node: n.id, input: false, port: port as u8 }));
                }
            }
        }
        if let Some((_, pr)) = best {
            return Some((pr, false));
        }
        self.g.nodes.iter().rev().find_map(|n| {
            let (w, h) = self.sizes.get(&n.id).copied().unwrap_or((node_w(n.kind), 150.0));
            Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(w * z, h * z)).contains(p).then(|| (Probe { node: n.id, input: n.outputs() == 0, port: 0 }, true))
        })
    }

    /// Tell the engine where the probe listens; a new node is made sure to be processed (it may lead nowhere).
    fn set_probe(&mut self, t: Option<Probe>) {
        if t == self.probe_hover {
            return;
        }
        let new_node = t.map(|p| p.node) != self.probe_hover.map(|p| p.node);
        self.probe_hover = t;
        if let Some(e) = &self.eng {
            *e.probe.lock() = t;
            if new_node {
                e.rev.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// A / B: two versions of the patch, switched with one click (to hear what a node or a change does).
    fn ab_buttons(&mut self, ui: &mut Ui) {
        let cur = self.ab.as_ref().map_or(0, |a| a.0);
        let mut go: Option<usize> = None;
        ui.spacing_mut().item_spacing.x = 2.0;
        for k in 0..2 {
            let tip = if self.ab.is_none() { "A / B compare: click B to make a second version of this patch from here on; click A or B to switch between them" } else { "Switch to this version of the patch" };
            if ui.selectable_label(self.ab.is_some() && cur == k || self.ab.is_none() && k == 0, ["A", "B"][k]).on_hover_text(tip).clicked() && (cur != k || self.ab.is_none() && k == 1) {
                go = Some(k);
            }
        }
        if self.ab.is_some() {
            let other = 1 - cur;
            if crate::mapping::small_btn(ui, "copy").on_hover_text(format!("Copy this version over {}", ["A", "B"][other])).clicked() {
                let g = self.g.clone();
                if let Some(a) = self.ab.as_mut() {
                    a.1[other] = g;
                }
            }
            if crate::mapping::small_btn(ui, "x").on_hover_text(crate::lang::tr("Stop comparing and keep this version")).clicked() {
                self.ab = None;
            }
        }
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(k) = go {
            self.ab_go(k);
        }
    }

    /// Switch to version `k` of the patch (A = 0, B = 1); the first switch makes B a copy of the patch as it is.
    fn ab_go(&mut self, k: usize) {
        {
            let cur_g = self.g.clone();
            let (from, slots) = match self.ab.take() {
                Some((c, mut s)) => {
                    s[c] = cur_g;
                    (c, s)
                }
                None => (0, [cur_g.clone(), cur_g]),
            };
            let _ = from;
            self.g = slots[k].clone();
            self.ab = Some((k, slots));
            self.sel.clear();
            self.sel_wire = None;
            self.tidy_pending = true;
            self.push();
        }
    }

    /// Ready-made patches for a task: for people who start from nothing. The current patch is replaced (Ctrl+Z brings it back).
    fn presets_menu(&mut self, ui: &mut Ui, cx: &Ctx) {
        let mut pick: Option<super::presets::Preset> = None;
        crate::windows::sticky_menu(ui, crate::lang::tr("Presets"), |ui| {
            ui.set_min_width(320.0);
            ui.label(RichText::new(crate::lang::tr("Start from a ready-made patch (replaces the current one; Ctrl+Z brings it back)")).small().weak());
            for p in super::presets::Preset::ALL {
                let name = p.name().split(':').next().unwrap_or("");
                if crate::mapping::btn(ui, name).on_hover_text(p.about()).clicked() {
                    pick = Some(p);
                    ui.close();
                }
                ui.label(RichText::new(p.about()).small().weak());
            }
        });
        if let Some(p) = pick {
            // the devices are the defaults (empty): the input node picks the first input, the output node the default output
            let g = super::presets::build(p, "", 1, 2, "");
            if let Some(rec) = g.nodes.iter().find(|n| n.kind == Kind::TimelineRec) {
                bus().track_requests.lock().push((rec.id, "Input".into()));
            }
            self.g = g;
            self.sel.clear();
            self.sel_wire = None;
            self.tidy_pending = true;
            self.fit_next = true;
            self.push();
            self.note = format!("Patch: {}", p.name());
        }
        let _ = cx;
    }

    /// Save, load and delete named routings.
    fn routings_menu(&mut self, ui: &mut Ui, cx: &Ctx) {
        let title = if self.cur_name.is_empty() { "Routings".to_string() } else { format!("Routings: {}", self.cur_name) };
        crate::windows::sticky_menu(ui, title, |ui| {
            ui.set_min_width(250.0);
            ui.label(RichText::new(crate::lang::tr("Saved routings")).small().weak());
            let names = list_routings();
            if names.is_empty() {
                ui.label(RichText::new(crate::lang::tr("none yet: type a name below and save")).small().weak());
            }
            let (mut load, mut del): (Option<String>, Option<String>) = (None, None);
            egui::ScrollArea::vertical().max_height(260.0).auto_shrink([true, true]).show(ui, |ui| {
                for n in &names {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(*n == self.cur_name, n).on_hover_text(crate::lang::tr("Load this routing (replaces the current one)")).clicked() {
                            load = Some(n.clone());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let armed = self.del_arm.as_deref() == Some(n.as_str());
                            if crate::mapping::small_btn(ui, if armed { "delete?" } else { "x" }).on_hover_text(crate::lang::tr("Delete this saved routing (click twice)")).clicked() {
                                if armed {
                                    del = Some(n.clone());
                                } else {
                                    self.del_arm = Some(n.clone());
                                }
                            }
                        });
                    });
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.save_name).hint_text(crate::lang::tr("name")).desired_width(130.0));
                let name = clean_name(&self.save_name);
                let exists = names.iter().any(|n| n.eq_ignore_ascii_case(&name));
                if ui.add_enabled(!name.is_empty(), egui::Button::new(if exists { "Overwrite" } else { "Save" })).on_hover_text(crate::lang::tr("Save the current routing under this name")).clicked() {
                    match save_routing(&name, &self.g) {
                        Ok(()) => {
                            self.note = format!("Saved routing {name}");
                            self.cur_name = name;
                            self.save_name.clear();
                        }
                        Err(e) => self.note = format!("Cannot save: {e}"),
                    }
                }
            });
            if !self.cur_name.is_empty() && crate::mapping::btn(ui, format!("Save changes to {}", self.cur_name)).clicked() {
                match save_routing(&self.cur_name, &self.g) {
                    Ok(()) => self.note = format!("Saved routing {}", self.cur_name),
                    Err(e) => self.note = format!("Cannot save: {e}"),
                }
            }
            ui.separator();
            if crate::mapping::btn(ui, "New empty routing").clicked() {
                self.g = Graph::default();
                self.sel.clear();
                self.cur_name.clear();
                self.push();
                ui.close();
            }
            if crate::mapping::btn(ui, "Input -> analyzer (starter)").clicked() {
                self.g = Graph::starter(&cx.default_in, None);
                self.sel.clear();
                self.cur_name.clear();
                self.fit_next = true;
                self.push();
                ui.close();
            }
            if let Some(n) = load {
                match load_routing(&n) {
                    Ok(g) => {
                        self.g = g;
                        self.sel.clear();
                        self.sel_wire = None;
                        self.cur_name = n.clone();
                        self.fit_next = true;
                        self.tidy_pending = true;
                        self.note = format!("Loaded routing {n}");
                        self.push();
                        ui.close();
                    }
                    Err(e) => self.note = format!("Cannot load {n}: {e}"),
                }
            }
            if let Some(n) = del {
                let _ = std::fs::remove_file(routings_dir().join(format!("{}.txt", clean_name(&n))));
                self.del_arm = None;
                if self.cur_name == n {
                    self.cur_name.clear();
                }
            }
        });
    }

    // ------------------------------------------------------------------ canvas

}

impl Tool for Routing {
    fn command(&mut self, c: &str) {
        if matches!(c, "probe" | "ab" | "undo" | "redo") && self.cmds.len() < 16 {
            self.cmds.push(c.to_string());
        }
        // settings from an input binding: "setp<tab>node<tab>parameter<tab>value" (only the latest value of each is kept),
        // "mute<tab>node<tab>input<tab>t (switch) or 1 / 0 (on / off)"
        let f: Vec<&str> = c.split('\t').collect();
        match f.as_slice() {
            ["setp", n, p, v] => {
                if let (Ok(n), Ok(p), Ok(v)) = (n.parse::<NodeId>(), p.parse::<usize>(), v.parse::<f32>())
                    && v.is_finite() {
                        self.bound_set.insert((n, p), v);
                    }
            }
            ["setdev", which, id] => {
                if self.bound_dev.len() < 8 {
                    self.bound_dev.push((*which == "in", id.to_string()));
                }
            }
            ["datsnodes", i, o] => {
                if self.bound_dats.len() < 4 {
                    self.bound_dats.push((i.to_string(), o.to_string()));
                }
            }
            ["togp", n, p, v] => {
                if let (Ok(n), Ok(p)) = (n.parse::<NodeId>(), p.parse::<usize>()) {
                    let state = if *v == "t" { None } else { Some(v.parse::<f32>().unwrap_or(0.0) >= 0.5) };
                    if self.bound_tog.len() < 64 {
                        self.bound_tog.push((n, p, state));
                    }
                }
            }
            ["mute", n, k, v] => {
                if let (Ok(n), Ok(k)) = (n.parse::<NodeId>(), k.parse::<usize>()) {
                    let state = if *v == "t" { None } else { Some(v.parse::<f32>().unwrap_or(0.0) >= 0.5) };
                    if self.bound_mute.len() < 64 {
                        self.bound_mute.push((n, k, state));
                    }
                }
            }
            _ => {}
        }
    }

    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        // commands wait for the window to be drawn; with the graph off there is nothing for them to act on
        if !cx.graph_on {
            self.cmds.clear();
        }
        if cx.graph_on && self.eng.is_none() {
            self.make_engine(cx);
        }
        let Some(e) = self.eng.clone() else { return };
        use std::sync::atomic::Ordering::Relaxed;
        self.apply_bound();
        e.active.store(cx.graph_on, Relaxed);
        if !cx.graph_on {
            return;
        }
        *e.default_in.lock() = cx.default_in.clone();
        e.rate_pref.store(self.rate_pref, Relaxed);
        // timeline tracks that want a record node
        let reqs: Vec<(u32, String)> = std::mem::take(&mut *bus().rec_requests.lock());
        for (tid, name) in reqs {
            if !self.g.nodes.iter().any(|n| n.kind == Kind::TimelineRec && n.s == tid.to_string()) {
                let at = self.free_spot();
                let id = self.g.add(Kind::TimelineRec, at.x, at.y);
                if let Some(n) = self.g.node_mut(id) {
                    n.s = tid.to_string();
                }
                self.note = format!("Added a record node for {name}: connect something to it");
                self.push();
            }
        }
        if let Some(e) = &self.eng {
            e.probe_gain.store(db_to_lin(self.probe_db).to_bits(), Relaxed);
            e.probe_solo.store(self.probe_solo, Relaxed);
        }
        if !self.probe_on {
            self.set_probe(None);
        }
        *bus().probe_dev.lock() = self.probe_dev.clone();
        *bus().first_out.lock() = self.g.nodes.iter().find(|n| n.kind == Kind::Output).map(|n| n.s.clone()).unwrap_or_default();
        // tracks the timeline made for record nodes that asked for one
        let made = std::mem::take(&mut *bus().node_tracks.lock());
        let mut tracked = false;
        for (node, tid) in made {
            if let Some(n) = self.g.node_mut(node) {
                n.s = tid.to_string();
                tracked = true;
            }
        }
        if tracked {
            self.note = "A new track was added to the timeline for this node: it is armed".into();
            self.push();
        }
        let restore = bus().graph_restore.lock().take();
        if let Some(g) = restore.and_then(|t| Graph::decode(&t)).filter(|g| !g.nodes.is_empty()) {
            self.g = g;
            self.sel.clear();
            self.sel_wire = None;
            self.ab = None;
            self.tidy_pending = true;
            self.push();
        }
        let sent = bus().eq_send.lock().take();
        if let Some((bands, preamp)) = sent {
            self.take_eq_bands(bands, preamp);
        }
        let xo = bus().xo_send.lock().take();
        if let Some(chains) = xo {
            self.take_crossover(chains);
        }
        if let Some((id, dlg)) = &self.ir_dlg
            && let Some(res) = dlg.take() {
                let id = *id;
                self.ir_dlg = None;
                if let (Some(path), Some(n)) = (res, self.g.node_mut(id)) {
                    n.s = path.to_string_lossy().to_string();
                    self.push();
                }
            }
        if self.dirty && self.last_save.elapsed().as_secs_f32() > 1.0 {
            self.save_file();
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        if !cx.graph_on {
            ui.label(crate::lang::tr("The routing graph is off. Turn it on in Options."));
            return;
        }
        for c in std::mem::take(&mut self.cmds) {
            match c.as_str() {
                "probe" => self.probe_on = !self.probe_on,
                "ab" => {
                    let next = self.ab.as_ref().map_or(1, |a| 1 - a.0);
                    self.ab_go(next);
                }
                "undo" => self.undo_graph(),
                "redo" => self.redo_graph(),
                _ => {}
            }
        }
        // what the menus of the parameters asked for (a parameter that follows a signal), and what they may choose from
        let reqs: Vec<ModReq> = std::mem::take(&mut *MOD_REQ.lock());
        if !reqs.is_empty() {
            for r in reqs {
                match r {
                    ModReq::Set(m) => {
                        self.g.set_modulation(m);
                    }
                    ModReq::Clear(n, p) => self.g.clear_modulation(n, p),
                }
            }
            self.push();
        }
        *MOD_SRC.lock() = self.g.nodes.iter().filter(|n| n.outputs() > 0).map(|n| (n.id, format!("{} #{}", n.kind.info().name, n.id))).collect();
        *MOD_CUR.lock() = self.g.mods.clone();
        MOD_DRAG.store(matches!(self.wire, Some(WireDrag::FromOut(..))), std::sync::atomic::Ordering::Relaxed);
        MOD_ROWS.lock().retain(|r| self.g.node(r.0 .0).is_some());
        // the followed values move: keep drawing
        if !self.g.mods.is_empty() {
            ui.ctx().request_repaint();
        }
        // test hook: VISUSCOPE_SEARCH=1 opens the add-node menu once (screenshots)
        if std::env::var_os("VISUSCOPE_SEARCH").is_some() && !self.search_hook {
            self.search_hook = true;
            self.search = Some(Search { at: pos2(120.0, 40.0), text: String::new(), sel: 0, link: Link::None, focused: false });
        }
        // every settled change of the patch is one undo step
        let settled = !ui.input(|i| i.pointer.any_down());
        self.hist.track(&self.g, settled);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if crate::mapping::btn(ui, "+ Node").on_hover_text(crate::lang::tr("Search all nodes (Space)")).clicked() {
                self.search = Some(Search { at: pos2(100.0, 100.0) - self.pan.to_pos2().to_vec2(), text: String::new(), sel: 0, link: Link::None, focused: false });
            }
            if crate::mapping::btn(ui, "Fit").on_hover_text(crate::lang::tr("Show all nodes (F)")).clicked() {
                self.fit_next = true;
            }
            if crate::mapping::btn(ui, "Tidy").on_hover_text(crate::lang::tr("Move nodes apart that overlap")).clicked() {
                self.tidy_pending = true;
            }
            self.presets_menu(ui, cx);
            self.routings_menu(ui, cx);
            if ui.add_enabled(!self.hist.undo.is_empty(), egui::Button::new(crate::lang::tr("Undo")).small()).on_hover_text(crate::lang::tr("Undo the last change of the patch (Ctrl+Z)")).clicked() {
                self.undo_graph();
            }
            if ui.add_enabled(!self.hist.redo.is_empty(), egui::Button::new(crate::lang::tr("Redo")).small()).on_hover_text(crate::lang::tr("Redo (Ctrl+Y or Ctrl+Shift+Z)")).clicked() {
                self.redo_graph();
            }
            self.ab_buttons(ui);
            ui.toggle_value(&mut self.probe_on, crate::lang::tr("Probe")).on_hover_text(crate::lang::tr("Listening probe (P): hover over a node, or over one of its input or output dots, to hear the signal there"));
            if self.probe_on {
                ui.add(egui::DragValue::new(&mut self.probe_db).range(-48.0..=6.0).speed(0.2).suffix(crate::lang::tr(" dB"))).on_hover_text(crate::lang::tr("Volume of the probe"));
                // where the probe plays
                let devs: Vec<(String, String)> = cx.out_devs.iter().filter(|d| !d.id.is_empty()).map(|d| (d.id.clone(), short_label(&d.label))).collect();
                let cur = match self.probe_dev.as_str() {
                    "" => "first output node".to_string(),
                    super::engine::PROBE_DEFAULT => "default output".to_string(),
                    d => devs.iter().find(|x| x.0 == d).map(|x| x.1.clone()).unwrap_or_else(|| "device missing".into()),
                };
                egui::ComboBox::from_id_salt("probe_dev").width(130.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.probe_dev, String::new(), crate::lang::tr("Same as the first output node"));
                    ui.selectable_value(&mut self.probe_dev, super::engine::PROBE_DEFAULT.to_string(), crate::lang::tr("Default output"));
                    for (id, label) in &devs {
                        ui.selectable_value(&mut self.probe_dev, id.clone(), label);
                    }
                }).response.on_hover_text(crate::lang::tr("Where the probe plays"));
                ui.checkbox(&mut self.probe_solo, crate::lang::tr("mute same device")).on_hover_text(crate::lang::tr("An output node that plays to the same device as the probe is silent while the probe listens (the other outputs go on)"));
                let name = self.probe_hover.and_then(|p| self.g.node(p.node).map(|n| format!("{} {}", n.kind.name(), if p.input { format!("in {}", p.port + 1) } else { format!("out {}", p.port + 1) })));
                ui.label(RichText::new(name.map(|n| format!("listening: {n}")).unwrap_or_else(|| "hover a node or a dot".into())).small().color(Color32::from_rgb(120, 220, 230)));
            }
            if crate::mapping::small_btn(ui, format!("{:.0} %", self.zoom * 100.0)).on_hover_text(crate::lang::tr("Zoom of the node window: mouse wheel, or pinch on a touchpad (click: back to 100 %)")).clicked() {
                self.zoom = 1.0;
            }
            ui.label(RichText::new(crate::lang::tr("Rate")).small().weak());
            let cur = if self.rate_pref == 0 { "auto".to_string() } else { format!("{} Hz", self.rate_pref) };
            egui::ComboBox::from_id_salt("graph_rate").width(80.0).selected_text(cur).show_ui(ui, |ui| {
                for r in [0u32, 44100, 48000, 88200, 96000, 176400, 192000] {
                    if ui.selectable_value(&mut self.rate_pref, r, if r == 0 { "auto (first input)".to_string() } else { format!("{r} Hz") }).clicked()
                        && let Some(e) = &self.eng {
                            e.rev.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                }
            });
            if let Some(e) = &self.eng {
                ui.label(RichText::new(e.status.lock().clone()).small().weak());
            }
            if !self.note.is_empty() {
                ui.label(RichText::new(&self.note).small().color(Color32::from_rgb(255, 190, 90)));
            }
        });
        // test hook: VISUSCOPE_EQ_DRAW=1 starts every equaliser in drawing mode (screenshots)
        if std::env::var_os("VISUSCOPE_EQ_DRAW").is_some() {
            let eqs: Vec<NodeId> = self.g.nodes.iter().filter(|n| n.kind == Kind::Eq).map(|n| n.id).collect();
            self.eq_draw.extend(eqs);
        }
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        self.canvas(ui, cx, rect);
        // hint when empty
        if self.g.nodes.is_empty() {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Press Space to add a node", FontId::proportional(15.0), Color32::from_gray(110));
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("rate", self.rate_pref);
        o.put("zoom", self.zoom);
        o.put("probe_dev", self.probe_dev.replace(['\n', '\r'], " "));
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("rate", &mut self.rate_pref);
        i.set("zoom", &mut self.zoom);
        i.set("probe_dev", &mut self.probe_dev);
        self.zoom = if self.zoom.is_finite() { self.zoom.clamp(0.4, 1.8) } else { 1.0 };
    }
}

#[allow(dead_code)]
fn _unused(_: f32) -> f32 {
    db_to_lin(0.0)
}

#[cfg(test)]
mod tests;
