//! The node editor window ("Routing"): drag from an output dot to an input dot to connect, drag
//! an input away to move or drop the connection, Space opens the node search, right-click a wire to
//! insert a node into it. Everything the engine runs comes from the graph edited here.

use super::dsp::{db_to_lin, EqUnit};
use super::engine::{bus, Engine};
use super::model::*;
use crate::tools::daw::capture::{short_label, DevEx};
use crate::tools::{Ctx, SettingsIn, SettingsOut, Tool};
use eframe::egui::{self, epaint::CubicBezierShape, pos2, vec2, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, UiBuilder};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

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
    if matches!(n.kind, Kind::Mixer | Kind::Merge) {
        // a mixer has the "Inputs" and "Master" rows above its inputs
        let above = if n.kind == Kind::Mixer { 2.0 } else { 0.0 };
        pos2(n.x, n.y + HDR + 11.0 + (above + port as f32) * ROW + ROW * 0.5)
    } else {
        pos2(n.x, n.y + HDR * 0.5)
    }
}

fn out_pos(n: &Node, port: usize) -> Pos2 {
    if n.kind == Kind::Split {
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
    /// equaliser nodes whose graph is in drawing mode, the band that a drag on empty space is moving, and whether
    /// the pointer was over such a graph (the wheel then edits the band instead of panning the canvas)
    eq_draw: HashSet<NodeId>,
    eq_new: Option<(NodeId, usize)>,
    eq_over: bool,
    eq_over_prev: bool,
    /// zoom of the canvas (Shift + wheel)
    zoom: f32,
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
    let gain = g.add(Kind::Harmonics, 290.0, 20.0);
    let mx = g.add(Kind::Mixer, 290.0, 190.0);
    let eq = g.add(Kind::Eq, 540.0, 60.0);
    let sp = g.add(Kind::Split, 820.0, 60.0);
    let mg = g.add(Kind::Merge, 1050.0, 60.0);
    let cp = g.add(Kind::Comp, 1050.0, 200.0);
    let an = g.add(Kind::Analyzer, 1330.0, 60.0);
    let out = g.add(Kind::Output, 1330.0, 170.0);
    let tl = g.add(Kind::TimelineRec, 1330.0, 310.0);
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
    let n: String = s.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')')).collect();
    n.trim().trim_matches('.').to_string()
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
    std::fs::write(routings_dir().join(format!("{n}.txt")), g.encode()).map_err(|e| e.to_string())
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
        Self { eng: None, g: Graph::default(), pan: vec2(20.0, 20.0), sel: HashSet::new(), sel_wire: None, wire: None, search: None, dirty: false, last_save: Instant::now(), rate_pref: 0, fit_next: true, note: String::new(), band: None, save_name: String::new(), cur_name: String::new(), del_arm: None, eq_draw: HashSet::new(), eq_new: None, eq_over: false, eq_over_prev: false, zoom: std::env::var("VISUSCOPE_ZOOM").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0) }
    }

    fn push(&mut self) {
        if let Some(e) = &self.eng {
            e.set_graph(self.g.clone());
        }
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
        e.set_graph(self.g.clone());
        self.eng = Some(e);
        self.fit_next = true;
    }

    fn save_file(&mut self) {
        if let Some(d) = file().parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(file(), self.g.encode());
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn free_spot(&self) -> Pos2 {
        let right = self.g.nodes.iter().map(|n| n.x + node_w(n.kind)).fold(0.0, f32::max);
        pos2(right + 60.0, 60.0)
    }

    fn entries(&self, cx: &Ctx) -> Vec<Entry> {
        let e = |label: String, kind: Kind, s: String, p0: Option<f32>| {
            let info = kind.info();
            Entry { keywords: format!("{} {} {}", label, info.name, info.keywords).to_lowercase(), label, group: info.group, kind, s, p0 }
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

    fn add_entry(&mut self, en: &Entry, at: Pos2, link: Link) {
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
                if let Some(old) = self.g.edge_into(to, port).copied() {
                    if info.inputs > 0 && info.outputs > 0 {
                        let _ = self.g.connect_from(old.from, old.from_port, id, 0);
                        let _ = self.g.connect(id, to, port);
                    }
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
        let pos = pos2(screen.x.clamp(canvas.left() + 4.0, (canvas.right() - 300.0).max(canvas.left() + 4.0)), screen.y.clamp(canvas.top() + 4.0, (canvas.bottom() - 260.0).max(canvas.top() + 4.0)));
        let mut close = esc;
        let area = egui::Area::new(egui::Id::new("routing_search")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(290.0);
                let te = ui.add(egui::TextEdit::singleline(&mut s.text).hint_text("Search nodes: eq, input, compressor ...").desired_width(f32::INFINITY));
                if !s.focused {
                    te.request_focus();
                    s.focused = true;
                }
                if te.changed() {
                    s.sel = 0;
                }
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    let mut group = "";
                    for (k, (_, e)) in hits.iter().enumerate().take(60) {
                        if e.group != group && s.text.trim().is_empty() {
                            group = e.group;
                            ui.label(RichText::new(group).small().weak());
                        }
                        let r = ui.selectable_label(k == s.sel, &e.label);
                        if k == s.sel && (down || up) {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some((*e).clone());
                        }
                    }
                    if hits.is_empty() {
                        ui.label(RichText::new("no match").weak());
                    }
                });
            });
        });
        if enter {
            if let Some((_, e)) = hits.get(s.sel) {
                chosen = Some((*e).clone());
            }
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

    fn device_combo(&self, ui: &mut Ui, n: &mut Node, id_salt: &str) -> bool {
        let Some(e) = &self.eng else { return false };
        let devs: Vec<DevEx> = e.devs.lock().clone();
        let cur = if n.s.is_empty() {
            let d = e.default_in.lock().clone();
            devs.iter().find(|x| x.id == d).map(|x| format!("{} (default)", short_label(&x.label))).unwrap_or_else(|| "default input".into())
        } else {
            devs.iter().find(|x| x.id == n.s).map(|x| short_label(&x.label)).unwrap_or_else(|| "device missing".into())
        };
        let mut changed = false;
        egui::ComboBox::from_id_salt((id_salt, n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
            for d in &devs {
                if ui.selectable_label(n.s == d.id, format!("{}  ({} ch, {} Hz)", short_label(&d.label), d.ch, d.sr)).clicked() {
                    n.s = d.id.clone();
                    changed = true;
                }
            }
        });
        changed
    }

    fn param_row(ui: &mut Ui, n: &mut Node, i: usize, label_w: f32) -> bool {
        let z = ui.spacing().interact_size.y / 16.0;
        let defs = n.kind.info().params;
        let Some(d) = defs.get(i) else { return false };
        if n.p.len() <= i {
            n.p.resize(i + 1, d.def);
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // right-click the name to put the value back to its default
            let name = ui.add_sized(vec2(label_w * z, 16.0 * z), egui::Label::new(RichText::new(d.name).small().color(Color32::from_rgb(160, 172, 190))).truncate().sense(Sense::click()));
            if name.on_hover_text("Right-click: reset to the default").secondary_clicked() {
                n.p[i] = d.def;
                changed = true;
            }
            if !d.choices.is_empty() {
                let cur = d.choices.get(n.p[i].round() as usize).copied().unwrap_or("?");
                egui::ComboBox::from_id_salt(("pc", n.id, i)).width(ui.available_width() - 4.0).selected_text(cur).show_ui(ui, |ui| {
                    for (k, c) in d.choices.iter().enumerate() {
                        if ui.selectable_label(n.p[i].round() as usize == k, *c).clicked() {
                            n.p[i] = k as f32;
                            changed = true;
                        }
                    }
                });
            } else {
                ui.spacing_mut().slider_width = (ui.available_width() - 62.0 * z).max(40.0 * z);
                let int = d.unit.is_empty() && d.max - d.min >= 1.0 && d.min.fract() == 0.0 && d.def.fract() == 0.0;
                let mut s = egui::Slider::new(&mut n.p[i], d.min..=d.max).suffix(d.unit).logarithmic(d.log).clamping(egui::SliderClamping::Always);
                if int {
                    s = s.step_by(1.0).max_decimals(0);
                } else if d.unit == " Hz" || d.unit == " ms" {
                    s = s.max_decimals(if d.log { 0 } else { 1 });
                } else {
                    s = s.max_decimals(1);
                }
                changed |= ui.add(s).changed();
            }
        });
        changed
    }

    /// Output nodes whose device is also captured by an input node (a loopback of it): a feedback loop.
    fn feedback_nodes(&self, cx: &Ctx) -> HashSet<NodeId> {
        let default_out = cx.out_devs.first().map(|d| d.id.clone()).unwrap_or_default();
        let outs: Vec<(NodeId, String)> = self.g.nodes.iter().filter(|n| n.kind == Kind::Output).map(|n| (n.id, if n.s.is_empty() { default_out.clone() } else { n.s.clone() })).collect();
        let mut bad = HashSet::new();
        for i in self.g.nodes.iter().filter(|n| n.kind == Kind::Input) {
            let dev = if i.s.is_empty() { cx.default_in.clone() } else { i.s.clone() };
            // only a path from the input to the output closes the loop
            for (o, od) in &outs {
                if *od == dev && !dev.is_empty() && self.g.reaches(i.id, *o) {
                    bad.insert(i.id);
                    bad.insert(*o);
                }
            }
        }
        bad
    }

    /// Widgets of one node; returns true when something changed.
    fn body(&mut self, ui: &mut Ui, cx: &Ctx, n: &mut Node, feedback: bool) -> bool {
        let z = self.zoom;
        ui.spacing_mut().item_spacing.y = 3.0 * z;
        ui.spacing_mut().interact_size.y = 16.0 * z;
        let mut ch = false;
        if feedback {
            ui.label(RichText::new("feedback: this output is captured by the input that feeds it").small().color(Color32::from_rgb(255, 120, 100)));
        }
        let status = self.eng.as_ref().and_then(|e| e.node_status.lock().get(&n.id).cloned());
        let meter = self.eng.as_ref().map(|e| e.meter(n.id)).unwrap_or_default();
        match n.kind {
            Kind::Input => {
                ch |= self.device_combo(ui, n, "indev");
                let ch_n = self.eng.as_ref().and_then(|e| {
                    let d = e.devs.lock();
                    let cur = if n.s.is_empty() { e.default_in.lock().clone() } else { n.s.clone() };
                    d.iter().find(|x| x.id == cur).map(|x| x.ch)
                });
                for i in 0..2 {
                    if let Some(c) = ch_n {
                        n.p[i] = n.p[i].min(c as f32);
                    }
                    ch |= Self::param_row(ui, n, i, 52.0);
                }
                ch |= Self::param_row(ui, n, 2, 52.0);
                ch |= Self::param_row(ui, n, 3, 52.0);
                if let Some(s) = status {
                    let bad = s.contains("not found") || s.contains("unsupported") || s.contains("reconnect") || s.contains("no device");
                    ui.label(RichText::new(s).small().color(if bad { Color32::from_rgb(255, 160, 90) } else { Color32::from_gray(120) }));
                }
            }
            Kind::Generator => {
                let n_gen = cx.gens.lock().len().max(1);
                let gi = (n.p[0].round() as usize).clamp(1, n_gen);
                let desc = cx.gens.lock().get(gi - 1).map(|p| format!("{} {:.0} Hz, {:.0} dBFS{}", p.wave.name(), p.freq, p.level_db[0], if p.on { "" } else { "  (off)" })).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Generator").small().color(Color32::from_rgb(160, 172, 190)));
                    egui::ComboBox::from_id_salt(("gsel", n.id)).selected_text(format!("Gen {gi}")).width(70.0 * z).show_ui(ui, |ui| {
                        for k in 1..=n_gen {
                            if ui.selectable_label(gi == k, format!("Gen {k}")).clicked() {
                                n.p[0] = k as f32;
                                ch = true;
                            }
                        }
                    });
                });
                ui.label(RichText::new(desc).small().weak());
                ui.label(RichText::new("set it in the Generator window (tabs)").small().weak());
            }
            Kind::TimelineOut | Kind::TimelineRec => {
                let tracks = bus().tracks.lock().clone();
                let master_ok = n.kind == Kind::TimelineOut;
                let cur = if n.s.is_empty() {
                    if master_ok { "Master mix".to_string() } else { "choose a track".to_string() }
                } else {
                    tracks.iter().find(|t| t.0.to_string() == n.s).map(|t| t.1.clone()).unwrap_or_else(|| "track missing".into())
                };
                egui::ComboBox::from_id_salt(("tsel", n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    if master_ok && ui.selectable_label(n.s.is_empty(), "Master mix").clicked() {
                        n.s.clear();
                        ch = true;
                    }
                    for (id, name) in &tracks {
                        if ui.selectable_label(n.s == id.to_string(), name).clicked() {
                            n.s = id.to_string();
                            ch = true;
                        }
                    }
                });
                ui.label(RichText::new(if master_ok { "play the timeline to hear / see it" } else { "arm this track to record it" }).small().weak());
            }
            Kind::Split | Kind::Merge => {
                // the rows line up with the dots: the left channel on top, the right one below
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0 * z);
                for (k, name) in ["Left", "Right"].into_iter().enumerate() {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                    let col = if k == 0 { Color32::from_rgb(70, 200, 255) } else { Color32::from_rgb(255, 100, 160) };
                    let (al, x) = if n.kind == Kind::Split { (Align2::RIGHT_CENTER, row.right() - 8.0) } else { (Align2::LEFT_CENTER, row.left() + 8.0) };
                    ui.painter().text(pos2(x, row.center().y), al, if n.kind == Kind::Split { format!("{name} channel out") } else { format!("{name} channel in") }, FontId::proportional(11.0 * z), col);
                }
                ui.label(RichText::new(if n.kind == Kind::Split { "each output carries one channel on both sides" } else { "each input is made mono; first = left, second = right" }).small().weak());
            }
            Kind::Output => {
                let cur = if n.s.is_empty() { cx.out_devs.first().map(|d| format!("{} (default)", d.label)).unwrap_or_else(|| "default output".into()) } else { cx.out_devs.iter().find(|d| d.id == n.s).map(|d| d.label.clone()).unwrap_or_else(|| "device missing".into()) };
                egui::ComboBox::from_id_salt(("outdev", n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    for (i, d) in cx.out_devs.iter().enumerate() {
                        let id = if i == 0 { String::new() } else { d.id.clone() };
                        if ui.selectable_label(n.s == id, &d.label).clicked() {
                            n.s = id;
                            ch = true;
                        }
                    }
                });
                ch |= Self::param_row(ui, n, 0, 40.0);
                ch |= Self::param_row(ui, n, 1, 40.0);
                if let Some(s) = status {
                    ui.label(RichText::new(s).small().color(Color32::from_gray(120)));
                }
            }
            Kind::Analyzer => {
                ui.label(RichText::new("feeds spectrum, scope, loudness ...").small().weak());
            }
            Kind::Eq => {
                let fs = self.eng.as_ref().map_or(48000.0, |e| e.rate.load(std::sync::atomic::Ordering::Relaxed) as f32);
                let drawing = self.eq_draw.contains(&n.id);
                let bands0 = (n.p.len() / 4).clamp(1, EQ_MAX);
                n.p.resize(bands0 * 4, 0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let mut d = drawing;
                    if ui.toggle_value(&mut d, "Draw").on_hover_text("Edit with the mouse in the graph: drag a dot (frequency and level), wheel over a dot = width, click empty space = new band, right-click a dot = remove it").changed() {
                        if d {
                            self.eq_draw.insert(n.id);
                        } else {
                            self.eq_draw.remove(&n.id);
                        }
                    }
                    if ui.add_enabled(n.p.len() / 4 < EQ_MAX, egui::Button::new("+ band")).on_hover_text("Add a band (up to 12)").clicked() {
                        n.p.extend([0.0, 1000.0, 0.0, 1.0]);
                        ch = true;
                    }
                    ui.label(RichText::new(format!("{} bands", n.p.len() / 4)).small().weak());
                });
                // response curve; with Draw on it is taller and the bands are dots to drag
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, if drawing { 150.0 * z } else { 54.0 * z }), if drawing { Sense::click_and_drag() } else { Sense::hover() });
                let range = 24.0f32;
                let xf = |f: f32| r.left() + ((f.max(20.0) / 20.0).log10() / 3.0).clamp(0.0, 1.0) * r.width();
                let fx = |x: f32| 20.0 * 10f32.powf(((x - r.left()) / r.width()).clamp(0.0, 1.0) * 3.0);
                let yg = |g: f32| r.center().y - (g / range).clamp(-1.0, 1.0) * r.height() * 0.5;
                let gy = |y: f32| ((r.center().y - y) / (r.height() * 0.5)).clamp(-1.0, 1.0) * range;
                let painter = ui.painter_at(r);
                painter.rect_filled(r, 3.0, Color32::from_rgb(14, 17, 23));
                for f in [100.0f32, 1000.0, 10000.0] {
                    painter.line_segment([pos2(xf(f), r.top()), pos2(xf(f), r.bottom())], Stroke::new(1.0, Color32::from_gray(30)));
                }
                if drawing {
                    for g in [-12.0f32, 12.0] {
                        painter.line_segment([pos2(r.left(), yg(g)), pos2(r.right(), yg(g))], Stroke::new(1.0, Color32::from_gray(30)));
                        painter.text(pos2(r.left() + 3.0, yg(g)), Align2::LEFT_CENTER, format!("{g:+.0}"), FontId::proportional(8.0), Color32::from_gray(90));
                    }
                }
                painter.line_segment([pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)], Stroke::new(1.0, Color32::from_gray(48)));
                let curve = |n: &Node| -> Vec<Pos2> {
                    let mut u = EqUnit::default();
                    u.set(&n.p, fs);
                    (0..120).map(|i| {
                        let t = i as f32 / 119.0;
                        pos2(r.left() + t * r.width(), yg(u.response_db(fs, 20.0 * 1000f32.powf(t))))
                    }).collect()
                };
                painter.add(Shape::line(curve(n), Stroke::new(1.6, if n.bypass { Color32::from_gray(90) } else { Color32::from_rgb(120, 190, 255) })));
                let mut remove: Option<usize> = None;
                if drawing {
                    if ui.input(|i| i.pointer.hover_pos()).is_some_and(|q| r.contains(q)) {
                        // the wheel belongs to the graph here, not to the canvas
                        self.eq_over = true;
                    }
                    for b in 0..n.p.len() / 4 {
                        let o = b * 4;
                        let t = n.p[o].round() as usize;
                        if t >= 6 {
                            continue;
                        }
                        let uses_gain = matches!(t, 0 | 1 | 2 | 5);
                        // the dots stay inside the graph, also for the extremes of the range
                        let m = 7.0 * z;
                        let c = pos2(xf(n.p[o + 1]).clamp(r.left() + m, r.right() - m), (if uses_gain { yg(n.p[o + 2]) } else { r.center().y }).clamp(r.top() + m, r.bottom() - m));
                        let hr = ui.interact(Rect::from_center_size(c, vec2(18.0 * z, 18.0 * z)), ui.scope_id().with(("eqh", n.id, b)), Sense::click_and_drag());
                        let col = EQ_PALETTE[b % EQ_PALETTE.len()];
                        painter.circle_filled(c, (if hr.hovered() || hr.dragged() { 7.0 } else { 5.5 }) * z, col);
                        painter.circle_stroke(c, 5.5 * z, Stroke::new(1.0, Color32::BLACK));
                        painter.text(c + vec2(0.0, -11.0 * z), Align2::CENTER_CENTER, format!("{}", b + 1), FontId::proportional(8.0 * z), col);
                        if hr.dragged() {
                            if let Some(q) = hr.interact_pointer_pos() {
                                n.p[o + 1] = fx(q.x).clamp(20.0, 20000.0).round();
                                if uses_gain {
                                    let g = gy(q.y);
                                    n.p[o + 2] = ((if t == 5 { g.min(0.0) } else { g }) * 10.0).round() / 10.0;
                                }
                                ch = true;
                            }
                        }
                        if hr.hovered() {
                            let sc = ui.input(|i| i.smooth_scroll_delta.y);
                            if sc != 0.0 {
                                n.p[o + 3] = (n.p[o + 3] * (sc * 0.004).exp()).clamp(0.1, 10.0);
                                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                                ch = true;
                            }
                            painter.text(pos2(r.right() - 4.0, r.top() + 8.0), Align2::RIGHT_CENTER, format!("{}  {:.0} Hz  {:+.1} dB  Q {:.2}", EQ_TYPES[t], n.p[o + 1], n.p[o + 2], n.p[o + 3]), FontId::proportional(9.5), Color32::from_gray(210));
                        }
                        if hr.secondary_clicked() && n.p.len() / 4 > 1 {
                            remove = Some(b);
                        }
                    }
                    // empty space: a click or a drag makes a new band under the pointer, which the drag then moves
                    if (resp.clicked() || resp.drag_started()) && n.p.len() / 4 < EQ_MAX {
                        if let Some(q) = resp.interact_pointer_pos() {
                            n.p.extend([0.0, fx(q.x).round(), (gy(q.y) * 10.0).round() / 10.0, 1.0]);
                            self.eq_new = Some((n.id, n.p.len() / 4 - 1));
                            ch = true;
                        }
                    }
                    if resp.dragged() {
                        if let (Some((id, b)), Some(q)) = (self.eq_new, resp.interact_pointer_pos()) {
                            if id == n.id && b < n.p.len() / 4 {
                                n.p[b * 4 + 1] = fx(q.x).round();
                                n.p[b * 4 + 2] = (gy(q.y) * 10.0).round() / 10.0;
                                ch = true;
                            }
                        }
                    } else if self.eq_new.is_some_and(|(id, _)| id == n.id) {
                        self.eq_new = None;
                    }
                }
                // the band list: type, frequency, level, width
                for b in 0..n.p.len() / 4 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let o = b * 4;
                        let (sw, _) = ui.allocate_exact_size(vec2(4.0 * z, 14.0 * z), Sense::hover());
                        ui.painter().rect_filled(sw, 2.0, EQ_PALETTE[b % EQ_PALETTE.len()]);
                        let cur = EQ_TYPES[(n.p[o].round() as usize).min(EQ_TYPES.len() - 1)];
                        egui::ComboBox::from_id_salt(("eqt", n.id, b)).width(66.0 * z).selected_text(cur).show_ui(ui, |ui| {
                            for (k, c) in EQ_TYPES.iter().enumerate() {
                                if ui.selectable_label(n.p[o].round() as usize == k, *c).clicked() {
                                    n.p[o] = k as f32;
                                    // a notch needs a depth: the level of the band
                                    if k == 5 && n.p[o + 2] > -1.0 {
                                        n.p[o + 2] = -24.0;
                                    }
                                    ch = true;
                                }
                            }
                        });
                        let t = n.p[o].round() as usize;
                        ch |= ui.add(egui::DragValue::new(&mut n.p[o + 1]).range(20.0..=20000.0).speed(5.0).suffix(" Hz").max_decimals(0)).changed();
                        let (lo, hi) = if t == 5 { (-60.0, 0.0) } else { (-24.0, 24.0) };
                        ch |= ui.add_enabled(matches!(t, 0 | 1 | 2 | 5), egui::DragValue::new(&mut n.p[o + 2]).range(lo..=hi).speed(0.1).suffix(" dB").max_decimals(1)).on_hover_text(if t == 5 { "Depth of the notch" } else { "Level of the band" }).changed();
                        ch |= ui.add(egui::DragValue::new(&mut n.p[o + 3]).range(0.1..=10.0).speed(0.02).max_decimals(2)).on_hover_text("Q (width: higher is narrower)").changed();
                        if n.p.len() / 4 > 1 && ui.small_button("x").on_hover_text("Remove this band").clicked() {
                            remove = Some(b);
                        }
                    });
                }
                if let Some(b) = remove {
                    n.p.drain(b * 4..b * 4 + 4);
                    ch = true;
                }
            }
            Kind::Comp | Kind::Limiter => {
                for i in 0..n.kind.info().params.len() {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 8.0 * z), Sense::hover());
                let gr = (-meter.extra).clamp(0.0, 24.0) / 24.0;
                ui.painter().rect_filled(r, 2.0, Color32::from_gray(30));
                ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * gr, r.height())), 2.0, Color32::from_rgb(235, 150, 60));
                ui.label(RichText::new(format!("gain reduction {:.1} dB", meter.extra)).small().weak());
            }
            Kind::Gate => {
                for i in 0..4 {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
                ui.label(RichText::new(if meter.extra > 0.5 { "open" } else { "closed" }).small().weak());
            }
            Kind::Mixer => {
                let cnt = n.inputs();
                // the number of inputs and the master level come first, so that adding an input moves nothing the
                // pointer is on; the rows below line up with the input dots (no extra spacing between them)
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0 * z);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), ROW * z), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_height(ROW * z);
                    ui.label(RichText::new("Inputs").small().color(Color32::from_rgb(160, 172, 190)));
                    let mut c = cnt;
                    if spinner(ui, ("mixn", n.id), &mut c, 1, MIX_MAX, z) {
                        n.p[0] = c as f32;
                        ch = true;
                    }
                });
                ui.allocate_ui_with_layout(vec2(ui.available_width(), ROW * z), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_height(ROW * z);
                    ch |= Self::param_row(ui, n, 1, 44.0);
                });
                let cnt = n.inputs();
                for k in 0..cnt {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                    ui.scope_builder(UiBuilder::new().max_rect(row.shrink2(vec2(10.0 * z, 2.0 * z))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(format!("{}", k + 1)).small().weak());
                        ui.spacing_mut().slider_width = 100.0 * z;
                        let muted = n.p[2 + MIX_MAX + k] >= 0.5;
                        ch |= ui.add_enabled(!muted, egui::Slider::new(&mut n.p[2 + k], -60.0..=12.0).suffix(" dB").max_decimals(1)).changed();
                        if ui.selectable_label(muted, "M").on_hover_text("Mute this input").clicked() {
                            n.p[2 + MIX_MAX + k] = if muted { 0.0 } else { 1.0 };
                            ch = true;
                        }
                    });
                }
            }
            _ => {
                for i in 0..n.kind.info().params.len() {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
            }
        }
        // output level
        if n.kind != Kind::Analyzer || true {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 7.0 * z), Sense::hover());
            let p = ui.painter();
            for (c, pk) in meter.peak.iter().enumerate() {
                let y = r.top() + c as f32 * 4.0 * z;
                let db = 20.0 * pk.max(1e-6).log10();
                let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let rr = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), 3.0 * z));
                p.rect_filled(rr, 1.0, Color32::from_gray(30));
                p.rect_filled(Rect::from_min_size(rr.min, vec2(rr.width() * t, 3.0 * z)), 1.0, if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) });
            }
        }
        ch
    }

    /// Save, load and delete named routings.
    fn routings_menu(&mut self, ui: &mut Ui, cx: &Ctx) {
        let title = if self.cur_name.is_empty() { "Routings".to_string() } else { format!("Routings: {}", self.cur_name) };
        ui.menu_button(title, |ui| {
            ui.set_min_width(250.0);
            ui.label(RichText::new("Saved routings").small().weak());
            let names = list_routings();
            if names.is_empty() {
                ui.label(RichText::new("none yet: type a name below and save").small().weak());
            }
            let (mut load, mut del): (Option<String>, Option<String>) = (None, None);
            egui::ScrollArea::vertical().max_height(260.0).auto_shrink([true, true]).show(ui, |ui| {
                for n in &names {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(*n == self.cur_name, n).on_hover_text("Load this routing (replaces the current one)").clicked() {
                            load = Some(n.clone());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let armed = self.del_arm.as_deref() == Some(n.as_str());
                            if ui.small_button(if armed { "delete?" } else { "x" }).on_hover_text("Delete this saved routing (click twice)").clicked() {
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
                ui.add(egui::TextEdit::singleline(&mut self.save_name).hint_text("name").desired_width(130.0));
                let name = clean_name(&self.save_name);
                let exists = names.iter().any(|n| n.eq_ignore_ascii_case(&name));
                if ui.add_enabled(!name.is_empty(), egui::Button::new(if exists { "Overwrite" } else { "Save" })).on_hover_text("Save the current routing under this name").clicked() {
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
            if !self.cur_name.is_empty() && ui.button(format!("Save changes to {}", self.cur_name)).clicked() {
                match save_routing(&self.cur_name, &self.g) {
                    Ok(()) => self.note = format!("Saved routing {}", self.cur_name),
                    Err(e) => self.note = format!("Cannot save: {e}"),
                }
            }
            ui.separator();
            if ui.button("New empty routing").clicked() {
                self.g = Graph::default();
                self.sel.clear();
                self.cur_name.clear();
                self.push();
                ui.close();
            }
            if ui.button("Input -> analyzer (starter)").clicked() {
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

    fn canvas(&mut self, ui: &mut Ui, cx: &Ctx, canvas: Rect) {
        let origin = canvas.min + self.pan;
        self.eq_over_prev = std::mem::take(&mut self.eq_over);
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, CANVAS_BG);
        // dot grid
        let step = (24.0 * self.zoom).max(10.0);
        let (ox, oy) = (self.pan.x.rem_euclid(step), self.pan.y.rem_euclid(step));
        let mut y = canvas.top() + oy;
        while y < canvas.bottom() {
            let mut x = canvas.left() + ox;
            while x < canvas.right() {
                painter.circle_filled(pos2(x, y), 0.8, Color32::from_gray(42));
                x += step;
            }
            y += step;
        }
        let bg = ui.interact(canvas, ui.scope_id().with("routing_bg"), Sense::click_and_drag());
        let shift = ui.input(|i| i.modifiers.shift);
        // pan with the background, or select with a band while Shift is held
        if bg.dragged_by(egui::PointerButton::Primary) && self.wire.is_none() {
            if shift {
                if self.band.is_none() {
                    self.band = ui.input(|i| i.pointer.press_origin());
                }
            } else {
                self.pan += bg.drag_delta();
            }
        }
        if bg.dragged_by(egui::PointerButton::Middle) {
            self.pan += bg.drag_delta();
        }
        // the wheel zooms about the pointer wherever it is in the node window (also over a node), except over the
        // graph of an equaliser in drawing mode (there it changes the width of a band) and over popups on top
        let over = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p) && ui.ctx().layer_id_at(*p) == Some(ui.layer_id()));
        if let (Some(p), false) = (over, self.eq_over_prev) {
            let (sc, alt) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.alt));
            if sc != egui::Vec2::ZERO {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                if alt {
                    // Alt + wheel (or a horizontal wheel) moves the view
                    self.pan += sc;
                } else if sc.y != 0.0 {
                    // the spot under the pointer stays where it is
                    let m = (p - origin) / self.zoom;
                    self.zoom = (self.zoom * (sc.y * 0.0025).exp()).clamp(0.4, 1.8);
                    self.pan = p - canvas.min - m * self.zoom;
                } else {
                    self.pan += sc;
                }
            }
        }
        // node coordinates <-> screen
        let (origin, z) = (canvas.min + self.pan, self.zoom);
        let sp = move |m: Pos2| origin + m.to_vec2() * z;
        let mp = move |s: Pos2| ((s - origin) / z).to_pos2();
        if bg.clicked() {
            self.sel.clear();
            self.sel_wire = None;
        }
        let pointer = ui.input(|i| i.pointer.interact_pos());

        // wires first (under the nodes)
        let mut wire_hit: Option<(NodeId, u8)> = None;
        let metersum: HashMap<NodeId, f32> = self.eng.as_ref().map(|e| self.g.nodes.iter().map(|n| (n.id, e.meter(n.id).peak[0].max(e.meter(n.id).peak[1]))).collect()).unwrap_or_default();
        for e in &self.g.edges {
            let (Some(a), Some(b)) = (self.g.node(e.from), self.g.node(e.to)) else { continue };
            let pts = wire_pts(sp(out_pos(a, e.from_port as usize)), sp(in_pos(b, e.port as usize)));
            let lvl = metersum.get(&e.from).copied().unwrap_or(0.0);
            let live = ((20.0 * lvl.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
            let selected = self.sel_wire == Some((e.to, e.port));
            let near = pointer.is_some_and(|p| canvas.contains(p) && (0..=20).any(|k| bezier_at(&pts, k as f32 / 20.0).distance(p) < 7.0));
            if near && self.wire.is_none() {
                wire_hit = Some((e.to, e.port));
            }
            let col = if selected { Color32::from_rgb(255, 214, 80) } else if near { Color32::from_rgb(200, 220, 255) } else { Color32::from_rgb(80, 120, 170).gamma_multiply(0.6 + 0.4 * live) };
            painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, Stroke::new(if selected || near { 3.0 } else { 2.2 }, col))));
        }
        if let Some((to, port)) = wire_hit {
            if bg.clicked() || ui.input(|i| i.pointer.primary_clicked()) {
                self.sel_wire = Some((to, port));
                self.sel.clear();
            }
        }
        // a right-click picks the wire under the pointer (or none) for the menu
        if bg.secondary_clicked() {
            self.sel_wire = wire_hit;
        }
        let mut changed_graph = false;
        bg.context_menu(|ui| {
            let at = pointer.map(mp).unwrap_or(pos2(100.0, 100.0));
            if let Some((to, port)) = self.sel_wire {
                if ui.button("Insert a node here...").clicked() {
                    self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::Insert(to, port), focused: false });
                    ui.close();
                }
                if ui.button("Disconnect").clicked() {
                    self.g.disconnect(to, port);
                    self.sel_wire = None;
                    changed_graph = true;
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("Add node...  (Space)").clicked() {
                self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::None, focused: false });
                ui.close();
            }
            if ui.button("Fit all nodes").clicked() {
                self.fit_next = true;
                ui.close();
            }
        });

        // nodes
        let mut dragged: Option<(NodeId, egui::Vec2)> = None;
        let mut clicked: Option<(NodeId, bool)> = None;
        let mut remove: Option<NodeId> = None;
        let mut port_drag: Option<(WireDrag, bool)> = None; // (what, released)
        let ids: Vec<NodeId> = self.g.nodes.iter().map(|n| n.id).collect();
        let loops = self.feedback_nodes(cx);
        for id in ids {
            let Some(idx) = self.g.nodes.iter().position(|n| n.id == id) else { continue };
            let mut n = self.g.nodes[idx].clone();
            let w = node_w(n.kind) * z;
            let (hdr, row) = (HDR * z, ROW * z);
            let info = n.kind.info();
            let top_left = sp(pos2(n.x, n.y));
            let selected = self.sel.contains(&id);
            let bg_idx = painter.add(Shape::Noop);
            let body_rect = Rect::from_min_size(top_left + vec2(0.0, hdr), vec2(w, 600.0 * z));
            let mut used_h = 0.0;
            let mut body_changed = false;
            if body_rect.top() < canvas.bottom() + 600.0 && top_left.x < canvas.right() && top_left.x + w > canvas.left() {
                let (changed, h) = {
                    let mut child = ui.new_child(UiBuilder::new().max_rect(body_rect.shrink2(vec2(10.0 * z, 5.0 * z))).layout(egui::Layout::top_down(egui::Align::LEFT)));
                    child.set_clip_rect(canvas);
                    scale_style(child.style_mut(), z);
                    let c = self.body(&mut child, cx, &mut n, loops.contains(&id));
                    (c, child.min_rect().height() + 10.0 * z)
                };
                body_changed = changed;
                used_h = h;
            }
            let full = Rect::from_min_size(top_left, vec2(w, hdr + used_h.max(row)));
            // header
            let header = Rect::from_min_size(top_left, vec2(w, hdr));
            let (r, g, b) = info.color;
            let hcol = Color32::from_rgb(r, g, b);
            painter.set(
                bg_idx,
                Shape::Vec(vec![
                    Shape::rect_filled(full, 6.0, BODY_BG),
                    Shape::rect_filled(header, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, if n.bypass { Color32::from_gray(70) } else { hcol.gamma_multiply(if selected { 1.0 } else { 0.78 }) }),
                    Shape::rect_stroke(full, 6.0, Stroke::new(if selected { 1.8 } else { 1.0 }, if selected { Color32::from_rgb(235, 240, 250) } else { Color32::from_rgb(52, 60, 76) }), egui::StrokeKind::Inside),
                ]),
            );
            let title = match n.kind {
                Kind::Input => "Input".to_string(),
                k => k.name().to_string(),
            };
            painter.text(header.min + vec2(10.0 * z, hdr * 0.5), Align2::LEFT_CENTER, title, FontId::proportional(12.0 * z), Color32::from_rgb(10, 14, 20));
            // header buttons: bypass, remove
            let hb = ui.interact(Rect::from_min_max(header.min, pos2(header.max.x - 44.0 * z, header.max.y)), ui.scope_id().with(("nh", id)), Sense::click_and_drag());
            if info.inputs > 0 && info.outputs > 0 {
                let br = Rect::from_center_size(pos2(header.max.x - 34.0 * z, header.center().y), vec2(14.0 * z, 14.0 * z));
                let r = ui.interact(br, ui.scope_id().with(("nb", id)), Sense::click());
                painter.rect_filled(br, 3.0, if n.bypass { Color32::from_rgb(235, 190, 70) } else { Color32::from_black_alpha(60) });
                painter.text(br.center(), Align2::CENTER_CENTER, "B", FontId::proportional(9.5 * z), if n.bypass { Color32::BLACK } else { Color32::from_gray(210) });
                if r.on_hover_text("Bypass this node").clicked() {
                    n.bypass = !n.bypass;
                    body_changed = true;
                }
            }
            let xr = Rect::from_center_size(pos2(header.max.x - 12.0 * z, header.center().y), vec2(14.0 * z, 14.0 * z));
            let xrr = ui.interact(xr, ui.scope_id().with(("nx", id)), Sense::click());
            painter.text(xr.center(), Align2::CENTER_CENTER, "x", FontId::proportional(12.0 * z), if xrr.hovered() { Color32::WHITE } else { Color32::from_gray(30) });
            if xrr.on_hover_text("Remove the node").clicked() {
                remove = Some(id);
            }
            if hb.dragged() {
                dragged = Some((id, hb.drag_delta() / z));
            }
            if hb.clicked() || hb.drag_started() {
                clicked = Some((id, shift));
            }
            hb.context_menu(|ui| {
                if ui.button("Duplicate").clicked() {
                    let (x, y) = (n.x + 30.0, n.y + 30.0);
                    let k = n.kind;
                    let nn = self.g.add(k, x, y);
                    if nn != n.id {
                        if let Some(m) = self.g.node_mut(nn) {
                            m.p = n.p.clone();
                            m.s = n.s.clone();
                            m.bypass = n.bypass;
                        }
                        changed_graph = true;
                    }
                    ui.close();
                }
                if ui.button("Remove").clicked() {
                    remove = Some(id);
                    ui.close();
                }
            });
            // ports
            // a port carries a stereo pair: the dot shows the left half in cyan and the right half in pink;
            // the ports of split / merge nodes carry one channel and are drawn in that channel's colour
            let mut dot = |ui: &mut Ui, c: Pos2, tag: (u8, NodeId, u8), wired: bool, lr: u8| {
                let pr = PORT_R * z.max(0.7);
                let rr = Rect::from_center_size(c, vec2(pr * 3.2, pr * 3.2));
                let r = ui.interact(rr, ui.scope_id().with(("port", tag)), Sense::click_and_drag());
                let (lc, rc) = (Color32::from_rgb(70, 200, 255), Color32::from_rgb(255, 100, 160));
                let k = if r.hovered() || wired { 1.0 } else { 0.55 };
                painter.circle_filled(c, pr + 0.5, Color32::from_rgb(20, 24, 32));
                match lr {
                    1 => {
                        painter.circle_filled(c, pr, lc.gamma_multiply(k));
                    }
                    2 => {
                        painter.circle_filled(c, pr, rc.gamma_multiply(k));
                    }
                    _ => {
                        half_dot(&painter, c, pr, true, lc.gamma_multiply(k));
                        half_dot(&painter, c, pr, false, rc.gamma_multiply(k));
                    }
                }
                painter.circle_stroke(c, pr, Stroke::new(if r.hovered() { 2.0 } else { 1.2 }, if r.hovered() { Color32::WHITE } else { Color32::from_rgb(150, 175, 215) }));
                r.on_hover_text(match lr {
                    1 => "Left channel",
                    2 => "Right channel",
                    _ => "Stereo: left (cyan half) and right (pink half) travel together in one wire",
                })
            };
            for p in 0..n.inputs() {
                let c = sp(in_pos(&n, p));
                let wired = self.g.edge_into(id, p as u8).is_some();
                let lr = if n.kind == Kind::Merge { 1 + p as u8 } else { 0 };
                let r = dot(ui, c, (0, id, p as u8), wired, lr);
                if r.drag_started() {
                    // dragging a connected input moves its wire; an empty one starts a new wire backwards
                    port_drag = Some(match self.g.edge_into(id, p as u8).copied() {
                        Some(e) => {
                            self.g.disconnect(id, p as u8);
                            changed_graph = true;
                            (WireDrag::FromOut(e.from, e.from_port), false)
                        }
                        None => (WireDrag::FromIn(id, p as u8), false),
                    });
                }
            }
            for op in 0..n.outputs() {
                let c = sp(out_pos(&n, op));
                let lr = if n.kind == Kind::Split { 1 + op as u8 } else { 0 };
                let r = dot(ui, c, (1, id, op as u8), self.g.edges.iter().any(|e| e.from == id && e.from_port as usize == op), lr);
                if r.drag_started() {
                    port_drag = Some((WireDrag::FromOut(id, op as u8), false));
                }
            }
            if body_changed {
                let still = n.inputs();
                self.g.nodes[idx] = n;
                self.g.edges.retain(|e| e.to != id || (e.port as usize) < still);
                changed_graph = true;
            }
        }
        if let Some((w, _)) = port_drag {
            self.wire = Some(w);
        }

        // moving, selecting, removing
        if let Some((id, d)) = clicked {
            if shift {
                if !self.sel.remove(&id) {
                    self.sel.insert(id);
                }
            } else if !self.sel.contains(&id) {
                self.sel.clear();
                self.sel.insert(id);
            }
            self.sel_wire = None;
            // bring to front
            if let Some(pos) = self.g.nodes.iter().position(|n| n.id == id) {
                let n = self.g.nodes.remove(pos);
                self.g.nodes.push(n);
            }
            let _ = d;
        }
        if let Some((id, d)) = dragged {
            let moving: Vec<NodeId> = if self.sel.contains(&id) { self.sel.iter().copied().collect() } else { vec![id] };
            for m in moving {
                if let Some(n) = self.g.node_mut(m) {
                    n.x += d.x;
                    n.y += d.y;
                }
            }
            self.dirty = true;
        }
        if let Some(id) = remove {
            self.g.remove(id);
            self.sel.remove(&id);
            changed_graph = true;
        }
        // rubber band
        if let Some(a) = self.band {
            if let Some(p) = pointer {
                let r = Rect::from_two_pos(a, p);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(140, 185, 255)), egui::StrokeKind::Inside);
                painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 160, 255, 24));
                if ui.input(|i| i.pointer.any_released()) {
                    self.sel.clear();
                    for n in &self.g.nodes {
                        let nr = Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(node_w(n.kind), 60.0) * z);
                        if r.intersects(nr) {
                            self.sel.insert(n.id);
                        }
                    }
                    self.band = None;
                }
            } else {
                self.band = None;
            }
        }

        // wire being dragged
        if let (Some(w), Some(p)) = (self.wire, pointer) {
            let (a, b) = match w {
                WireDrag::FromOut(id, op) => (self.g.node(id).map(|n| sp(out_pos(n, op as usize))), p),
                WireDrag::FromIn(id, port) => (Some(p), self.g.node(id).map_or(p, |n| sp(in_pos(n, port as usize)))),
            };
            if let Some(a) = a {
                painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(wire_pts(a, b), false, Color32::TRANSPARENT, Stroke::new(2.4, Color32::from_rgb(255, 214, 80)))));
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.wire = None;
                let near = |c: Pos2| c.distance(p) < 15.0;
                match w {
                    WireDrag::FromOut(from, fport) => {
                        let target = self.g.nodes.iter().find_map(|n| (0..n.inputs()).find(|&k| near(sp(in_pos(n, k)))).map(|k| (n.id, k as u8)));
                        // dropped on a node body: first free input
                        let target = target.or_else(|| {
                            self.g.nodes.iter().find_map(|n| {
                                let r = Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(node_w(n.kind), HDR + 20.0) * z);
                                if r.contains(p) && n.id != from {
                                    (0..n.inputs()).find(|&k| self.g.edge_into(n.id, k as u8).is_none()).map(|k| (n.id, k as u8))
                                } else {
                                    None
                                }
                            })
                        });
                        match target {
                            Some((to, port)) => {
                                if let Err(m) = self.g.connect_from(from, fport, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: mp(p), text: String::new(), sel: 0, link: Link::FromOut(from, fport), focused: false });
                            }
                            None => {}
                        }
                    }
                    WireDrag::FromIn(to, port) => {
                        let src = self.g.nodes.iter().find_map(|n| (0..n.outputs()).find(|&k| near(sp(out_pos(n, k)))).map(|k| (n.id, k as u8)));
                        match src {
                            Some((from, fport)) => {
                                if let Err(m) = self.g.connect_from(from, fport, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: mp(p) - vec2(260.0, 0.0), text: String::new(), sel: 0, link: Link::ToIn(to, port), focused: false });
                            }
                            None => {}
                        }
                    }
                }
            }
        } else if self.wire.is_some() && ui.input(|i| i.pointer.any_released()) {
            self.wire = None;
        }

        // keyboard: Space = search, Delete = remove, F = fit
        let typing = ui.ctx().egui_wants_keyboard_input();
        if self.search.is_none() && !typing && ui.rect_contains_pointer(canvas) {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
                let at = pointer.map(mp).unwrap_or(pos2(100.0, 100.0));
                self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::None, focused: false });
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
                if let Some((to, port)) = self.sel_wire.take() {
                    self.g.disconnect(to, port);
                    changed_graph = true;
                }
                let gone: Vec<NodeId> = self.sel.drain().collect();
                for id in gone {
                    self.g.remove(id);
                    changed_graph = true;
                }
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)) {
                self.fit_next = true;
            }
        }
        self.search_ui(ui, cx, canvas);
        if self.fit_next && !self.g.nodes.is_empty() {
            self.fit_next = false;
            let (mut lo, mut hi) = (pos2(f32::MAX, f32::MAX), pos2(f32::MIN, f32::MIN));
            for n in &self.g.nodes {
                lo = pos2(lo.x.min(n.x), lo.y.min(n.y));
                hi = pos2(hi.x.max(n.x + node_w(n.kind)), hi.y.max(n.y + 120.0));
            }
            let centre = (lo.to_vec2() + hi.to_vec2()) * 0.5;
            // zoom out when everything does not fit, but never in beyond 100 %
            let size = (hi - lo).max(vec2(1.0, 1.0));
            if std::env::var_os("VISUSCOPE_ZOOM").is_none() {
                self.zoom = ((canvas.width() / size.x).min(canvas.height() / size.y) * 0.92).clamp(0.4, 1.0);
            }
            self.pan = canvas.size() * 0.5 - centre * self.zoom;
        }
        if changed_graph {
            self.push();
        }
    }
}

impl Tool for Routing {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        if cx.graph_on && self.eng.is_none() {
            self.make_engine(cx);
        }
        let Some(e) = self.eng.clone() else { return };
        use std::sync::atomic::Ordering::Relaxed;
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
        if self.dirty && self.last_save.elapsed().as_secs_f32() > 1.0 {
            self.save_file();
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        if !cx.graph_on {
            ui.label("The routing graph is off. Turn it on in Options.");
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.button("+ Node").on_hover_text("Search all nodes (Space)").clicked() {
                self.search = Some(Search { at: pos2(100.0, 100.0) - self.pan.to_pos2().to_vec2(), text: String::new(), sel: 0, link: Link::None, focused: false });
            }
            if ui.button("Fit").on_hover_text("Show all nodes (F)").clicked() {
                self.fit_next = true;
            }
            self.routings_menu(ui, cx);
            if ui.small_button(format!("{:.0} %", self.zoom * 100.0)).on_hover_text("Zoom of the node window: Shift + mouse wheel (click: back to 100 %)").clicked() {
                self.zoom = 1.0;
            }
            ui.label(RichText::new("Rate").small().weak());
            let cur = if self.rate_pref == 0 { "auto".to_string() } else { format!("{} Hz", self.rate_pref) };
            egui::ComboBox::from_id_salt("graph_rate").width(80.0).selected_text(cur).show_ui(ui, |ui| {
                for r in [0u32, 44100, 48000, 88200, 96000, 176400, 192000] {
                    if ui.selectable_value(&mut self.rate_pref, r, if r == 0 { "auto (first input)".to_string() } else { format!("{r} Hz") }).clicked() {
                        if let Some(e) = &self.eng {
                            e.rev.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
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
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("rate", &mut self.rate_pref);
        i.set("zoom", &mut self.zoom);
        self.zoom = if self.zoom.is_finite() { self.zoom.clamp(0.4, 1.8) } else { 1.0 };
    }
}

#[allow(dead_code)]
fn _unused(_: f32) -> f32 {
    db_to_lin(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind) -> Entry {
        Entry { label: kind.name().into(), group: kind.info().group, keywords: String::new(), kind, s: String::new(), p0: None }
    }

    #[test]
    fn new_nodes_link_up_and_split_wires() {
        let mut r = Routing::new();
        r.g = Graph::starter("dev", None);
        let (inp, ana) = (1, 2);
        // insert a node into the wire input -> analyzer
        r.add_entry(&entry(Kind::Eq), pos2(100.0, 0.0), Link::Insert(ana, 0));
        let eq = r.g.nodes.iter().find(|n| n.kind == Kind::Eq).unwrap().id;
        assert_eq!(r.g.edge_into(eq, 0).unwrap().from, inp);
        assert_eq!(r.g.edge_into(ana, 0).unwrap().from, eq);
        // dropping a wire from the input on empty canvas and picking a mixer connects it
        r.add_entry(&entry(Kind::Mixer), pos2(0.0, 100.0), Link::FromOut(inp, 0));
        let mx = r.g.nodes.iter().find(|n| n.kind == Kind::Mixer).unwrap().id;
        assert_eq!(r.g.edge_into(mx, 0).unwrap().from, inp);
        // the input now feeds two nodes: a split
        assert_eq!(r.g.edges.iter().filter(|e| e.from == inp).count(), 2);
        // a sink picked from a wire that starts at an input port ends up connected to its source
        r.add_entry(&entry(Kind::Generator), pos2(0.0, 200.0), Link::ToIn(mx, 1));
        let gen_id = r.g.nodes.iter().find(|n| n.kind == Kind::Generator).unwrap().id;
        assert_eq!(r.g.edge_into(mx, 1).unwrap().from, gen_id);
        // a node without an input cannot be inserted into a wire
        let before = r.g.edges.len();
        r.add_entry(&entry(Kind::Generator), pos2(0.0, 300.0), Link::Insert(ana, 0));
        assert_eq!(r.g.edges.len(), before);
    }

    #[test]
    fn routings_can_be_saved_listed_loaded_and_names_are_cleaned() {
        let dir = std::env::temp_dir().join(format!("visuscope_routings_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY of the test: nothing else reads this variable
        unsafe { std::env::set_var("VISUSCOPE_ROUTINGS_DIR", &dir) };
        assert!(list_routings().is_empty());
        assert_eq!(clean_name("  ../evil/na:me*? "), "evilname");
        let mut g = Graph::starter("dev", None);
        let eq = g.add(Kind::Eq, 5.0, 6.0);
        g.node_mut(eq).unwrap().p[2] = 3.0;
        save_routing("Studio A", &g).unwrap();
        save_routing("alpha", &Graph::default()).unwrap();
        assert!(save_routing("///", &g).is_err());
        assert_eq!(list_routings(), vec!["alpha".to_string(), "Studio A".to_string()]);
        let back = load_routing("Studio A").unwrap();
        assert_eq!(back.nodes.len(), g.nodes.len());
        assert_eq!(back.node(eq).unwrap().p[2], 3.0);
        assert!(load_routing("missing").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_and_merge_ports_are_per_channel() {
        let mut g = Graph::default();
        let (i, s, m, a) = (g.add(Kind::Input, 0.0, 0.0), g.add(Kind::Split, 0.0, 0.0), g.add(Kind::Merge, 0.0, 0.0), g.add(Kind::Analyzer, 0.0, 0.0));
        g.connect(i, s, 0).unwrap();
        g.connect_from(s, 0, m, 0).unwrap();
        g.connect_from(s, 1, m, 1).unwrap();
        g.connect(m, a, 0).unwrap();
        assert!(g.connect_from(s, 2, m, 0).is_err(), "a split node has two outputs");
        assert!(g.connect_from(i, 1, s, 0).is_err(), "an input device has one");
        assert_eq!(g.edge_into(m, 1).unwrap().from_port, 1);
        let h = Graph::decode(&g.encode()).unwrap();
        assert_eq!(h.edge_into(m, 1).unwrap().from_port, 1);
        // the two outputs sit one row apart and the rows line up with the merge inputs
        let (sn, mn) = (g.node(s).unwrap(), g.node(m).unwrap());
        assert!((out_pos(sn, 1).y - out_pos(sn, 0).y - ROW).abs() < 1e-4);
        assert!((in_pos(mn, 1).y - in_pos(mn, 0).y - ROW).abs() < 1e-4);
    }

    #[test]
    fn port_positions_line_up_with_mixer_rows() {
        let mut g = Graph::default();
        let m = g.add(Kind::Mixer, 10.0, 20.0);
        let n = g.node(m).unwrap();
        // consecutive input dots are one row apart, the output sits in the header
        assert!((in_pos(n, 1).y - in_pos(n, 0).y - ROW).abs() < 1e-4);
        assert!(out_pos(n, 0).y < in_pos(n, 0).y);
        assert!(out_pos(n, 0).x > in_pos(n, 0).x);
    }
}
