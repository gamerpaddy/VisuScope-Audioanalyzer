//! Drawing and mouse / keyboard handling of the timeline.

use super::*;
use crate::tools::plot::{hz_label, inset, Plot};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Layout, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, UiBuilder};

const HEADER_W: f32 = 178.0;
const RULER_H: f32 = 22.0;
const ADD_ROW_H: f32 = 34.0;
const BG_DARK: Color32 = Color32::from_rgb(14, 17, 22);
const LANE_A: Color32 = Color32::from_rgb(20, 24, 31);
const LANE_B: Color32 = Color32::from_rgb(17, 20, 27);
const HDR_BG: Color32 = Color32::from_rgb(26, 31, 40);
const HDR_SEL: Color32 = Color32::from_rgb(34, 44, 62);
const PLAYHEAD: Color32 = Color32::from_rgb(255, 214, 80);
const REC_RED: Color32 = Color32::from_rgb(235, 70, 70);
const GRID_LINE: Color32 = Color32::from_rgb(34, 40, 52);

/// Timeline <-> screen mapping.
#[derive(Clone, Copy)]
struct View {
    left: f32,
    t0: f64,
    pps: f64,
    sr: f64,
}

impl View {
    fn x(&self, f: i64) -> f32 {
        self.left + ((f as f64 / self.sr - self.t0) * self.pps) as f32
    }
    fn frame(&self, x: f32) -> i64 {
        (((x - self.left) as f64 / self.pps + self.t0) * self.sr).round() as i64
    }
}

fn track_color(t: &Track) -> Color32 {
    let (r, g, b) = TRACK_COLORS[t.color % COLOR_COUNT];
    Color32::from_rgb(r, g, b)
}

/// 0:00.000 style clock.
pub(super) fn clock(secs: f32) -> String {
    let s = secs.max(0.0);
    let (h, m, r) = ((s / 3600.0).floor() as u32, ((s / 60.0).floor() as u32) % 60, s % 60.0);
    if h > 0 { format!("{h}:{m:02}:{r:06.3}") } else { format!("{m}:{r:06.3}") }
}

/// Ruler step in seconds: decimal below 10 s, 15 / 30 s and minutes above.
fn time_step(raw: f64) -> f64 {
    const S: [f64; 28] = [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 14400.0, 28800.0, 57600.0, 86400.0, 172800.0];
    S.iter().copied().find(|&s| s >= raw).unwrap_or(172800.0)
}

fn ruler_label(t: f64, step: f64) -> String {
    let (m, s) = ((t / 60.0).floor() as u32, t % 60.0);
    if step >= 1.0 {
        format!("{m}:{:02}", s.round() as u32 % 60)
    } else if step >= 0.1 {
        format!("{m}:{s:04.1}")
    } else if step >= 0.01 {
        format!("{m}:{s:05.2}")
    } else {
        format!("{m}:{s:06.3}")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Start,
    Play,
    Stop,
    Rec,
    Loop,
}

fn icon_button(ui: &mut Ui, icon: Icon, on: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
    let hov = resp.hovered();
    let accent = match icon {
        Icon::Rec => Color32::from_rgb(150, 40, 40),
        Icon::Play | Icon::Stop => Color32::from_rgb(40, 100, 70),
        _ => Color32::from_rgb(45, 85, 140),
    };
    let bg = if resp.is_pointer_button_down_on() {
        Color32::from_rgb(70, 115, 180)
    } else if on {
        accent
    } else if hov {
        Color32::from_rgb(56, 68, 88)
    } else {
        Color32::from_rgb(30, 36, 46)
    };
    let p = ui.painter();
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, if hov { Color32::from_rgb(110, 130, 160) } else { Color32::from_rgb(52, 60, 74) }), egui::StrokeKind::Inside);
    let c = rect.center();
    let col = if icon == Icon::Rec && !on { Color32::from_rgb(255, 110, 110) } else if on || hov { Color32::WHITE } else { Color32::from_rgb(195, 205, 220) };
    match icon {
        Icon::Start => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(-5.0, 5.0)], Stroke::new(1.8, col));
            p.add(Shape::convex_polygon(vec![c + vec2(5.0, -5.5), c + vec2(5.0, 5.5), c + vec2(-3.0, 0.0)], col, Stroke::NONE));
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -6.0), c + vec2(-4.0, 6.0), c + vec2(6.0, 0.0)], col, Stroke::NONE));
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, vec2(10.0, 10.0)), 1.5, col);
        }
        Icon::Rec => {
            p.circle_filled(c, 5.5, col);
        }
        Icon::Loop => {
            let st = Stroke::new(1.6, col);
            p.rect_stroke(Rect::from_center_size(c, vec2(14.0, 8.0)), 4.0, st, egui::StrokeKind::Inside);
            p.add(Shape::convex_polygon(vec![c + vec2(2.0, -7.0), c + vec2(2.0, -1.0), c + vec2(6.0, -4.0)], col, Stroke::NONE));
        }
    }
    resp.on_hover_text(tip).clicked()
}

/// Small M / S / R style toggle.
fn mini_toggle(ui: &mut Ui, text: &str, on: bool, col: Color32, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 16.0), Sense::click());
    let p = ui.painter();
    let bg = if on { col } else if resp.hovered() { Color32::from_rgb(52, 62, 80) } else { Color32::from_rgb(34, 40, 52) };
    p.rect_filled(rect, 3.0, bg);
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.5), if on { Color32::from_rgb(12, 14, 18) } else { Color32::from_rgb(175, 185, 200) });
    resp.on_hover_text(tip).clicked()
}

#[derive(Clone, Copy)]
enum SliderIcon {
    Volume,
    Pan,
}

/// Tiny glyph in front of a slider that says what it controls.
fn draw_icon(ui: &mut Ui, icon: SliderIcon) {
    let (r, resp) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
    let p = ui.painter();
    let c = r.center();
    let col = Color32::from_rgb(150, 166, 192);
    let st = Stroke::new(1.1, col);
    match icon {
        SliderIcon::Volume => {
            p.add(Shape::convex_polygon(vec![c + vec2(-5.5, -2.0), c + vec2(-2.5, -2.0), c + vec2(0.5, -4.5), c + vec2(0.5, 4.5), c + vec2(-2.5, 2.0), c + vec2(-5.5, 2.0)], col, Stroke::NONE));
            p.line_segment([c + vec2(2.5, -2.0), c + vec2(3.5, 0.0)], st);
            p.line_segment([c + vec2(3.5, 0.0), c + vec2(2.5, 2.0)], st);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(5.5, 0.0)], st);
            p.line_segment([c + vec2(5.5, 0.0), c + vec2(4.0, 4.0)], st);
        }
        SliderIcon::Pan => {
            p.line_segment([c + vec2(-4.0, 0.0), c + vec2(4.0, 0.0)], st);
            p.add(Shape::convex_polygon(vec![c + vec2(-6.0, 0.0), c + vec2(-3.0, -3.0), c + vec2(-3.0, 3.0)], col, Stroke::NONE));
            p.add(Shape::convex_polygon(vec![c + vec2(6.0, 0.0), c + vec2(3.0, -3.0), c + vec2(3.0, 3.0)], col, Stroke::NONE));
            p.line_segment([c + vec2(0.0, -4.0), c + vec2(0.0, 4.0)], Stroke::new(1.0, Color32::from_gray(90)));
        }
    }
    resp.on_hover_text(match icon {
        SliderIcon::Volume => "Volume",
        SliderIcon::Pan => "Pan: left / right",
    });
}

fn meter_bar(p: &Painter, r: Rect, level: f32) {
    p.rect_filled(r, 1.0, Color32::from_gray(30));
    let db = 20.0 * level.max(1e-6).log10();
    let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let col = if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) };
    p.rect_filled(Rect::from_min_size(pos2(r.left(), r.bottom() - r.height() * t), vec2(r.width(), r.height() * t)), 1.0, col);
}

fn hbar(p: &Painter, r: Rect, level: f32) {
    p.rect_filled(r, 1.0, Color32::from_gray(30));
    let db = 20.0 * level.max(1e-6).log10();
    let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let col = if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) };
    p.rect_filled(Rect::from_min_size(r.min, vec2(r.width() * t, r.height())), 1.0, col);
}

/// Min / max of a clip between clip-relative frames `[a, b)`; `ch` = one channel, `None` = all.
fn wave_minmax(c: &Clip, ch: Option<usize>, a: usize, b: usize) -> (f32, f32) {
    let (s, e) = if c.reverse { (c.off + c.len - b, c.off + c.len - a) } else { (c.off + a, c.off + b) };
    let chans: Vec<usize> = match ch {
        Some(k) => vec![k.min(c.src.ch.len() - 1)],
        None => (0..c.src.ch.len()).collect(),
    };
    let mut r = (0f32, 0f32);
    if e - s >= 2 * OV_BLOCK {
        let (b0, b1) = (s / OV_BLOCK, e.div_ceil(OV_BLOCK));
        for &k in &chans {
            let ov = &c.src.ov[k];
            for &(lo, hi) in &ov[b0..b1.min(ov.len())] {
                r.0 = r.0.min(lo);
                r.1 = r.1.max(hi);
            }
        }
    } else {
        for &k in &chans {
            for &v in &c.src.ch[k][s..e.min(c.src.len())] {
                r.0 = r.0.min(v);
                r.1 = r.1.max(v);
            }
        }
    }
    r
}

#[derive(Clone, Copy)]
struct Hit {
    ti: usize,
    ci: usize,
    kind: DragKind,
}

impl Daw {
    pub(super) fn draw(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let mut p = self.proj.lock().clone();
        self.edited = false;
        let whole = ui.max_rect();
        self.handle_drops(ui, whole);
        self.toolbar(ui, cx, &mut p);
        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, Sense::hover());
        let an_h = if self.an_open && avail.height() > 330.0 { (avail.height() * 0.40).clamp(150.0, 240.0) } else { 0.0 };
        let body = Rect::from_min_max(avail.min, pos2(avail.max.x, avail.max.y - an_h));
        self.keys(ui, cx, &mut p, whole);
        self.body(ui, cx, &mut p, body);
        if an_h > 0.0 {
            let an = Rect::from_min_max(pos2(avail.min.x, avail.max.y - an_h + 4.0), avail.max);
            self.analysis_panel(ui, cx, &p, an);
            self.schedule_analysis(&p);
        }
        if self.edited {
            *self.proj.lock() = p;
            self.rev += 1;
        }
    }

    fn handle_drops(&mut self, ui: &mut Ui, whole: Rect) {
        let (hover, dropped) = ui.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>()));
        if hover && ui.rect_contains_pointer(whole) {
            ui.painter().rect_stroke(whole.shrink(2.0), 6.0, Stroke::new(2.0, Color32::from_rgb(110, 170, 255)), egui::StrokeKind::Inside);
            ui.painter().text(whole.center(), Align2::CENTER_CENTER, "Drop audio files to add them as tracks", FontId::proportional(16.0), Color32::from_rgb(160, 200, 255));
        }
        if !dropped.is_empty() && ui.rect_contains_pointer(whole) {
            self.import(dropped);
        }
    }

    // ------------------------------------------------------------------ toolbar

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &mut Project) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            if icon_button(ui, Icon::Start, false, "To the start (Home)") {
                self.seek(0);
            }
            let playing = self.play.is_some();
            if icon_button(ui, if playing { Icon::Stop } else { Icon::Play }, playing && self.rec.is_none(), "Play / stop (Space)") {
                self.toggle_play(cx);
            }
            if icon_button(ui, Icon::Rec, self.rec.is_some(), "Record on every armed track (R). Arms a track on the default input if none is armed.") {
                if self.rec.is_some() {
                    self.stop_record(cx);
                } else {
                    self.start_record(cx);
                }
            }
            let recording = self.rec.is_some();
            ui.label(RichText::new(clock(self.pos() as f32 / p.sr as f32)).monospace().size(15.0).strong().color(if recording { REC_RED } else { Color32::from_rgb(225, 232, 245) }));
            if icon_button(ui, Icon::Loop, self.loop_on, "Loop the selection (or the whole project) (L)") {
                self.loop_on = !self.loop_on;
            }
            ui.toggle_value(&mut self.follow, "Follow").on_hover_text("Scroll the view with the playhead");
            ui.toggle_value(&mut self.snap, "Snap").on_hover_text("Snap clips, the playhead and selections to the grid, clip edges and the playhead (hold Alt to bypass)");
            ui.separator();
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo").small()).on_hover_text("Undo the last timeline edit (Ctrl+Z)").clicked() {
                self.undo(p);
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo").small()).on_hover_text("Redo (Ctrl+Y)").clicked() {
                self.redo(p);
            }
            ui.separator();
            if ui.small_button("-").on_hover_text("Zoom out (mouse wheel over the tracks)").clicked() {
                self.zoom_by(0.7, None);
            }
            if ui.small_button("+").on_hover_text("Zoom in (mouse wheel over the tracks; Shift+wheel scrolls sideways)").clicked() {
                self.zoom_by(1.4, None);
            }
            if ui.small_button("Fit").on_hover_text("Show the whole project").clicked() {
                self.fit_view();
            }
            ui.separator();
            ui.menu_button("+ Track", |ui| {
                if ui.button("Empty track").clicked() {
                    self.checkpoint(p);
                    let n = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
                    self.sel_track = Some(p.tracks[n].id);
                    ui.close();
                }
                ui.separator();
                if graph_on() {
                    if ui.button("Track that records from the Routing window").on_hover_text("An armed track plus a Timeline record node to connect").clicked() {
                        self.checkpoint(p);
                        let n = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
                        let id = p.tracks[n].id;
                        p.tracks[n].input = graph_input(id);
                        p.tracks[n].arm = true;
                        crate::graph::bus().request_rec_node(id, &p.tracks[n].name);
                        self.sel_track = Some(id);
                        ui.close();
                    }
                    return;
                }
                ui.label(RichText::new("Record from an input (armed)").small().weak());
                if self.devs_v.is_empty() {
                    ui.label(RichText::new("scanning inputs...").small().weak());
                }
                let devs = self.devs_v.clone();
                for d in devs {
                    ui.menu_button(capture::short_label(&d.label), |ui| {
                        ui.label(RichText::new(format!("{} ch, {} Hz", d.ch, d.sr)).small().weak());
                        if ui.button(format!("One mono track per channel ({})", d.ch.min(32))).clicked() {
                            self.checkpoint(p);
                            for c in 0..d.ch.min(32) {
                                self.add_input_track(p, &d, Mode::Chan(c), Some(format!("In {}", c + 1)));
                            }
                            ui.close();
                        }
                        if d.ch >= 2 && ui.button(format!("Stereo pairs ({})", d.ch / 2)).clicked() {
                            self.checkpoint(p);
                            for k in 0..(d.ch / 2).min(16) {
                                self.add_input_track(p, &d, Mode::Pair(k), Some(format!("In {}+{}", 2 * k + 1, 2 * k + 2)));
                            }
                            ui.close();
                        }
                        if ui.button("Mono mix of all channels").clicked() {
                            self.checkpoint(p);
                            self.add_input_track(p, &d, Mode::Mix, Some(format!("{} mix", capture::short_label(&d.label))));
                            ui.close();
                        }
                    });
                }
            });
            if ui.button("Import...").on_hover_text("Add audio files (WAV, FLAC, MP3 ...) as tracks. Files can also be dropped on the window.").clicked() && self.open_dlg.is_none() {
                self.open_dlg = Some(dialogs::open_many("Add audio files", "Audio files", decode::EXTENSIONS));
            }
            ui.menu_button("Edit", |ui| self.edit_menu(ui, p));
            ui.menu_button("Export", |ui| {
                let range = self.range;
                if ui.button(if range.is_some() { "Mix of the selection to WAV..." } else { "Mix to WAV..." }).clicked() {
                    self.ask_export(Export::Mix(range));
                    ui.close();
                }
                if ui.button("Every track as its own WAV...").clicked() {
                    self.ask_export(Export::StemsDir);
                    ui.close();
                }
                if let Some(id) = self.sel.iter().next().copied() {
                    if ui.button("Selected clip to WAV...").clicked() {
                        self.ask_export(Export::Clip(id));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Bounce mix to a new track").on_hover_text("Render the selection (or everything) into one new track").clicked() {
                    self.act_bounce(p, false);
                    ui.close();
                }
                if ui.add_enabled(self.sel_track.is_some(), egui::Button::new("Bounce selected track")).clicked() {
                    self.act_bounce(p, true);
                    ui.close();
                }
            });
            ui.menu_button("Project", |ui| {
                if ui.button("New").clicked() {
                    self.new_project(cx);
                    p.tracks.clear();
                    ui.close();
                }
                if ui.button("Open...").clicked() {
                    self.open_clicked();
                    ui.close();
                }
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.save_clicked(false);
                    ui.close();
                }
                if ui.button("Save as...").clicked() {
                    self.save_clicked(true);
                    ui.close();
                }
                ui.separator();
                if ui.button("Add demo tracks").on_hover_text("Tone, sweep, noise and kick: something to edit and analyse").clicked() {
                    self.demo();
                    ui.close();
                }
                ui.label(RichText::new(format!("{} Hz project", p.sr)).small().weak());
                if let Some(path) = &self.path {
                    ui.label(RichText::new(path.display().to_string()).small().weak());
                }
            });
            ui.toggle_value(&mut self.an_open, "Analysis").on_hover_text("Statistics, spectrum and spectrogram of the selection");
            ui.menu_button("Output", |ui| {
                if graph_on() {
                    ui.label("The timeline plays into the Routing window:\nconnect its Timeline out node to an output or the analyser.");
                    ui.add(egui::DragValue::new(&mut self.rec_off_ms).range(-500.0..=500.0).speed(0.5).suffix(" ms")).on_hover_text("Shift new recordings earlier by this much to make up for the interface latency");
                    ui.label(RichText::new("recording latency compensation").small().weak());
                    return;
                }
                ui.checkbox(&mut self.sound, "Play through a sound device");
                ui.add_enabled_ui(self.sound, |ui| {
                    let cur = cx.out_devs.iter().find(|d| d.id == self.mon_dev).or(cx.out_devs.first()).map(|d| d.label.clone()).unwrap_or_else(|| "no output".into());
                    egui::ComboBox::from_id_salt("daw_out").width(220.0).selected_text(cur).show_ui(ui, |ui| {
                        for d in cx.out_devs {
                            ui.selectable_value(&mut self.mon_dev, d.id.clone(), &d.label);
                        }
                    });
                    ui.add(egui::Slider::new(&mut self.mon_vol, 0.0..=1.0).text("volume"));
                });
                ui.separator();
                ui.checkbox(&mut self.feed_analysers, "Show the timeline in the analysers").on_hover_text("Off: playing leaves the live input in the spectrum, scope, loudness ... windows.\nOn: they follow the timeline while it plays (the live input pauses).\n'Loop & play' in the analysis panel always does this.");
                ui.add_enabled_ui(self.feed_analysers, |ui| {
                    ui.radio_value(&mut self.an_src, AnSrc::Master, "the master mix");
                    ui.radio_value(&mut self.an_src, AnSrc::Track, "the selected track");
                });
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.rec_off_ms).range(-500.0..=500.0).speed(0.5).suffix(" ms")).on_hover_text("Shift new recordings earlier by this much to make up for the interface latency");
                ui.label(RichText::new("recording latency compensation").small().weak());
                if ui.button("Rescan input devices").clicked() {
                    let d2 = self.devs.clone();
                    std::thread::Builder::new().name("daw-probe".into()).spawn(move || *d2.lock() = Some(capture::probe_all())).ok();
                }
            });
            // master
            ui.separator();
            let (mr, _) = ui.allocate_exact_size(vec2(54.0, 14.0), Sense::hover());
            hbar(ui.painter(), Rect::from_min_size(mr.min + vec2(0.0, 1.0), vec2(mr.width(), 5.0)), self.master_level[0]);
            hbar(ui.painter(), Rect::from_min_size(mr.min + vec2(0.0, 8.0), vec2(mr.width(), 5.0)), self.master_level[1]);
            ui.spacing_mut().slider_width = 60.0;
            draw_icon(ui, SliderIcon::Volume);
            let tip = format!("Master volume {:+.1} dB", p.master_db);
            if self.fader(ui, "master", &mut p.master_db, -40.0..=12.0, 0.0, 1.0, &tip) {
                self.touch();
            }
            if self.busy.load(Relaxed) > 0 {
                ui.spinner();
            }
        });
    }

    fn add_input_track(&mut self, p: &mut Project, d: &DevEx, mode: Mode, name: Option<String>) {
        if p.tracks.len() >= MAX_TRACKS {
            return;
        }
        let ti = p.add_track(name, Input { dev: d.id.clone(), mode });
        p.tracks[ti].arm = true;
        p.tracks[ti].h = 0;
        self.sel_track = Some(p.tracks[ti].id);
    }

    fn edit_menu(&mut self, ui: &mut Ui, p: &mut Project) {
        let has_sel = !self.sel.is_empty();
        let item = |ui: &mut Ui, text: &str, key: &str, enabled: bool| ui.add_enabled(enabled, egui::Button::new(text).shortcut_text(key)).clicked();
        if item(ui, "Undo", "Ctrl+Z", !self.undo.is_empty()) {
            self.undo(p);
            ui.close();
        }
        if item(ui, "Redo", "Ctrl+Y", !self.redo.is_empty()) {
            self.redo(p);
            ui.close();
        }
        ui.separator();
        if item(ui, "Split at the playhead", "S", true) {
            self.act_split(p);
            ui.close();
        }
        if item(ui, "Delete", "Del", has_sel || self.range.is_some()) {
            self.act_delete(p);
            ui.close();
        }
        if item(ui, "Duplicate", "Ctrl+D", has_sel) {
            self.act_duplicate(p);
            ui.close();
        }
        if item(ui, "Cut", "Ctrl+X", has_sel) {
            self.act_copy(p, true);
            ui.close();
        }
        if item(ui, "Copy", "Ctrl+C", has_sel) {
            self.act_copy(p, false);
            ui.close();
        }
        if item(ui, "Paste at the playhead", "Ctrl+V", !self.clip_buf.is_empty()) {
            self.act_paste(p);
            ui.close();
        }
        if item(ui, "Select all clips", "Ctrl+A", true) {
            self.act_select_all(p);
            ui.close();
        }
        ui.separator();
        if item(ui, "Trim clips to the selection", "", self.range.is_some()) {
            self.act_trim_to_range(p);
            ui.close();
        }
        if item(ui, "Normalize to -1 dBFS", "N", has_sel) {
            self.act_normalize(p);
            ui.close();
        }
        if item(ui, "Reverse", "", has_sel) {
            self.act_clip_prop(p, |c| c.reverse = !c.reverse);
            ui.close();
        }
        if item(ui, "Mute / unmute clip", "M", has_sel) {
            self.act_clip_prop(p, |c| c.muted = !c.muted);
            ui.close();
        }
        ui.menu_button("Gain", |ui| {
            for (t, d) in [("+1 dB", 1.0f32), ("-1 dB", -1.0), ("+6 dB", 6.0), ("-6 dB", -6.0)] {
                if ui.add_enabled(has_sel, egui::Button::new(t)).clicked() {
                    self.act_clip_prop(p, |c| c.gain_db = (c.gain_db + d).clamp(-90.0, 36.0));
                    ui.close();
                }
            }
            if ui.add_enabled(has_sel, egui::Button::new("Reset to 0 dB")).clicked() {
                self.act_clip_prop(p, |c| c.gain_db = 0.0);
                ui.close();
            }
        });
        ui.menu_button("Fades", |ui| {
            let sr = p.sr as f32;
            for (t, s) in [("10 ms", 0.01f32), ("100 ms", 0.1), ("500 ms", 0.5), ("2 s", 2.0)] {
                if ui.add_enabled(has_sel, egui::Button::new(format!("In and out {t}"))).clicked() {
                    self.act_clip_prop(p, |c| {
                        c.fade_in = (s * sr) as usize;
                        c.fade_out = (s * sr) as usize;
                        c.clamp_fades();
                    });
                    ui.close();
                }
            }
            if ui.add_enabled(has_sel, egui::Button::new("Remove fades")).clicked() {
                self.act_clip_prop(p, |c| {
                    c.fade_in = 0;
                    c.fade_out = 0;
                });
                ui.close();
            }
        });
    }

    // ------------------------------------------------------------------ keyboard

    fn keys(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &mut Project, whole: Rect) {
        let ctx = ui.ctx().clone();
        if !ui.rect_contains_pointer(whole) || ctx.egui_wants_keyboard_input() {
            return;
        }
        use egui::{Key, Modifiers};
        let key = |m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
        let (none, ctrl, ctrl_shift) = (Modifiers::NONE, Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT);
        if key(none, Key::Space) {
            self.toggle_play(cx);
        }
        if key(none, Key::R) {
            if self.rec.is_some() {
                self.stop_record(cx);
            } else {
                self.start_record(cx);
            }
        }
        if key(none, Key::S) {
            self.act_split(p);
        }
        if key(none, Key::Delete) || key(none, Key::Backspace) {
            self.act_delete(p);
        }
        if key(none, Key::L) {
            self.loop_on = !self.loop_on;
        }
        if key(none, Key::M) {
            self.act_clip_prop(p, |c| c.muted = !c.muted);
        }
        if key(none, Key::N) {
            self.act_normalize(p);
        }
        if key(none, Key::Home) {
            self.seek(0);
            self.t0 = 0.0;
        }
        if key(none, Key::End) {
            let e = p.end_frame();
            self.seek(e);
        }
        if key(none, Key::Escape) {
            self.sel.clear();
            self.range = None;
        }
        if key(none, Key::Plus) || key(none, Key::Equals) {
            self.zoom_by(1.4, None);
        }
        if key(none, Key::Minus) {
            self.zoom_by(0.7, None);
        }
        if key(ctrl, Key::D) {
            self.act_duplicate(p);
        }
        if key(ctrl, Key::A) {
            self.act_select_all(p);
        }
        if key(ctrl, Key::C) {
            self.act_copy(p, false);
        }
        if key(ctrl, Key::X) {
            self.act_copy(p, true);
        }
        if key(ctrl, Key::V) {
            self.act_paste(p);
        }
        if key(ctrl, Key::S) {
            self.save_clicked(false);
        }
        if key(ctrl, Key::Z) {
            self.undo(p);
        }
        if key(ctrl, Key::Y) || key(ctrl_shift, Key::Z) {
            self.redo(p);
        }
    }

    fn zoom_by(&mut self, f: f64, anchor_x: Option<(f32, f32)>) {
        // anchor: (screen x, lanes left); default is the middle of the visible part
        let w = self.lane_w as f64;
        let (ax, left) = anchor_x.unwrap_or((self.lane_w * 0.5, 0.0));
        let t_anchor = self.t0 + (ax - left) as f64 / self.pps;
        self.pps = (self.pps * f).clamp(0.5, 60000.0);
        self.t0 = (t_anchor - (ax - left) as f64 / self.pps).max(0.0);
        let _ = w;
    }

    // ------------------------------------------------------------------ snapping

    /// Candidate positions to snap to (frames), apart from the grid.
    fn snap_points(&self, p: &Project, exclude: &[u32]) -> Vec<i64> {
        let mut v = vec![0, self.pos()];
        if let Some((a, b)) = self.range {
            v.push(a);
            v.push(b);
        }
        for c in p.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| !exclude.contains(&c.id)) {
            v.push(c.start);
            v.push(c.end());
        }
        v
    }

    fn snap_to(&self, view: &View, pts: &[i64], f: i64, alt: bool) -> i64 {
        if !self.snap || alt {
            return f;
        }
        let tol = (8.0 / view.pps * view.sr) as i64;
        let step = time_step(70.0 / view.pps) * view.sr;
        let grid = ((f as f64 / step).round() * step) as i64;
        let mut best = (tol + 1, f);
        for &c in pts.iter().chain(std::iter::once(&grid)) {
            let d = (c - f).abs();
            if d < best.0 {
                best = (d, c);
            }
        }
        if best.0 <= tol { best.1 } else { f }
    }

    // ------------------------------------------------------------------ the main area

    fn hit(&self, p: &Project, view: &View, rows: &[Rect], pos: Pos2) -> Option<Hit> {
        let ti = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom())?;
        let row = rows[ti];
        for (ci, c) in p.tracks[ti].clips.iter().enumerate().rev() {
            let (x0, x1) = (view.x(c.start), view.x(c.end()));
            if pos.x < x0 - 2.0 || pos.x > x1 + 2.0 {
                continue;
            }
            let w = x1 - x0;
            let edge = (w / 3.0).clamp(2.0, 7.0);
            let kind = if pos.x - x0 <= edge {
                DragKind::TrimL
            } else if x1 - pos.x <= edge {
                DragKind::TrimR
            } else if w >= 60.0 && pos.y < row.top() + 16.0 && pos.x - x0 < 22.0 {
                DragKind::FadeIn
            } else if w >= 60.0 && pos.y < row.top() + 16.0 && x1 - pos.x < 22.0 {
                DragKind::FadeOut
            } else {
                DragKind::Move
            };
            return Some(Hit { ti, ci, kind });
        }
        None
    }

    fn row_at(rows: &[Rect], y: f32) -> usize {
        if rows.is_empty() {
            return 0;
        }
        rows.iter().position(|r| y < r.bottom()).unwrap_or(rows.len() - 1)
    }

    fn body(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &mut Project, rect: Rect) {
        if rect.height() < 60.0 || rect.width() < 200.0 {
            return;
        }
        let sr = p.sr as f64;
        let lanes_left = rect.left() + HEADER_W;
        self.lane_w = (rect.right() - lanes_left).max(50.0);
        // follow the playhead
        if self.play.is_some() && self.follow && self.drag.is_none() {
            let t = self.pos() as f64 / sr;
            let span = self.lane_w as f64 / self.pps;
            if t > self.t0 + span * 0.92 || t < self.t0 {
                self.t0 = (t - span * 0.08).max(0.0);
            }
        }
        if self.fit_pending {
            self.fit_pending = false;
            self.fit_view();
        }
        let view = View { left: lanes_left, t0: self.t0, pps: self.pps, sr };
        let ruler = Rect::from_min_max(pos2(lanes_left, rect.top()), pos2(rect.right(), rect.top() + RULER_H));
        let tracks_rect = Rect::from_min_max(pos2(rect.left(), ruler.bottom()), rect.max);
        let lanes_area = Rect::from_min_max(pos2(lanes_left, ruler.bottom()), rect.max);
        // geometry of the rows
        let total_h: f32 = p.tracks.iter().map(|t| t.height()).sum::<f32>() + ADD_ROW_H;
        let max_scroll = (total_h - tracks_rect.height()).max(0.0);
        self.vscroll = self.vscroll.clamp(0.0, max_scroll);
        let mut rows: Vec<Rect> = Vec::with_capacity(p.tracks.len());
        let mut y = tracks_rect.top() - self.vscroll;
        for t in &p.tracks {
            rows.push(Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + t.height())));
            y += t.height();
        }
        let add_row = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + ADD_ROW_H));

        // ---- wheel: zoom / pan / scroll
        let over = ui.rect_contains_pointer(rect);
        if over && self.drag.is_none() {
            let (scroll, mods, hover) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers, i.pointer.hover_pos()));
            if scroll != egui::Vec2::ZERO {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                // lanes: wheel zooms, Shift pans, Alt scrolls the tracks; over the headers the wheel scrolls the tracks
                let on_lanes = hover.is_some_and(|h| h.x >= lanes_left);
                if mods.shift || scroll.x != 0.0 {
                    let d = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                    self.t0 = (self.t0 - d as f64 / self.pps).max(0.0);
                } else if mods.alt || !on_lanes {
                    self.vscroll = (self.vscroll - scroll.y).clamp(0.0, max_scroll);
                } else {
                    let ax = hover.map_or(lanes_left, |h| h.x.max(lanes_left));
                    self.zoom_by((scroll.y as f64 * 0.003).exp(), Some((ax, lanes_left)));
                }
            }
        }
        let view = View { t0: self.t0, pps: self.pps, ..view };

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BG_DARK);

        // ---- interaction on the lanes
        let lane_resp = ui.interact(lanes_area, ui.scope_id().with("daw_lanes"), Sense::click_and_drag());
        let alt = ui.input(|i| i.modifiers.alt);
        let shift = ui.input(|i| i.modifiers.shift);
        let hover = lane_resp.hover_pos();
        if let Some(h) = hover {
            if self.drag.is_none() {
                match self.hit(p, &view, &rows, h) {
                    Some(Hit { kind: DragKind::TrimL | DragKind::TrimR, .. }) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                    Some(Hit { kind: DragKind::FadeIn | DragKind::FadeOut, .. }) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                    Some(_) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                    None => ui.ctx().set_cursor_icon(CursorIcon::Text),
                }
            }
        }
        if lane_resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                match self.hit(p, &view, &rows, pos) {
                    Some(h) => {
                        let id = p.tracks[h.ti].clips[h.ci].id;
                        if !self.sel.contains(&id) {
                            if !shift {
                                self.sel.clear();
                            }
                            self.sel.insert(id);
                        }
                        self.sel_track = Some(p.tracks[h.ti].id);
                        self.checkpoint(p);
                        let orig: Vec<(Clip, usize)> = if h.kind == DragKind::Move {
                            self.sel.iter().filter_map(|i| p.find_clip(*i).map(|(ti, ci)| (p.tracks[ti].clips[ci].clone(), ti))).collect()
                        } else {
                            vec![(p.tracks[h.ti].clips[h.ci].clone(), h.ti)]
                        };
                        self.drag = Some(ClipDrag { kind: h.kind, press_frame: view.frame(pos.x), press_track: h.ti, grab: id, orig });
                    }
                    None => {
                        let f = view.frame(pos.x);
                        let pts = self.snap_points(p, &[]);
                        self.range_anchor = Some(self.snap_to(&view, &pts, f, alt).max(0));
                        if let Some(ti) = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()) {
                            self.sel_track = Some(p.tracks[ti].id);
                        }
                    }
                }
            }
        }
        if lane_resp.dragged() {
            if let Some(ptr) = lane_resp.interact_pointer_pos() {
                if let Some(d) = self.drag.take() {
                    self.update_clip_drag(p, &view, &rows, &d, ptr, alt);
                    self.drag = Some(d);
                } else if let Some(a) = self.range_anchor {
                    let pts = self.snap_points(p, &[]);
                    let f = self.snap_to(&view, &pts, view.frame(ptr.x), alt).max(0);
                    self.range = if (f - a).abs() as f64 / sr * self.pps >= 3.0 { Some((a.min(f), a.max(f))) } else { None };
                    // keep the pointer in reach: scroll when dragging past the edges
                    if ptr.x > rect.right() - 12.0 {
                        self.t0 += 20.0 / self.pps;
                    } else if ptr.x < lanes_left + 12.0 {
                        self.t0 = (self.t0 - 20.0 / self.pps).max(0.0);
                    }
                }
            }
        }
        if lane_resp.drag_stopped() {
            self.drag = None;
            self.range_anchor = None;
        }
        if lane_resp.clicked() {
            if let Some(pos) = lane_resp.interact_pointer_pos() {
                match self.hit(p, &view, &rows, pos) {
                    Some(h) => {
                        let id = p.tracks[h.ti].clips[h.ci].id;
                        if shift {
                            if !self.sel.remove(&id) {
                                self.sel.insert(id);
                            }
                        } else {
                            self.sel.clear();
                            self.sel.insert(id);
                        }
                        self.sel_track = Some(p.tracks[h.ti].id);
                    }
                    None => {
                        let pts = self.snap_points(p, &[]);
                        let f = self.snap_to(&view, &pts, view.frame(pos.x), alt).max(0);
                        self.seek(f);
                        if !shift {
                            self.sel.clear();
                            self.range = None;
                        }
                        self.sel_track = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()).map(|ti| p.tracks[ti].id);
                    }
                }
            }
        }
        if lane_resp.double_clicked() {
            if let Some(pos) = lane_resp.interact_pointer_pos() {
                if let Some(h) = self.hit(p, &view, &rows, pos) {
                    let c = &p.tracks[h.ti].clips[h.ci];
                    self.range = Some((c.start, c.end()));
                    self.sel.clear();
                    self.sel.insert(c.id);
                }
            }
        }
        if lane_resp.secondary_clicked() {
            if let Some(pos) = lane_resp.interact_pointer_pos() {
                self.ctx_clip = self.hit(p, &view, &rows, pos).map(|h| p.tracks[h.ti].clips[h.ci].id);
                if let Some(id) = self.ctx_clip {
                    if !self.sel.contains(&id) {
                        self.sel.clear();
                        self.sel.insert(id);
                    }
                }
                if let Some(ti) = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()) {
                    self.sel_track = Some(p.tracks[ti].id);
                }
                self.ctx_frame = self.snap_to(&view, &self.snap_points(p, &[]), view.frame(pos.x), alt).max(0);
            }
        }
        lane_resp.context_menu(|ui| {
            if self.ctx_clip.is_some() {
                self.edit_menu(ui, p);
                if let Some(id) = self.ctx_clip {
                    ui.separator();
                    if ui.button("Select the clip's time range").clicked() {
                        if let Some((ti, ci)) = p.find_clip(id) {
                            self.range = Some((p.tracks[ti].clips[ci].start, p.tracks[ti].clips[ci].end()));
                        }
                        ui.close();
                    }
                }
            } else {
                if let Some(ti) = self.sel_track.and_then(|t| p.track_index(t)) {
                    let (tid, f) = (p.tracks[ti].id, self.ctx_frame);
                    if ui.button(format!("Import audio file here ({})...", p.tracks[ti].name)).clicked() && self.open_dlg.is_none() {
                        self.import_target = Some((tid, f));
                        self.open_dlg = Some(dialogs::open_many("Add audio to the track", "Audio files", decode::EXTENSIONS));
                        ui.close();
                    }
                    if !graph_on() {
                        ui.menu_button("Record input of this track", |ui| self.input_menu(ui, p, ti));
                    }
                    ui.separator();
                }
                if ui.button("Split everything here (S)").clicked() {
                    self.act_split(p);
                    ui.close();
                }
                if ui.add_enabled(!self.clip_buf.is_empty(), egui::Button::new("Paste")).clicked() {
                    self.act_paste(p);
                    ui.close();
                }
                if ui.add_enabled(self.range.is_some(), egui::Button::new("Delete the selected time range")).clicked() {
                    self.act_delete(p);
                    ui.close();
                }
                if ui.button("Select all (Ctrl+A)").clicked() {
                    self.act_select_all(p);
                    ui.close();
                }
            }
        });

        // ---- ruler interaction (seek)
        let ruler_resp = ui.interact(ruler, ui.scope_id().with("daw_ruler"), Sense::click_and_drag());
        if ruler_resp.clicked() || ruler_resp.dragged() {
            if let Some(pos) = ruler_resp.interact_pointer_pos() {
                let pts = self.snap_points(p, &[]);
                let f = self.snap_to(&view, &pts, view.frame(pos.x.max(lanes_left)), alt).max(0);
                self.seek(f);
            }
        }

        // ---- lanes
        let lane_clip = lanes_area;
        let selected_ids = self.sel.clone();
        let recording = self.rec.is_some();
        for (ti, t) in p.tracks.iter().enumerate() {
            let row = rows[ti];
            if row.bottom() < tracks_rect.top() || row.top() > tracks_rect.bottom() {
                continue;
            }
            let lane = Rect::from_min_max(pos2(lanes_left, row.top()), pos2(rect.right(), row.bottom()));
            let lp = painter.with_clip_rect(lane.intersect(lane_clip));
            lp.rect_filled(lane, 0.0, if ti % 2 == 0 { LANE_A } else { LANE_B });
            if self.sel_track == Some(t.id) {
                lp.rect_filled(lane, 0.0, Color32::from_white_alpha(6));
            }
            self.draw_grid(&lp, &view, lane);
            let col = track_color(t);
            let chans: &[Option<usize>] = match t.chan {
                Chan::Stereo if t.h >= 2 => &[Some(0), Some(1)],
                Chan::Left => &[Some(0)],
                Chan::Right => &[Some(1)],
                _ => &[None],
            };
            let dimmed = t.mute || (p.tracks.iter().any(|o| o.solo) && !t.solo);
            for c in &t.clips {
                self.draw_clip(&lp, &view, lane, c, col, selected_ids.contains(&c.id), chans, t.ch_mute, dimmed);
            }
            if recording {
                self.draw_rec_preview(&lp, &view, lane, t);
            }
            painter.line_segment([pos2(rect.left(), row.bottom()), pos2(rect.right(), row.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
        }
        // empty space below the tracks
        if add_row.top() < rect.bottom() {
            let below = Rect::from_min_max(pos2(lanes_left, add_row.top()), rect.max);
            painter.with_clip_rect(tracks_rect).rect_filled(below, 0.0, BG_DARK);
        }

        // ---- time selection, loop, playhead
        let pl = painter.with_clip_rect(lanes_area);
        if let Some((a, b)) = self.range {
            let (xa, xb) = (view.x(a), view.x(b));
            let r = Rect::from_min_max(pos2(xa, lanes_area.top()), pos2(xb, lanes_area.bottom()));
            pl.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 160, 255, 34));
            pl.line_segment([pos2(xa, lanes_area.top()), pos2(xa, lanes_area.bottom())], Stroke::new(1.0, Color32::from_rgba_unmultiplied(140, 185, 255, 160)));
            pl.line_segment([pos2(xb, lanes_area.top()), pos2(xb, lanes_area.bottom())], Stroke::new(1.0, Color32::from_rgba_unmultiplied(140, 185, 255, 160)));
        }
        let px = view.x(self.pos());
        if px >= lanes_left - 1.0 && px <= rect.right() + 1.0 {
            pl.line_segment([pos2(px, lanes_area.top()), pos2(px, lanes_area.bottom())], Stroke::new(1.3, PLAYHEAD));
        }
        // vertical scroll indicator
        if max_scroll > 0.0 {
            let h = tracks_rect.height();
            let th = (h * h / total_h).max(24.0);
            let top = tracks_rect.top() + (h - th) * (self.vscroll / max_scroll);
            painter.rect_filled(Rect::from_min_size(pos2(rect.right() - 5.0, top), vec2(3.0, th)), 1.5, Color32::from_white_alpha(40));
        }

        // ---- headers
        let hp = painter.with_clip_rect(Rect::from_min_max(tracks_rect.min, pos2(lanes_left, tracks_rect.max.y)));
        hp.rect_filled(Rect::from_min_max(tracks_rect.min, pos2(lanes_left, tracks_rect.max.y)), 0.0, HDR_BG);
        let mut remove: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        let mut dup: Option<usize> = None;
        for ti in 0..p.tracks.len() {
            let row = rows[ti];
            if row.bottom() < tracks_rect.top() || row.top() > tracks_rect.bottom() {
                continue;
            }
            let hr = Rect::from_min_max(pos2(rect.left(), row.top()), pos2(lanes_left - 1.0, row.bottom() - 1.0));
            self.track_header(ui, p, ti, hr, tracks_rect, &mut remove, &mut swap, &mut dup);
        }
        // "+" row
        if add_row.bottom() > tracks_rect.top() && add_row.top() < tracks_rect.bottom() {
            let ar = Rect::from_min_max(pos2(rect.left() + 6.0, add_row.top() + 5.0), pos2(lanes_left - 7.0, add_row.bottom() - 5.0));
            let r = ui.interact(ar.intersect(tracks_rect), ui.scope_id().with("daw_add"), Sense::click());
            hp.rect_filled(ar, 4.0, if r.hovered() { Color32::from_rgb(44, 54, 72) } else { Color32::from_rgb(32, 39, 52) });
            hp.text(ar.center(), Align2::CENTER_CENTER, "+ Add track", FontId::proportional(11.0), Color32::from_rgb(170, 185, 205));
            if r.on_hover_text("Add an empty track").clicked() && p.tracks.len() < MAX_TRACKS {
                self.checkpoint(p);
                let n = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
                self.sel_track = Some(p.tracks[n].id);
            }
        }
        if let Some(ti) = remove {
            self.checkpoint(p);
            let id = p.tracks[ti].id;
            let gone: Vec<u32> = p.tracks[ti].clips.iter().map(|c| c.id).collect();
            p.tracks.remove(ti);
            self.sel.retain(|c| !gone.contains(c));
            if self.sel_track == Some(id) {
                self.sel_track = None;
            }
        }
        if let Some((a, b)) = swap {
            self.checkpoint(p);
            p.tracks.swap(a, b);
        }
        if let Some(tid) = self.split_req.take() {
            if let Some(ti) = p.track_index(tid) {
                self.checkpoint(p);
                if let Some(n) = p.split_channels(ti) {
                    self.sel_track = Some(p.tracks[n].id);
                }
            }
        }
        if let Some(ti) = dup {
            self.checkpoint(p);
            let mut t = p.tracks[ti].clone();
            t.id = p.next();
            t.name = format!("{} copy", t.name);
            t.arm = false;
            for c in t.clips.iter_mut() {
                c.id = p.next();
            }
            p.tracks.insert(ti + 1, t);
        }

        // ---- ruler (drawn last: it covers the lanes when scrolled)
        self.draw_ruler(&painter, &view, ruler, rect);
        painter.rect_filled(Rect::from_min_max(rect.min, pos2(lanes_left - 1.0, ruler.bottom())), 0.0, Color32::from_rgb(30, 36, 48));
        painter.text(pos2(rect.left() + 8.0, ruler.center().y), Align2::LEFT_CENTER, format!("{} tracks", p.tracks.len()), FontId::proportional(10.0), Color32::from_rgb(130, 142, 160));
        painter.line_segment([pos2(lanes_left - 1.0, rect.top()), pos2(lanes_left - 1.0, rect.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
        // empty project hint
        if p.tracks.is_empty() {
            painter.text(
                lanes_area.center(),
                Align2::CENTER_CENTER,
                "Press R to record from the default input,\nor drop audio files here / use Import...",
                FontId::proportional(14.0),
                Color32::from_rgb(110, 122, 140),
            );
        }
        // hover readout
        if let (Some(h), None) = (hover, &self.drag) {
            if lanes_area.contains(h) {
                let t = view.frame(h.x).max(0) as f64 / sr;
                painter.text(pos2(h.x + 8.0, ruler.bottom() + 2.0), Align2::LEFT_TOP, clock(t as f32), FontId::monospace(9.5), Color32::from_white_alpha(120));
            }
        }
        let _ = (cx, add_row);
    }

    fn update_clip_drag(&mut self, p: &mut Project, view: &View, rows: &[Rect], d: &ClipDrag, ptr: Pos2, alt: bool) {
        let ids: Vec<u32> = d.orig.iter().map(|o| o.0.id).collect();
        let pts = self.snap_points(p, &ids);
        let now = view.frame(ptr.x);
        let raw_dt = now - d.press_frame;
        let grab = d.orig.iter().find(|o| o.0.id == d.grab).map(|o| &o.0);
        let Some(grab) = grab else { return };
        match d.kind {
            DragKind::Move => {
                // snap the start or the end of the grabbed clip, whichever is closer
                let s = self.snap_to(view, &pts, grab.start + raw_dt, alt);
                let e = self.snap_to(view, &pts, grab.end() + raw_dt, alt);
                let mut dt = if (s - (grab.start + raw_dt)).abs() <= (e - (grab.end() + raw_dt)).abs() { s - grab.start } else { e - grab.end() };
                let min_start = d.orig.iter().map(|o| o.0.start).min().unwrap_or(0);
                dt = dt.max(-min_start);
                let ti_now = Self::row_at(rows, ptr.y) as isize;
                let mut dtr = ti_now - d.press_track as isize;
                let (lo, hi) = d.orig.iter().fold((isize::MAX, isize::MIN), |(l, h), o| (l.min(o.1 as isize), h.max(o.1 as isize)));
                dtr = dtr.clamp(-lo, p.tracks.len() as isize - 1 - hi);
                for (c, ti) in &d.orig {
                    for t in p.tracks.iter_mut() {
                        t.clips.retain(|x| x.id != c.id);
                    }
                    let mut n = c.clone();
                    n.start = c.start + dt;
                    p.tracks[(*ti as isize + dtr) as usize].clips.push(n);
                }
                self.sel_track = Some(p.tracks[(d.press_track as isize + dtr) as usize].id);
            }
            DragKind::TrimL | DragKind::TrimR => {
                let mut c = grab.clone();
                if d.kind == DragKind::TrimL {
                    let t = self.snap_to(view, &pts, grab.start + raw_dt, alt);
                    c.set_start(t);
                } else {
                    let t = self.snap_to(view, &pts, grab.end() + raw_dt, alt);
                    c.set_end(t);
                }
                if let Some(slot) = p.clip_mut(c.id) {
                    *slot = c;
                }
            }
            DragKind::FadeIn | DragKind::FadeOut => {
                let mut c = grab.clone();
                if d.kind == DragKind::FadeIn {
                    c.fade_in = (now - c.start).clamp(0, c.len as i64) as usize;
                    c.fade_out = c.fade_out.min(c.len - c.fade_in);
                } else {
                    c.fade_out = (c.end() - now).clamp(0, c.len as i64) as usize;
                    c.fade_in = c.fade_in.min(c.len - c.fade_out);
                }
                if let Some(slot) = p.clip_mut(c.id) {
                    *slot = c;
                }
            }
        }
        self.touch();
    }

    fn draw_grid(&self, lp: &Painter, view: &View, lane: Rect) {
        let step = time_step(70.0 / view.pps);
        let t_left = (lane.left() - view.left) as f64 / view.pps + view.t0;
        let t_right = (lane.right() - view.left) as f64 / view.pps + view.t0;
        let mut t = (t_left / step).floor() * step;
        while t <= t_right {
            let x = view.left + ((t - view.t0) * view.pps) as f32;
            lp.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(1.0, GRID_LINE));
            t += step;
        }
    }

    fn draw_ruler(&self, painter: &Painter, view: &View, ruler: Rect, rect: Rect) {
        painter.rect_filled(ruler, 0.0, Color32::from_rgb(26, 31, 42));
        let rp = painter.with_clip_rect(ruler);
        let step = time_step(80.0 / view.pps);
        let t_left = (ruler.left() - view.left) as f64 / view.pps + view.t0;
        let t_right = (ruler.right() - view.left) as f64 / view.pps + view.t0;
        let mut t = (t_left / step).floor() * step;
        while t <= t_right {
            let x = view.left + ((t - view.t0) * view.pps) as f32;
            rp.line_segment([pos2(x, ruler.bottom() - 9.0), pos2(x, ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(110, 122, 142)));
            for k in 1..5 {
                let xm = x + (step * k as f64 / 5.0 * view.pps) as f32;
                rp.line_segment([pos2(xm, ruler.bottom() - 4.0), pos2(xm, ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(64, 74, 92)));
            }
            rp.text(pos2(x + 3.0, ruler.top() + 2.0), Align2::LEFT_TOP, ruler_label(t.max(0.0), step), FontId::proportional(9.5), Color32::from_rgb(150, 162, 182));
            t += step;
        }
        if let Some((a, b)) = self.range {
            let r = Rect::from_min_max(pos2(view.x(a), ruler.top()), pos2(view.x(b), ruler.bottom()));
            rp.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 160, 255, 60));
        }
        // loop markers
        if self.loop_on {
            let (a, b) = self.range.unwrap_or((0, 0));
            if b > a {
                rp.rect_filled(Rect::from_min_max(pos2(view.x(a), ruler.top()), pos2(view.x(b), ruler.top() + 3.0)), 0.0, Color32::from_rgb(110, 200, 140));
            }
        }
        let px = view.x(self.pos());
        rp.add(Shape::convex_polygon(vec![pos2(px - 5.0, ruler.top() + 1.0), pos2(px + 5.0, ruler.top() + 1.0), pos2(px, ruler.top() + 9.0)], PLAYHEAD, Stroke::NONE));
        painter.line_segment([pos2(rect.left(), ruler.bottom()), pos2(rect.right(), ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_clip(&self, lp: &Painter, view: &View, lane: Rect, c: &Clip, col: Color32, selected: bool, chans: &[Option<usize>], chm: [bool; 2], dimmed: bool) {
        let (x0, x1) = (view.x(c.start), view.x(c.end()));
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        let cr = Rect::from_min_max(pos2(x0, lane.top() + 3.0), pos2(x1.max(x0 + 1.5), lane.bottom() - 3.0));
        let off = c.muted || dimmed;
        let base = if off { Color32::from_gray(90) } else { col };
        let vis = cr.intersect(lp.clip_rect());
        let cp = lp.with_clip_rect(vis);
        cp.rect_filled(cr, 4.0, base.gamma_multiply(if selected { 0.42 } else { 0.26 }));
        // name strip
        let strip = Rect::from_min_max(cr.min, pos2(cr.max.x, cr.min.y + 14.0));
        cp.rect_filled(strip, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, base.gamma_multiply(if selected { 0.9 } else { 0.62 }));
        let mut label = c.name.clone();
        if c.gain_db.abs() > 0.05 {
            label.push_str(&format!("   {:+.1} dB", c.gain_db));
        }
        if c.reverse {
            label.push_str("   rev");
        }
        if c.muted {
            label.push_str("   muted");
        }
        cp.text(strip.min + vec2(5.0, 1.5), Align2::LEFT_TOP, label, FontId::proportional(10.0), Color32::from_rgb(10, 12, 16));
        // waveform
        let inner = Rect::from_min_max(pos2(cr.left(), strip.bottom() + 1.0), pos2(cr.right(), cr.bottom() - 1.0));
        let chans: &[Option<usize>] = if c.src.stereo() { chans } else { &[None] };
        let wave_col = base.gamma_multiply(if selected { 1.0 } else { 0.85 });
        let g = db_to_lin(c.gain_db);
        let fpp = view.sr / view.pps;
        let (xa, xb) = (vis.left().floor().max(cr.left().floor()), vis.right().ceil().min(cr.right().ceil()));
        let n_ch = chans.len();
        let lane_h = inner.height() / n_ch as f32;
        let mut shapes: Vec<Shape> = Vec::with_capacity(((xb - xa).max(0.0) as usize) * n_ch);
        let mut x = xa;
        while x < xb {
            let fa = ((x - view.left) as f64 / view.pps + view.t0) * view.sr - c.start as f64;
            let a = fa.max(0.0) as usize;
            let b = ((fa + fpp).ceil().max(0.0) as usize).min(c.len);
            if b > a {
                for (k, ch) in chans.iter().enumerate() {
                    let (lo, hi) = wave_minmax(c, *ch, a, b);
                    let sub = Rect::from_min_size(pos2(inner.left(), inner.top() + k as f32 * lane_h), vec2(inner.width(), lane_h));
                    let (mid, half) = (sub.center().y, sub.height() * 0.5 * 0.94);
                    let y0 = mid - (hi * g).clamp(-1.0, 1.0) * half;
                    let y1 = (mid - (lo * g).clamp(-1.0, 1.0) * half).max(y0 + 1.0);
                    let over = hi * g > 1.0 || -lo * g > 1.0;
                    let silent = ch.is_some_and(|c| chm[c]);
                    shapes.push(Shape::line_segment([pos2(x + 0.5, y0), pos2(x + 0.5, y1)], Stroke::new(1.0, if silent { Color32::from_gray(80) } else if over && !off { Color32::from_rgb(255, 90, 90) } else { wave_col })));
                }
            }
            x += 1.0;
        }
        cp.extend(shapes);
        for k in 0..n_ch {
            let y = inner.top() + (k as f32 + 0.5) * lane_h;
            cp.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(14)));
            if n_ch == 2 && cr.width() > 40.0 {
                let ytop = inner.top() + k as f32 * lane_h;
                if k == 1 {
                    cp.line_segment([pos2(inner.left(), ytop), pos2(inner.right(), ytop)], Stroke::new(1.0, Color32::from_white_alpha(40)));
                }
                cp.text(pos2(inner.left().max(lane.left()) + 4.0, ytop + 2.0), Align2::LEFT_TOP, if k == 0 { "L" } else { "R" }, FontId::proportional(9.0), Color32::from_white_alpha(140));
            }
        }
        // fades
        let (fi, fo) = (c.fade_in as f64 / fpp, c.fade_out as f64 / fpp);
        let shade = Color32::from_black_alpha(110);
        let fl = Stroke::new(1.2, Color32::from_white_alpha(170));
        if fi >= 1.0 {
            let w = fi as f32;
            cp.add(Shape::convex_polygon(vec![pos2(x0, strip.bottom()), pos2(x0 + w, strip.bottom()), pos2(x0, cr.bottom())], shade, Stroke::NONE));
            cp.line_segment([pos2(x0, cr.bottom()), pos2(x0 + w, strip.bottom())], fl);
        }
        if fo >= 1.0 {
            let w = fo as f32;
            cp.add(Shape::convex_polygon(vec![pos2(x1, strip.bottom()), pos2(x1 - w, strip.bottom()), pos2(x1, cr.bottom())], shade, Stroke::NONE));
            cp.line_segment([pos2(x1, cr.bottom()), pos2(x1 - w, strip.bottom())], fl);
        }
        if selected && cr.width() >= 60.0 {
            cp.circle_filled(pos2(x0 + fi as f32 + 7.0, strip.center().y), 3.0, Color32::from_white_alpha(200));
            cp.circle_filled(pos2(x1 - fo as f32 - 7.0, strip.center().y), 3.0, Color32::from_white_alpha(200));
        }
        cp.rect_stroke(cr, 4.0, Stroke::new(if selected { 1.6 } else { 1.0 }, if selected { Color32::from_rgb(235, 240, 250) } else { base.gamma_multiply(0.9) }), egui::StrokeKind::Inside);
    }

    /// Live waveform of what is being recorded on an armed track.
    fn draw_rec_preview(&self, lp: &Painter, view: &View, lane: Rect, t: &Track) {
        let Some(rec) = &self.rec else { return };
        let Some(plan) = rec.plan.iter().find(|r| r.track_id == t.id) else { return };
        let Some(cap) = self.capt.get(&plan.dev) else { return };
        let Some((dev_sr, _)) = cap.format() else { return };
        let frames = cap.frames();
        if frames == 0 {
            return;
        }
        let proj_frames = (frames as f64 * view.sr / dev_sr as f64) as i64;
        let (x0, x1) = (view.x(rec.start), view.x(rec.start + proj_frames));
        let cr = Rect::from_min_max(pos2(x0, lane.top() + 3.0), pos2(x1.max(x0 + 2.0), lane.bottom() - 3.0));
        lp.rect_filled(cr, 3.0, Color32::from_rgba_unmultiplied(235, 70, 70, 40));
        lp.rect_stroke(cr, 3.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(235, 90, 90, 160)), egui::StrokeKind::Inside);
        let vis = cr.intersect(lp.clip_rect());
        if vis.width() < 1.0 {
            return;
        }
        // columns of the visible part only
        let lane_idx = plan.lanes[0];
        let cols = vis.width().ceil() as usize;
        let frac0 = ((vis.left() - cr.left()) / cr.width().max(1.0)) as f64;
        let frac1 = ((vis.right() - cr.left()) / cr.width().max(1.0)) as f64;
        let (a, b) = ((frac0 * frames as f64) as usize, (frac1 * frames as f64) as usize);
        let mid = cr.center().y;
        let half = (cr.height() - 4.0) * 0.5;
        let mut shapes = Vec::new();
        for (i, (lo, hi)) in cap.preview(lane_idx, a, b, cols).into_iter().enumerate() {
            let x = vis.left() + i as f32 + 0.5;
            shapes.push(Shape::line_segment([pos2(x, mid - hi.clamp(-1.0, 1.0) * half), pos2(x, (mid - lo.clamp(-1.0, 1.0) * half).max(mid - hi.clamp(-1.0, 1.0) * half + 1.0))], Stroke::new(1.0, Color32::from_rgb(255, 150, 150))));
        }
        lp.with_clip_rect(vis).extend(shapes);
    }

    #[allow(clippy::too_many_arguments)]
    fn track_header(&mut self, ui: &mut Ui, p: &mut Project, ti: usize, hr: Rect, clip: Rect, remove: &mut Option<usize>, swap: &mut Option<(usize, usize)>, dup: &mut Option<usize>) {
        let selected = self.sel_track == Some(p.tracks[ti].id);
        let col = track_color(&p.tracks[ti]);
        let painter = ui.painter().with_clip_rect(hr.intersect(clip));
        painter.rect_filled(hr, 0.0, if selected { HDR_SEL } else { HDR_BG });
        painter.rect_filled(Rect::from_min_size(hr.min, vec2(4.0, hr.height())), 0.0, col);
        let id = p.tracks[ti].id;
        // click on the empty header selects the track; right-click opens the track menu
        let bg_resp = ui.interact(hr.intersect(clip), ui.scope_id().with(("daw_hdr", id)), Sense::click());
        if bg_resp.clicked() {
            self.sel_track = Some(id);
        }
        bg_resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                self.renaming = Some((id, p.tracks[ti].name.clone()));
                ui.close();
            }
            if ui.button("Import audio file to this track...").on_hover_text("Place a file on this track at the playhead").clicked() && self.open_dlg.is_none() {
                self.import_target = Some((id, self.cursor));
                self.open_dlg = Some(dialogs::open_many("Add audio to the track", "Audio files", decode::EXTENSIONS));
                ui.close();
            }
            if graph_on() {
                if ui.button("Record from the Routing window").on_hover_text("Arms the track and adds a Timeline record node you can connect to anything").clicked() {
                    self.checkpoint(p);
                    p.tracks[ti].input = graph_input(id);
                    p.tracks[ti].arm = true;
                    crate::graph::bus().request_rec_node(id, &p.tracks[ti].name);
                    ui.close();
                }
            } else {
                ui.menu_button("Record input", |ui| self.input_menu(ui, p, ti));
            }
            if p.tracks[ti].arm && ui.button("Disarm").clicked() {
                p.tracks[ti].arm = false;
                self.touch();
                ui.close();
            }
            if p.tracks[ti].clips.iter().any(|c| c.src.stereo()) {
                ui.separator();
                let t = &p.tracks[ti];
                if t.chan == Chan::Stereo || t.chan == Chan::Mono {
                    let mono = t.chan == Chan::Mono;
                    if ui.button(if mono { "Back to stereo" } else { "Convert to mono" }).clicked() {
                        self.checkpoint(p);
                        p.tracks[ti].chan = if mono { Chan::Stereo } else { Chan::Mono };
                        ui.close();
                    }
                    if ui.button("Split into left and right tracks").on_hover_text("Two tracks, panned hard left and hard right").clicked() {
                        self.split_req = Some(id);
                        ui.close();
                    }
                } else if ui.button("Back to stereo").clicked() {
                    self.checkpoint(p);
                    p.tracks[ti].chan = Chan::Stereo;
                    p.tracks[ti].pan = 0.0;
                    ui.close();
                }
                let expanded = p.tracks[ti].h >= 2;
                if ui.button(if expanded { "Hide channel sub-tracks" } else { "Show channel sub-tracks" }).clicked() {
                    p.tracks[ti].h = if expanded { 1 } else { 2 };
                    self.touch();
                    ui.close();
                }
            }
            ui.menu_button("Input trim (gain / delay)", |ui| {
                ui.label(RichText::new("For recording from several interfaces at once:").small().weak());
                let t = &mut p.tracks[ti];
                let mut ch = false;
                ui.horizontal(|ui| {
                    ui.label("Gain");
                    ch |= ui.add(egui::DragValue::new(&mut t.in_db).range(-60.0..=40.0).speed(0.1).suffix(" dB")).changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Place earlier by");
                    ch |= ui.add(egui::DragValue::new(&mut t.in_ms).range(-1000.0..=1000.0).speed(0.2).suffix(" ms")).changed();
                });
                ui.label(RichText::new("Use the delay for an interface whose audio arrives late (its latency),\nor a negative value to push a take later. Applied to the next recording.").small().weak());
                if ch {
                    self.touch();
                }
            });
            ui.separator();
            ui.menu_button("Colour", |ui| {
                for (k, &(r, g, b)) in TRACK_COLORS.iter().enumerate() {
                    if ui.add(egui::Button::new("      ").fill(Color32::from_rgb(r, g, b))).clicked() {
                        p.tracks[ti].color = k;
                        self.touch();
                        ui.close();
                    }
                }
            });
            ui.menu_button("Height", |ui| {
                for (k, n) in ["Small", "Normal", "Large (split channels)"].iter().enumerate() {
                    if ui.selectable_label(p.tracks[ti].h as usize == k, *n).clicked() {
                        p.tracks[ti].h = k as u8;
                        self.touch();
                        ui.close();
                    }
                }
            });
            if ui.add_enabled(ti > 0, egui::Button::new("Move up")).clicked() {
                *swap = Some((ti, ti - 1));
                ui.close();
            }
            if ui.add_enabled(ti + 1 < p.tracks.len(), egui::Button::new("Move down")).clicked() {
                *swap = Some((ti, ti + 1));
                ui.close();
            }
            if ui.button("Duplicate track").clicked() {
                *dup = Some(ti);
                ui.close();
            }
            if ui.button("Delete track").clicked() {
                *remove = Some(ti);
                ui.close();
            }
        });
        let content = Rect::from_min_max(hr.min + vec2(9.0, 3.0), pos2(hr.max.x - 12.0, hr.max.y - 2.0));
        ui.scope_builder(UiBuilder::new().max_rect(content).layout(Layout::top_down(Align::LEFT)), |ui| {
            ui.shrink_clip_rect(content.intersect(clip));
            ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
            ui.spacing_mut().interact_size.y = 15.0;
            ui.spacing_mut().slider_width = 52.0;
            ui.spacing_mut().slider_rail_height = 4.0;
            // name + M S R
            let w = content.width();
            ui.horizontal(|ui| {
                ui.set_height(17.0);
                if self.renaming.as_ref().is_some_and(|r| r.0 == id) {
                    let (_, text) = self.renaming.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(w - 66.0).font(FontId::proportional(11.5)));
                    // focus once when the field appears; asking every frame would keep it from ever closing
                    let init = ui.scope_id().with(("daw_rename_init", id));
                    if !ui.data(|d| d.get_temp::<bool>(init).unwrap_or(false)) {
                        r.request_focus();
                        ui.data_mut(|d| d.insert_temp(init, true));
                    }
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if r.lost_focus() {
                        ui.data_mut(|d| d.insert_temp(init, false));
                        let (_, text) = self.renaming.take().unwrap();
                        if !esc && !text.trim().is_empty() {
                            p.tracks[ti].name = text.trim().to_string();
                            self.touch();
                        }
                    }
                } else {
                    let r = ui.add_sized(vec2(w - 62.0, 15.0), egui::Label::new(RichText::new(&p.tracks[ti].name).strong().size(11.5)).truncate().sense(Sense::click()));
                    if r.double_clicked() {
                        self.renaming = Some((id, p.tracks[ti].name.clone()));
                    } else if r.clicked() {
                        self.sel_track = Some(id);
                    }
                    r.on_hover_text("Double-click to rename");
                }
                let t = &mut p.tracks[ti];
                if mini_toggle(ui, "M", t.mute, Color32::from_rgb(235, 190, 70), "Mute") {
                    t.mute = !t.mute;
                    self.touch();
                }
                if mini_toggle(ui, "S", t.solo, Color32::from_rgb(110, 200, 250), "Solo") {
                    t.solo = !t.solo;
                    self.touch();
                }
                if mini_toggle(ui, "R", t.arm, Color32::from_rgb(235, 80, 80), "Arm for recording") {
                    t.arm = !t.arm;
                    if t.arm && graph_on() {
                        // the track records what its record node in the routing graph receives
                        p.tracks[ti].input = graph_input(id);
                        crate::graph::bus().request_rec_node(id, &p.tracks[ti].name);
                    } else if t.arm && t.input.dev.is_empty() {
                        if let Some(inp) = self.default_input_for(p, ti) {
                            p.tracks[ti].input = inp;
                        }
                    }
                    self.touch();
                }
            });
            // volume + pan (+ channel view)
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = 46.0;
                let mut t = p.tracks[ti].clone();
                draw_icon(ui, SliderIcon::Volume);
                let tip = format!("Volume {:+.1} dB", t.vol_db);
                let ch1 = self.fader(ui, ("vol", id), &mut t.vol_db, -60.0..=12.0, 0.0, 1.0, &tip);
                draw_icon(ui, SliderIcon::Pan);
                let ptxt = if t.pan.abs() < 0.01 { "centre".to_string() } else if t.pan < 0.0 { format!("{:.0}% left", -t.pan * 100.0) } else { format!("{:.0}% right", t.pan * 100.0) };
                let ch2 = self.fader(ui, ("pan", id), &mut t.pan, -1.0..=1.0, 0.0, 100.0, &format!("Pan (left / right balance): {ptxt}"));
                if ch1 || ch2 {
                    p.tracks[ti].vol_db = t.vol_db;
                    p.tracks[ti].pan = t.pan;
                    self.touch();
                }
                if p.tracks[ti].clips.iter().any(|c| c.src.stereo()) {
                    let on = p.tracks[ti].h >= 2;
                    if mini_toggle(ui, "LR", on, Color32::from_rgb(120, 170, 250), "Show the left and right channel as sub-tracks you can edit separately") {
                        p.tracks[ti].h = if on { 1 } else { 2 };
                        self.touch();
                    }
                }
            });
            if p.tracks[ti].h >= 2 {
                self.channel_rows(ui, p, ti);
            }
            // input selection of armed tracks
            if p.tracks[ti].arm && hr.height() >= 62.0 {
                if graph_on() {
                    ui.label(RichText::new("records from the Routing window").small().weak());
                } else {
                    self.input_row(ui, p, ti);
                }
            }
        });
        // level meters
        let lv = if p.tracks[ti].arm && self.play.is_none() || self.rec.is_some() && p.tracks[ti].arm {
            self.in_level.get(&id).copied().unwrap_or([0.0; 2])
        } else {
            self.out_level.get(ti).copied().unwrap_or([0.0; 2])
        };
        let mh = hr.height() - 8.0;
        meter_bar(&painter, Rect::from_min_size(pos2(hr.right() - 10.0, hr.top() + 4.0), vec2(3.0, mh)), lv[0]);
        meter_bar(&painter, Rect::from_min_size(pos2(hr.right() - 6.0, hr.top() + 4.0), vec2(3.0, mh)), lv[1]);
        painter.line_segment([pos2(hr.left(), hr.bottom()), pos2(hr.right(), hr.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
    }

    /// A small slider. Double-click types a value, right-click resets. `disp` scales the typed / shown number
    /// (e.g. 100 for a pan in percent). Returns true when the value changed.
    fn fader(&mut self, ui: &mut Ui, key: impl std::hash::Hash + std::fmt::Debug, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, disp: f32, tip: &str) -> bool {
        let id = ui.scope_id().with(key);
        let mut changed = false;
        if self.val_edit.as_ref().is_some_and(|e| e.0 == id) {
            let (_, text, focused) = self.val_edit.as_mut().unwrap();
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.spacing().slider_width).font(FontId::proportional(10.5)));
            if !*focused {
                *focused = true;
                r.request_focus();
                let mut st = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
                st.store(ui.ctx(), r.id);
            }
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if r.lost_focus() {
                let (_, text, _) = self.val_edit.take().unwrap();
                if !esc {
                    let num: String = text.chars().filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | ',')).collect();
                    if let Ok(x) = num.replace(',', ".").parse::<f32>() {
                        *v = (x / disp).clamp(*range.start(), *range.end());
                        changed = true;
                    }
                }
            }
            return changed;
        }
        let before = *v;
        let r = ui.add(egui::Slider::new(v, range).show_value(false));
        // a double-click first moves the slider with its single clicks: remember the value from before them
        let pre_id = id.with("pre");
        let now = ui.input(|i| i.time);
        if r.hovered() && ui.input(|i| i.pointer.any_pressed()) {
            let old = ui.data(|d| d.get_temp::<(f32, f64)>(pre_id));
            if old.is_none_or(|(_, t)| now - t > 0.6) {
                ui.data_mut(|d| d.insert_temp(pre_id, (before, now)));
            }
        }
        if r.double_clicked() {
            if let Some((pv, _)) = ui.data(|d| d.get_temp::<(f32, f64)>(pre_id)) {
                *v = pv;
            }
            self.val_edit = Some((id, format!("{:.1}", *v * disp), false));
        } else if r.changed() {
            changed = true;
        }
        if r.secondary_clicked() {
            *v = default;
            changed = true;
        }
        r.on_hover_text(format!("{tip}\nDouble-click: type a value.  Right-click: reset."));
        changed
    }

    /// Left / right sub-track controls of a stereo track, plus mono conversion and splitting.
    fn channel_rows(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        let id = p.tracks[ti].id;
        let mut t = p.tracks[ti].clone();
        let (mut changed, mut ck) = (false, false);
        match t.chan {
            Chan::Stereo => {
                for k in 0..2 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().slider_width = 46.0;
                        let name = if k == 0 { "Left" } else { "Right" };
                        ui.add_sized(vec2(11.0, 15.0), egui::Label::new(RichText::new(&name[..1]).strong().size(10.5).color(Color32::from_rgb(160, 185, 230))));
                        if mini_toggle(ui, "M", t.ch_mute[k], Color32::from_rgb(235, 190, 70), &format!("Mute the {} channel", name.to_lowercase())) {
                            t.ch_mute[k] = !t.ch_mute[k];
                            changed = true;
                            ck = true;
                        }
                        draw_icon(ui, SliderIcon::Volume);
                        let tip = format!("{name} channel gain {:+.1} dB", t.ch_db[k]);
                        changed |= self.fader(ui, ("chv", id, k), &mut t.ch_db[k], -24.0..=12.0, 0.0, 1.0, &tip);
                    });
                }
            }
            Chan::Left | Chan::Right | Chan::Mono => {
                let txt = match t.chan {
                    Chan::Left => "Plays the left channel only",
                    Chan::Right => "Plays the right channel only",
                    _ => "Left and right mixed to mono",
                };
                ui.label(RichText::new(txt).small().weak());
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mono = t.chan == Chan::Mono;
            if t.chan == Chan::Left || t.chan == Chan::Right {
                if ui.small_button("Back to stereo").on_hover_text("Play both channels again").clicked() {
                    t.chan = Chan::Stereo;
                    t.pan = 0.0;
                    changed = true;
                    ck = true;
                }
            } else {
                if ui.selectable_label(mono, RichText::new("Mono").size(10.5)).on_hover_text("Mix left and right to one mono signal (not destructive)").clicked() {
                    t.chan = if mono { Chan::Stereo } else { Chan::Mono };
                    changed = true;
                    ck = true;
                }
                if ui.small_button("Split L/R").on_hover_text("Make two tracks: the left channel panned left and the right channel panned right").clicked() {
                    self.split_req = Some(id);
                }
            }
        });
        if ck {
            self.checkpoint(p);
        }
        if changed {
            let dst = &mut p.tracks[ti];
            dst.chan = t.chan;
            dst.pan = t.pan;
            dst.ch_db = t.ch_db;
            dst.ch_mute = t.ch_mute;
            self.touch();
        }
    }

    /// Menu entries that connect a track to a recording input (and arm it).
    fn input_menu(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        if self.devs_v.is_empty() {
            ui.label(RichText::new("scanning inputs...").small().weak());
        }
        let devs = self.devs_v.clone();
        for d in devs {
            ui.menu_button(capture::short_label(&d.label), |ui| {
                ui.label(RichText::new(format!("{} ch, {} Hz", d.ch, d.sr)).small().weak());
                let mut pick: Option<Mode> = None;
                for c in 0..d.ch.min(32) {
                    if ui.button(format!("Channel {}", c + 1)).clicked() {
                        pick = Some(Mode::Chan(c));
                    }
                }
                ui.separator();
                for k in 0..(d.ch / 2).min(16) {
                    if ui.button(format!("Stereo pair {}+{}", 2 * k + 1, 2 * k + 2)).clicked() {
                        pick = Some(Mode::Pair(k));
                    }
                }
                if ui.button("Mono mix of all channels").clicked() {
                    pick = Some(Mode::Mix);
                }
                if let Some(m) = pick {
                    self.checkpoint(p);
                    p.tracks[ti].input = Input { dev: d.id.clone(), mode: m };
                    p.tracks[ti].arm = true;
                    self.touch();
                    ui.close();
                }
            });
        }
    }

    fn default_input_for(&self, p: &Project, ti: usize) -> Option<Input> {
        let mut q = p.clone();
        q.tracks[ti].arm = false;
        self.default_input(&q)
    }

    fn input_row(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        let id = p.tracks[ti].id;
        let cur_dev = p.tracks[ti].input.dev.clone();
        let dev_ch = self.devs_v.iter().find(|d| d.id == cur_dev).map(|d| d.ch).unwrap_or(2);
        let dev_label = self.devs_v.iter().find(|d| d.id == cur_dev).map(|d| capture::short_label(&d.label)).unwrap_or_else(|| if cur_dev.is_empty() { "no input".into() } else { "device missing".into() });
        let status = self.capt.get(&cur_dev).map(|c| c.status.lock().clone());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mut new_dev: Option<String> = None;
            egui::ComboBox::from_id_salt(("daw_dev", id)).width(74.0).truncate().selected_text(dev_label).show_ui(ui, |ui| {
                for d in &self.devs_v {
                    if ui.selectable_label(d.id == cur_dev, format!("{}  ({} ch)", capture::short_label(&d.label), d.ch)).clicked() {
                        new_dev = Some(d.id.clone());
                    }
                }
            });
            if let Some(d) = new_dev {
                p.tracks[ti].input = Input { dev: d, mode: Mode::Chan(0) };
                self.touch();
            }
            let mut mode = p.tracks[ti].input.mode;
            egui::ComboBox::from_id_salt(("daw_ch", id)).width(64.0).selected_text(mode.label()).show_ui(ui, |ui| {
                for c in 0..dev_ch.min(32) {
                    ui.selectable_value(&mut mode, Mode::Chan(c), Mode::Chan(c).label());
                }
                for k in 0..(dev_ch / 2).min(16) {
                    ui.selectable_value(&mut mode, Mode::Pair(k), format!("Stereo {}", Mode::Pair(k).label()));
                }
                ui.selectable_value(&mut mode, Mode::Mix, "Mono mix");
            });
            if mode != p.tracks[ti].input.mode {
                p.tracks[ti].input.mode = mode;
                self.touch();
            }
            if let Some(s) = status {
                if s.contains("not found") || s.contains("unsupported") || s.contains("reconnect") {
                    ui.label(RichText::new("!").color(Color32::from_rgb(255, 170, 70))).on_hover_text(s);
                }
            }
        });
    }

    // ------------------------------------------------------------------ analysis panel

    fn analysis_panel(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &Project, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(17, 21, 28));
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_rgb(40, 48, 62)), egui::StrokeKind::Inside);
        let inner = rect.shrink2(vec2(8.0, 5.0));
        let (a, b) = self.analysis_range(p);
        let what = if self.range.is_some() {
            "selection"
        } else if !self.sel.is_empty() {
            "selected clips"
        } else {
            "whole project"
        };
        let src = match (self.an_src, self.analysis_track_name(p)) {
            (AnSrc::Track, Some(n)) => format!("track {n}"),
            _ => "master mix".to_string(),
        };
        // header line
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), 18.0));
        ui.scope_builder(UiBuilder::new().max_rect(head).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(format!("Analysis of the {what}, {src}   {} - {}", clock(a as f32 / p.sr as f32), clock(b as f32 / p.sr as f32))).small().color(Color32::from_rgb(170, 182, 200)));
            ui.selectable_value(&mut self.an_src, AnSrc::Master, "Master");
            ui.selectable_value(&mut self.an_src, AnSrc::Track, "Track");
            if ui.small_button("Loop & play").on_hover_text("Play this stretch in a loop: all analysers (spectrum, scope, loudness ...) follow it").clicked() {
                if self.range.is_none() && b > a {
                    self.range = Some((a, b));
                }
                self.loop_on = true;
                self.seek(a);
                if self.play.is_none() {
                    self.feed_once = true;
                    self.play_start(cx);
                }
            }
            if let Some(out) = self.an_out.clone() {
                if ui.small_button("Store spectrum as trace").on_hover_text("Add the averaged spectrum to the trace library (Response, Transfer and EQ windows can use it)").clicked() {
                    let name = format!("Timeline {what} ({})", src);
                    let avg: Vec<f32> = (0..out.f.len()).map(|i| 10.0 * (0.5 * (10f32.powf(out.db[0][i] / 10.0) + 10f32.powf(out.db[1][i] / 10.0))).max(1e-12).log10()).collect();
                    cx.lib.add(Some(name), out.f.clone(), avg, Vec::new());
                    cx.req.note = Some("Spectrum stored in the trace library".into());
                }
            }
            if self.an_running {
                ui.spinner();
            }
        });
        let body = Rect::from_min_max(pos2(inner.left(), inner.top() + 22.0), inner.max);
        let Some(out) = self.an_out.clone() else {
            painter.text(body.center(), Align2::CENTER_CENTER, if b > a { "analysing..." } else { "nothing to analyse yet" }, FontId::proportional(12.0), Color32::from_rgb(110, 122, 140));
            return;
        };
        // wide enough for "-10.4 / -14.2 dBFS" and "+0.00093 / +0.00001" in the monospace values
        let stats_w = 250.0f32.min(body.width() * 0.34);
        let rest = body.width() - stats_w - 12.0;
        let spec_w = rest * 0.56;
        let stats_r = Rect::from_min_size(body.min, vec2(stats_w, body.height()));
        let spec_r = Rect::from_min_size(pos2(stats_r.right() + 6.0, body.top()), vec2(spec_w, body.height()));
        let spg_r = Rect::from_min_max(pos2(spec_r.right() + 6.0, body.top()), body.max);
        self.stats_grid(ui, &out, stats_r);
        self.spectrum_plot(ui, &out, spec_r);
        self.spectrogram_view(ui, &out, spg_r, (a, b), p.sr);
        if out.truncated {
            painter.text(pos2(rect.right() - 10.0, rect.top() + 6.0), Align2::RIGHT_TOP, "analysed: first 5 minutes", FontId::proportional(10.0), Color32::from_rgb(255, 190, 80));
        }
    }

    fn analysis_track_name(&self, p: &Project) -> Option<String> {
        let id = self.sel_track.or_else(|| self.sel.iter().next().and_then(|c| p.find_clip(*c)).map(|(ti, _)| p.tracks[ti].id))?;
        p.track_index(id).map(|ti| p.tracks[ti].name.clone())
    }

    fn stats_grid(&self, ui: &mut Ui, out: &AnalysisOut, r: Rect) {
        let s = &out.stats;
        let f = |v: f32| if v <= -119.0 { "-inf".to_string() } else { format!("{v:.1}") };
        let rows: Vec<(&str, String)> = vec![
            ("Length", clock(s.secs)),
            ("Peak L / R", format!("{} / {} dBFS", f(s.peak_db[0]), f(s.peak_db[1]))),
            ("RMS L / R", format!("{} / {} dBFS", f(s.rms_db[0]), f(s.rms_db[1]))),
            ("True peak", format!("{} dBTP", f(s.true_peak_db))),
            ("Loudness", s.lufs.map_or("-".into(), |v| format!("{v:.1} LUFS"))),
            ("Loudness range", s.lra.map_or("-".into(), |v| format!("{v:.1} LU"))),
            ("Crest factor", format!("{:.1} dB", s.crest_db)),
            ("L/R correlation", format!("{:+.2}", s.corr)),
            ("DC offset", format!("{:+.5} / {:+.5}", s.dc[0], s.dc[1])),
            ("Clipped samples", format!("{}", s.clipped)),
            ("Silence", format!("{:.0} %", s.silence_pct)),
        ];
        ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
            ui.shrink_clip_rect(r);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("daw_stats").num_columns(2).spacing([6.0, 1.0]).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(RichText::new(k).small().weak());
                        let warn = (k == "Clipped samples" && s.clipped > 0) || (k == "True peak" && s.true_peak_db > -0.1);
                        ui.add(egui::Label::new(RichText::new(v).small().monospace().color(if warn { Color32::from_rgb(255, 130, 110) } else { Color32::from_rgb(215, 224, 238) })).truncate());
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn spectrum_plot(&self, ui: &mut Ui, out: &AnalysisOut, r: Rect) {
        if r.width() < 60.0 || r.height() < 40.0 {
            return;
        }
        let painter = ui.painter_at(r);
        let resp = ui.interact(r, ui.scope_id().with("daw_spec"), Sense::hover());
        painter.rect_filled(r, 3.0, crate::views::BG);
        let rect = r;
        let plot_r = inset(rect);
        let f1 = (out.sr as f32 * 0.5).min(20000.0);
        let pl = Plot::new(plot_r, analysis::F_LO, f1, -100.0, 0.0, true);
        pl.grid_x(&painter, &|f| hz_label(f));
        pl.grid_y(&painter, &|v| format!("{v:.0}"));
        pl.frame(&painter);
        for (c, col) in [(0usize, Color32::from_rgb(70, 200, 255)), (1, Color32::from_rgb(255, 100, 160))] {
            pl.line(&painter, out.f.iter().copied().zip(out.db[c].iter().copied()), Stroke::new(1.3, col.gamma_multiply(0.9)));
        }
        painter.text(rect.left_top() + vec2(48.0, 6.0), Align2::LEFT_TOP, "Average spectrum (dBFS)", FontId::proportional(9.5), Color32::from_rgb(130, 142, 160));
        if let Some(h) = resp.hover_pos() {
            if plot_r.contains(h) {
                let f = pl.fx(h.x);
                if let Some(i) = out.f.iter().position(|&x| x >= f) {
                    painter.line_segment([pos2(h.x, plot_r.top()), pos2(h.x, plot_r.bottom())], Stroke::new(1.0, Color32::from_white_alpha(50)));
                    pl.label(&painter, pos2(plot_r.right() - 4.0, plot_r.top() + 4.0), format!("{}Hz  L {:.1}  R {:.1}", hz_label(out.f[i]), out.db[0][i], out.db[1][i]), Color32::WHITE, Align2::RIGHT_TOP);
                }
            }
        }
    }

    fn spectrogram_view(&mut self, ui: &mut Ui, out: &Arc<AnalysisOut>, r: Rect, range: (i64, i64), sr: u32) {
        if r.width() < 60.0 || r.height() < 40.0 || out.spg.is_empty() {
            return;
        }
        // texture from the cached analysis (rebuilt when a new result arrived)
        let ptr = Arc::as_ptr(out) as usize;
        if self.spg_tex.is_none() || self.spg_ptr != ptr {
            let lut = crate::colormap::Cmap::Inferno.lut();
            let (w, h) = (out.spg_w, out.spg_h);
            let mut px = vec![Color32::BLACK; w * h];
            for x in 0..w {
                for y in 0..h {
                    px[(h - 1 - y) * w + x] = lut[out.spg[x * h + y] as usize];
                }
            }
            let img = egui::ColorImage::new([w, h], px);
            match &mut self.spg_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.spg_tex = Some(ui.ctx().load_texture("daw_spectrogram", img, egui::TextureOptions::LINEAR)),
            }
            self.spg_ptr = ptr;
        }
        let painter = ui.painter_at(r);
        let rect = r;
        painter.rect_filled(rect, 3.0, Color32::BLACK);
        let plot_r = Rect::from_min_max(rect.min + vec2(34.0, 4.0), rect.max - vec2(4.0, 16.0));
        if let Some(t) = &self.spg_tex {
            painter.image(t.id(), plot_r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // frequency axis (log, same mapping as the rows)
        let f1 = (out.sr as f32 * 0.5).min(20000.0);
        for f in [50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            if f >= f1 {
                continue;
            }
            let t = (f / analysis::F_LO).ln() / (f1 / analysis::F_LO).ln();
            let y = plot_r.bottom() - t * plot_r.height();
            painter.line_segment([pos2(plot_r.left() - 3.0, y), pos2(plot_r.left(), y)], Stroke::new(1.0, Color32::from_gray(120)));
            painter.text(pos2(plot_r.left() - 5.0, y), Align2::RIGHT_CENTER, hz_label(f), FontId::proportional(8.5), Color32::from_gray(140));
        }
        let (a, b) = range;
        painter.text(pos2(plot_r.left(), plot_r.bottom() + 2.0), Align2::LEFT_TOP, clock(a as f32 / sr as f32), FontId::proportional(8.5), Color32::from_gray(140));
        painter.text(pos2(plot_r.right(), plot_r.bottom() + 2.0), Align2::RIGHT_TOP, clock(b as f32 / sr as f32), FontId::proportional(8.5), Color32::from_gray(140));
        painter.rect_stroke(plot_r, 0.0, Stroke::new(1.0, Color32::from_gray(50)), egui::StrokeKind::Inside);
        // playhead inside the analysed stretch
        let pos = self.pos();
        if pos >= a && pos <= b && b > a {
            let x = plot_r.left() + (pos - a) as f32 / (b - a) as f32 * plot_r.width();
            painter.line_segment([pos2(x, plot_r.top()), pos2(x, plot_r.bottom())], Stroke::new(1.0, PLAYHEAD));
        }
    }
}
