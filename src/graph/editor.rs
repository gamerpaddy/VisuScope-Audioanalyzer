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
const BODY_BG: Color32 = Color32::from_rgb(27, 32, 42);
const CANVAS_BG: Color32 = Color32::from_rgb(16, 19, 25);

fn node_w(k: Kind) -> f32 {
    match k {
        Kind::Eq => 280.0,
        Kind::Mixer => 230.0,
        Kind::Input | Kind::Output | Kind::Comp | Kind::Gate => 240.0,
        _ => 210.0,
    }
}

/// Position of an input port in canvas coordinates.
fn in_pos(n: &Node, port: usize) -> Pos2 {
    if n.kind == Kind::Mixer {
        pos2(n.x, n.y + HDR + 6.0 + port as f32 * ROW + ROW * 0.5)
    } else {
        pos2(n.x, n.y + HDR * 0.5)
    }
}

fn out_pos(n: &Node) -> Pos2 {
    pos2(n.x + node_w(n.kind), n.y + HDR * 0.5)
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
    /// dragging from an output dot
    FromOut(NodeId),
    /// dragging from an input dot (connect it to an output)
    FromIn(NodeId, u8),
}

/// What the search window will do with the node that is picked.
#[derive(Clone, Copy)]
enum Link {
    None,
    /// connect the new node's input to this output
    FromOut(NodeId),
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
    let gain = g.add(Kind::Gain, 300.0, 40.0);
    let mx = g.add(Kind::Mixer, 300.0, 190.0);
    let eq = g.add(Kind::Eq, 580.0, 60.0);
    let cp = g.add(Kind::Comp, 580.0, 330.0);
    let an = g.add(Kind::Analyzer, 880.0, 70.0);
    let out = g.add(Kind::Output, 880.0, 190.0);
    let tl = g.add(Kind::TimelineRec, 880.0, 330.0);
    for (a, b, p) in [(inp, gain, 0), (gain, mx, 2), (g1, mx, 0), (g2, mx, 1), (mx, eq, 0), (eq, cp, 0), (cp, an, 0), (cp, out, 0), (cp, tl, 0)] {
        let _ = g.connect(a, b, p);
    }
    g.node_mut(eq).unwrap().p[2] = 4.0;
    g
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
        Self { eng: None, g: Graph::default(), pan: vec2(20.0, 20.0), sel: HashSet::new(), sel_wire: None, wire: None, search: None, dirty: false, last_save: Instant::now(), rate_pref: 0, fit_next: true, note: String::new(), band: None }
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
        if let Some(en) = &self.eng {
            for d in en.devs.lock().iter() {
                v.push(e(format!("Input: {} ({} ch, {} Hz)", short_label(&d.label), d.ch, d.sr), Kind::Input, d.id.clone(), None));
            }
        }
        for (i, d) in cx.out_devs.iter().enumerate() {
            v.push(e(format!("Output: {}", d.label), Kind::Output, if i == 0 { String::new() } else { d.id.clone() }, None));
        }
        let n_gen = cx.gens.lock().len();
        for g in 0..n_gen {
            v.push(e(format!("Generator {}", g + 1), Kind::Generator, String::new(), Some(g as f32 + 1.0)));
        }
        v.push(e("Timeline master out".into(), Kind::TimelineOut, String::new(), None));
        for (id, name) in bus().tracks.lock().iter() {
            v.push(e(format!("Timeline track out: {name}"), Kind::TimelineOut, id.to_string(), None));
            v.push(e(format!("Timeline record: {name}"), Kind::TimelineRec, id.to_string(), None));
        }
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
            Link::FromOut(a) if info.inputs > 0 => {
                if let Err(m) = self.g.connect(a, id, 0) {
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
                        let _ = self.g.connect(old.from, id, 0);
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
        let want_in = matches!(s.link, Link::FromOut(_) | Link::Insert(..));
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
        let screen = canvas.min + self.pan.to_pos2().to_vec2() + s.at.to_vec2();
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
        let defs = n.kind.info().params;
        let Some(d) = defs.get(i) else { return false };
        if n.p.len() <= i {
            n.p.resize(i + 1, d.def);
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_sized(vec2(label_w, 16.0), egui::Label::new(RichText::new(d.name).small().color(Color32::from_rgb(160, 172, 190))).truncate());
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
                ui.spacing_mut().slider_width = (ui.available_width() - 62.0).max(40.0);
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
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.spacing_mut().interact_size.y = 16.0;
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
                    egui::ComboBox::from_id_salt(("gsel", n.id)).selected_text(format!("Gen {gi}")).width(70.0).show_ui(ui, |ui| {
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
            Kind::FilePlayer => {
                ui.label(RichText::new("plays what the Recorder window plays").small().weak());
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
                // response curve
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 54.0), Sense::hover());
                let mut u = EqUnit::default();
                let fs = self.eng.as_ref().map_or(48000.0, |e| e.rate.load(std::sync::atomic::Ordering::Relaxed) as f32);
                u.set(&n.p, fs);
                let p = ui.painter_at(r);
                p.rect_filled(r, 3.0, Color32::from_rgb(14, 17, 23));
                p.line_segment([pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)], Stroke::new(1.0, Color32::from_gray(40)));
                let pts: Vec<Pos2> = (0..96)
                    .map(|i| {
                        let t = i as f32 / 95.0;
                        let f = 20.0 * 1000f32.powf(t);
                        let db = u.response_db(fs, f).clamp(-20.0, 20.0);
                        pos2(r.left() + t * r.width(), r.center().y - db / 20.0 * r.height() * 0.5)
                    })
                    .collect();
                p.add(Shape::line(pts, Stroke::new(1.5, if n.bypass { Color32::from_gray(90) } else { Color32::from_rgb(120, 190, 255) })));
                for b in 0..EQ_BANDS {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let o = b * 4;
                        let defs = n.kind.info().params;
                        let cur = defs[o].choices[n.p[o].round() as usize];
                        egui::ComboBox::from_id_salt(("eqt", n.id, b)).width(64.0).selected_text(cur).show_ui(ui, |ui| {
                            for (k, c) in EQ_TYPES.iter().enumerate() {
                                if ui.selectable_label(n.p[o].round() as usize == k, *c).clicked() {
                                    n.p[o] = k as f32;
                                    ch = true;
                                }
                            }
                        });
                        for (j, w) in [(1usize, 56.0f32), (2, 40.0), (3, 36.0)] {
                            let d = &defs[o + j];
                            let mut dv = egui::DragValue::new(&mut n.p[o + j]).range(d.min..=d.max).suffix(d.unit).speed(if j == 1 { 5.0 } else { 0.1 });
                            if j == 1 {
                                dv = dv.max_decimals(0);
                            } else {
                                dv = dv.max_decimals(1);
                            }
                            let _ = w;
                            ch |= ui.add(dv).changed();
                        }
                    });
                }
            }
            Kind::Comp | Kind::Limiter => {
                for i in 0..n.kind.info().params.len() {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 8.0), Sense::hover());
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
                // the rows line up with the input dots
                ui.add_space(6.0);
                for k in 0..cnt {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
                    ui.scope_builder(UiBuilder::new().max_rect(row.shrink2(vec2(10.0, 2.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(format!("{}", k + 1)).small().weak());
                        ui.spacing_mut().slider_width = 100.0;
                        let muted = n.p[2 + MIX_MAX + k] >= 0.5;
                        ch |= ui.add_enabled(!muted, egui::Slider::new(&mut n.p[2 + k], -60.0..=12.0).suffix(" dB").max_decimals(1)).changed();
                        if ui.selectable_label(muted, "M").on_hover_text("Mute this input").clicked() {
                            n.p[2 + MIX_MAX + k] = if muted { 0.0 } else { 1.0 };
                            ch = true;
                        }
                    });
                }
                ch |= Self::param_row(ui, n, 1, 44.0);
                ch |= Self::param_row(ui, n, 0, 44.0);
            }
            _ => {
                for i in 0..n.kind.info().params.len() {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
            }
        }
        // output level
        if n.kind != Kind::Analyzer || true {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 7.0), Sense::hover());
            let p = ui.painter();
            for (c, pk) in meter.peak.iter().enumerate() {
                let y = r.top() + c as f32 * 4.0;
                let db = 20.0 * pk.max(1e-6).log10();
                let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let rr = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), 3.0));
                p.rect_filled(rr, 1.0, Color32::from_gray(30));
                p.rect_filled(Rect::from_min_size(rr.min, vec2(rr.width() * t, 3.0)), 1.0, if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) });
            }
        }
        ch
    }

    // ------------------------------------------------------------------ canvas

    fn canvas(&mut self, ui: &mut Ui, cx: &Ctx, canvas: Rect) {
        let origin = canvas.min + self.pan;
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, CANVAS_BG);
        // dot grid
        let step = 24.0;
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
        if bg.hovered() {
            let sc = ui.input(|i| i.smooth_scroll_delta);
            if sc != egui::Vec2::ZERO {
                self.pan += if shift { vec2(sc.y, 0.0) } else { sc };
            }
        }
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
            let pts = wire_pts(origin + out_pos(a).to_vec2(), origin + in_pos(b, e.port as usize).to_vec2());
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
            let at = pointer.map(|p| p - origin).unwrap_or(vec2(100.0, 100.0)).to_pos2();
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
            let w = node_w(n.kind);
            let info = n.kind.info();
            let top_left = origin + vec2(n.x, n.y);
            let selected = self.sel.contains(&id);
            let bg_idx = painter.add(Shape::Noop);
            let body_rect = Rect::from_min_size(top_left + vec2(0.0, HDR), vec2(w, 600.0));
            let mut used_h = 0.0;
            let mut body_changed = false;
            if body_rect.top() < canvas.bottom() + 600.0 && top_left.x < canvas.right() && top_left.x + w > canvas.left() {
                let (changed, h) = {
                    let mut child = ui.new_child(UiBuilder::new().max_rect(body_rect.shrink2(vec2(10.0, 5.0))).layout(egui::Layout::top_down(egui::Align::LEFT)));
                    child.set_clip_rect(canvas);
                    let c = self.body(&mut child, cx, &mut n, loops.contains(&id));
                    (c, child.min_rect().height() + 10.0)
                };
                body_changed = changed;
                used_h = h;
            }
            let full = Rect::from_min_size(top_left, vec2(w, HDR + used_h.max(ROW)));
            // header
            let header = Rect::from_min_size(top_left, vec2(w, HDR));
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
            painter.text(header.min + vec2(10.0, HDR * 0.5), Align2::LEFT_CENTER, title, FontId::proportional(12.0), Color32::from_rgb(10, 14, 20));
            // header buttons: bypass, remove
            let hb = ui.interact(Rect::from_min_max(header.min, pos2(header.max.x - 44.0, header.max.y)), ui.scope_id().with(("nh", id)), Sense::click_and_drag());
            if info.inputs > 0 && info.outputs > 0 {
                let br = Rect::from_center_size(pos2(header.max.x - 34.0, header.center().y), vec2(14.0, 14.0));
                let r = ui.interact(br, ui.scope_id().with(("nb", id)), Sense::click());
                painter.rect_filled(br, 3.0, if n.bypass { Color32::from_rgb(235, 190, 70) } else { Color32::from_black_alpha(60) });
                painter.text(br.center(), Align2::CENTER_CENTER, "B", FontId::proportional(9.5), if n.bypass { Color32::BLACK } else { Color32::from_gray(210) });
                if r.on_hover_text("Bypass this node").clicked() {
                    n.bypass = !n.bypass;
                    body_changed = true;
                }
            }
            let xr = Rect::from_center_size(pos2(header.max.x - 12.0, header.center().y), vec2(14.0, 14.0));
            let xrr = ui.interact(xr, ui.scope_id().with(("nx", id)), Sense::click());
            painter.text(xr.center(), Align2::CENTER_CENTER, "x", FontId::proportional(12.0), if xrr.hovered() { Color32::WHITE } else { Color32::from_gray(30) });
            if xrr.on_hover_text("Remove the node").clicked() {
                remove = Some(id);
            }
            if hb.dragged() {
                dragged = Some((id, hb.drag_delta()));
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
            let mut dot = |ui: &mut Ui, c: Pos2, tag: (u8, NodeId, u8), wired: bool| {
                let rr = Rect::from_center_size(c, vec2(PORT_R * 3.2, PORT_R * 3.2));
                let r = ui.interact(rr, ui.scope_id().with(("port", tag)), Sense::click_and_drag());
                painter.circle_filled(c, PORT_R, if r.hovered() { Color32::WHITE } else if wired { Color32::from_rgb(110, 170, 235) } else { Color32::from_rgb(34, 40, 52) });
                painter.circle_stroke(c, PORT_R, Stroke::new(1.5, Color32::from_rgb(150, 175, 215)));
                r
            };
            for p in 0..n.inputs() {
                let c = origin + in_pos(&n, p).to_vec2();
                let wired = self.g.edge_into(id, p as u8).is_some();
                let r = dot(ui, c, (0, id, p as u8), wired);
                if r.drag_started() {
                    // dragging a connected input moves its wire; an empty one starts a new wire backwards
                    port_drag = Some(match self.g.edge_into(id, p as u8).copied() {
                        Some(e) => {
                            self.g.disconnect(id, p as u8);
                            changed_graph = true;
                            (WireDrag::FromOut(e.from), false)
                        }
                        None => (WireDrag::FromIn(id, p as u8), false),
                    });
                }
            }
            if info.outputs > 0 {
                let c = origin + out_pos(&n).to_vec2();
                let r = dot(ui, c, (1, id, 0), self.g.edges.iter().any(|e| e.from == id));
                if r.drag_started() {
                    port_drag = Some((WireDrag::FromOut(id), false));
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
                        let nr = Rect::from_min_size(origin + vec2(n.x, n.y), vec2(node_w(n.kind), 60.0));
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
                WireDrag::FromOut(id) => (self.g.node(id).map(|n| origin + out_pos(n).to_vec2()), p),
                WireDrag::FromIn(id, port) => (Some(p), self.g.node(id).map_or(p, |n| origin + in_pos(n, port as usize).to_vec2())),
            };
            if let Some(a) = a {
                painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(wire_pts(a, b), false, Color32::TRANSPARENT, Stroke::new(2.4, Color32::from_rgb(255, 214, 80)))));
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.wire = None;
                let near = |c: Pos2| c.distance(p) < 15.0;
                match w {
                    WireDrag::FromOut(from) => {
                        let target = self.g.nodes.iter().find_map(|n| (0..n.inputs()).find(|&k| near(origin + in_pos(n, k).to_vec2())).map(|k| (n.id, k as u8)));
                        // dropped on a node body: first free input
                        let target = target.or_else(|| {
                            self.g.nodes.iter().find_map(|n| {
                                let r = Rect::from_min_size(origin + vec2(n.x, n.y), vec2(node_w(n.kind), HDR + 20.0));
                                if r.contains(p) && n.id != from {
                                    (0..n.inputs()).find(|&k| self.g.edge_into(n.id, k as u8).is_none()).map(|k| (n.id, k as u8))
                                } else {
                                    None
                                }
                            })
                        });
                        match target {
                            Some((to, port)) => {
                                if let Err(m) = self.g.connect(from, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: (p - origin).to_pos2(), text: String::new(), sel: 0, link: Link::FromOut(from), focused: false });
                            }
                            None => {}
                        }
                    }
                    WireDrag::FromIn(to, port) => {
                        let src = self.g.nodes.iter().find(|n| n.outputs() > 0 && near(origin + out_pos(n).to_vec2())).map(|n| n.id);
                        match src {
                            Some(from) => {
                                if let Err(m) = self.g.connect(from, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: (p - origin).to_pos2() - vec2(260.0, 0.0), text: String::new(), sel: 0, link: Link::ToIn(to, port), focused: false });
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
                let at = pointer.map(|p| p - origin).unwrap_or(vec2(100.0, 100.0));
                self.search = Some(Search { at: at.to_pos2(), text: String::new(), sel: 0, link: Link::None, focused: false });
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
            self.pan = canvas.size() * 0.5 - centre;
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
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("rate", &mut self.rate_pref);
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
        r.add_entry(&entry(Kind::Mixer), pos2(0.0, 100.0), Link::FromOut(inp));
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
    fn port_positions_line_up_with_mixer_rows() {
        let mut g = Graph::default();
        let m = g.add(Kind::Mixer, 10.0, 20.0);
        let n = g.node(m).unwrap();
        // consecutive input dots are one row apart, the output sits in the header
        assert!((in_pos(n, 1).y - in_pos(n, 0).y - ROW).abs() < 1e-4);
        assert!(out_pos(n).y < in_pos(n, 0).y);
        assert!(out_pos(n).x > in_pos(n, 0).x);
    }
}
