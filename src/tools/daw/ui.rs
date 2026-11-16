//! Drawing and mouse / keyboard handling of the timeline.

use super::*;
use crate::tools::plot::{hz_label, inset, Plot};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Layout, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, UiBuilder};

mod draw;
mod tracks;
mod panel;

const HEADER_W: f32 = 178.0;
const RULER_H: f32 = 22.0;
/// Height of the upper bar of a track row: a click there selects, a click below it (in the waveform) places the playback cursor.
const TRACK_BAR_H: f32 = 18.0;
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
    let r = resp.on_hover_text(tip);
    let (id, label) = match icon {
        Icon::Start => ("daw_home", "Timeline: to the start"),
        Icon::Play | Icon::Stop => ("daw_play", "Timeline: play / stop"),
        Icon::Rec => ("daw_record", "Timeline: record / stop"),
        Icon::Loop => ("daw_loop", "Timeline: loop on / off"),
    };
    crate::mapping::button_menu(&r, &crate::mapping::Target::toggle(id, label, false));
    r.clicked()
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

fn meter_bar(p: &Painter, r: Rect, level: f32, red: bool) {
    p.rect_filled(r, 1.0, Color32::from_gray(30));
    let db = 20.0 * level.max(1e-6).log10();
    let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let col = if db > -1.0 && red { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) };
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
        let mut p;
        self.edited = false;
        let whole = ui.max_rect();
        if ui.input(|i| i.pointer.any_pressed()) { self.keyboard_active = ui.rect_contains_pointer(whole); }
        self.project_confirmation(ui, cx);
        if self.pending_replace.is_some() { return; }
        p = self.proj.lock().clone();
        self.handle_drops(ui, whole);
        self.toolbar(ui, cx, &mut p);
        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, Sense::hover());
        // the analysis goes below the tracks when the window is tall enough, to the right of them when it is wide
        // but low, and is left out when there is no room for either
        let (mut an_h, mut an_w) = (0.0f32, 0.0f32);
        if self.an_open {
            // (VISUSCOPE_AN_SIDE=1 forces the side placement for screenshots)
            if avail.height() > 330.0 && std::env::var_os("VISUSCOPE_AN_SIDE").is_none() {
                an_h = (avail.height() * 0.40).clamp(150.0, 240.0);
            } else if avail.width() >= 900.0 && avail.height() >= 170.0 {
                an_w = (avail.width() * 0.36).clamp(340.0, 560.0);
            }
        }
        self.an_hidden = self.an_open && an_h == 0.0 && an_w == 0.0;
        if self.an_hidden { ui.painter().text(avail.left_bottom() - vec2(0.0, 12.0), Align2::LEFT_BOTTOM, "Analysis hidden: enlarge the Timeline window", FontId::proportional(12.0), Color32::YELLOW); }
        let body = Rect::from_min_max(avail.min, pos2(avail.max.x - an_w, avail.max.y - an_h));
        self.keys(ui, cx, &mut p, whole);
        self.edit_windows(ui, &p);
        for c in std::mem::take(&mut self.edit_cmds) {
            match c.as_str() {
                "marker" => {
                    self.checkpoint(&p);
                    p.add_mark(self.pos(), 0, None);
                    self.touch();
                }
                "split" => self.act_split(&mut p),
                "undo" => self.undo(&mut p),
                "redo" => self.redo(&mut p),
                "zoom_in" => self.zoom_by(1.4, None),
                "zoom_out" => self.zoom_by(0.7, None),
                "fit" => self.fit_view(),
                _ => {}
            }
        }
        self.body(ui, cx, &mut p, body);
        if an_h > 0.0 {
            let an = Rect::from_min_max(pos2(avail.min.x, avail.max.y - an_h + 4.0), avail.max);
            self.analysis_panel(ui, cx, &p, an);
            self.schedule_analysis(&p);
        } else if an_w > 0.0 {
            let an = Rect::from_min_max(pos2(avail.max.x - an_w + 4.0, avail.min.y), avail.max);
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
        // (while files are dragged over the window the system does not report the mouse: its last position says nothing about where
        // the files are, so a drop counts wherever it is, as long as no other window has the pointer)
        let elsewhere = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| !whole.contains(p)) && ui.ctx().layer_id_at(ui.ctx().input(|i| i.pointer.hover_pos()).unwrap_or_default()).is_some_and(|l| l != ui.layer_id() && l.order != egui::Order::Background);
        if hover && !elsewhere {
            ui.painter().rect_stroke(whole.shrink(2.0), 6.0, Stroke::new(2.0, Color32::from_rgb(110, 170, 255)), egui::StrokeKind::Inside);
            ui.painter().text(whole.center(), Align2::CENTER_CENTER, "Drop audio files to add them as tracks", FontId::proportional(16.0), Color32::from_rgb(160, 200, 255));
        }
        if !dropped.is_empty() && !elsewhere {
            crate::log::log(&format!("timeline: {} file(s) dropped", dropped.len()));
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
            ui.toggle_value(&mut self.follow, crate::lang::tr("Follow")).on_hover_text(crate::lang::tr("Scroll the view with the playhead"));
            ui.toggle_value(&mut self.show_clip, crate::lang::tr("Clipping highlights")).on_hover_text(crate::lang::tr("Show samples above full scale in red on the waveforms and in the track meters (switch off to see the plain waveform)"));
            ui.toggle_value(&mut self.punch, crate::lang::tr("Punch")).on_hover_text(crate::lang::tr("Punch in / out: a recording keeps only the selected time range (the audio it replaces stays on the track as an older take). Select the range first."));
            ui.toggle_value(&mut self.snap, crate::lang::tr("Snap")).on_hover_text(crate::lang::tr("Clips, the playhead and selections snap to the grid, to the edges of other clips and to the playhead (hold Alt to move freely, hold Shift while selecting to snap to the beat of the tracks that show it)"));
            if ui.add_enabled_ui(self.range.is_some(), |ui| crate::mapping::small_btn(ui, "Selection to new track")).inner.on_hover_text(crate::lang::tr("A new track with the selected time range: the clips of the selected track cut to it (the originals stay), or with no track selected the whole mix over that range")).clicked() {
                self.act_track_from_range(p);
            }
            ui.separator();
            let ub = ui.add_enabled(!self.undo.is_empty(), egui::Button::new(crate::lang::tr("Undo")).small()).on_hover_text(crate::lang::tr("Undo the last timeline edit (Ctrl+Z)"));
            crate::mapping::button_menu(&ub, &crate::mapping::Target::toggle("daw_undo", "Timeline: undo", false));
            if ub.clicked() {
                self.undo(p);
            }
            let rb = ui.add_enabled(!self.redo.is_empty(), egui::Button::new(crate::lang::tr("Redo")).small()).on_hover_text(crate::lang::tr("Redo (Ctrl+Y)"));
            crate::mapping::button_menu(&rb, &crate::mapping::Target::toggle("daw_redo", "Timeline: redo", false));
            if rb.clicked() {
                self.redo(p);
            }
            ui.separator();
            if crate::mapping::small_btn(ui, "-").on_hover_text(crate::lang::tr("Zoom out (mouse wheel over the tracks)")).clicked() {
                self.zoom_by(0.7, None);
            }
            if crate::mapping::small_btn(ui, "+").on_hover_text(crate::lang::tr("Zoom in (mouse wheel over the tracks; Shift+wheel scrolls sideways)")).clicked() {
                self.zoom_by(1.4, None);
            }
            if crate::mapping::small_btn(ui, "Fit").on_hover_text(crate::lang::tr("Show the whole project")).clicked() {
                self.fit_view();
            }
            ui.separator();
            ui.menu_button(crate::lang::tr("+ Track"), |ui| {
                if crate::mapping::btn(ui, "Empty track").clicked() {
                    self.checkpoint(p);
                    let n = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
                    self.sel_track = Some(p.tracks[n].id);
                    ui.close();
                }
                if crate::mapping::btn(ui, "Draw track (a control signal)").on_hover_text(crate::lang::tr("Draw a curve over the timeline with the mouse. It is not heard in the master mix: pick it in a Timeline out node of the Routing window, and let it control a setting of a node (right-click a value there)")).clicked() {
                    self.checkpoint(p);
                    let n = p.add_draw_track();
                    self.sel_track = Some(p.tracks[n].id);
                    ui.close();
                }
                ui.separator();
                if graph_on() {
                    if crate::mapping::btn(ui, "Track that records from the Routing window").on_hover_text(crate::lang::tr("An armed track plus a Timeline record node to connect")).clicked() {
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
                ui.label(RichText::new(crate::lang::tr("Record from an input (armed)")).small().weak());
                if self.devs_v.is_empty() {
                    ui.label(RichText::new(crate::lang::tr("scanning inputs...")).small().weak());
                }
                let devs = self.devs_v.clone();
                for d in devs {
                    ui.menu_button(capture::short_label(&d.label), |ui| {
                        ui.label(RichText::new(format!("{} ch, {} Hz", d.ch, d.sr)).small().weak());
                        if crate::mapping::btn(ui, format!("One mono track per channel ({})", d.ch.min(32))).clicked() {
                            self.checkpoint(p);
                            for c in 0..d.ch.min(32) {
                                self.add_input_track(p, &d, Mode::Chan(c), Some(format!("In {}", c + 1)));
                            }
                            ui.close();
                        }
                        if d.ch >= 2 && crate::mapping::btn(ui, format!("Stereo pairs ({})", d.ch / 2)).clicked() {
                            self.checkpoint(p);
                            for k in 0..(d.ch / 2).min(16) {
                                self.add_input_track(p, &d, Mode::Pair(k), Some(format!("In {}+{}", 2 * k + 1, 2 * k + 2)));
                            }
                            ui.close();
                        }
                        if crate::mapping::btn(ui, "Mono mix of all channels").clicked() {
                            self.checkpoint(p);
                            self.add_input_track(p, &d, Mode::Mix, Some(format!("{} mix", capture::short_label(&d.label))));
                            ui.close();
                        }
                    });
                }
            });
            if crate::mapping::btn(ui, "Import...").on_hover_text(crate::lang::tr("Add audio files (WAV, FLAC, MP3 ...) as tracks. Files can also be dropped on the window.")).clicked() && self.open_dlg.is_none() {
                self.open_dlg = Some(dialogs::open_many("Add audio files", "Audio files", decode::EXTENSIONS));
            }
            ui.menu_button(crate::lang::tr("Edit"), |ui| self.edit_menu(ui, p));
            ui.menu_button(crate::lang::tr("Export"), |ui| {
                let range = self.range;
                if crate::mapping::btn(ui, if range.is_some() { "Mix of the selection to WAV..." } else { "Mix to WAV..." }).clicked() {
                    self.ask_export(Export::Mix(range));
                    ui.close();
                }
                if crate::mapping::btn(ui, "Every track as its own WAV...").clicked() {
                    self.ask_export(Export::StemsDir);
                    ui.close();
                }
                if let Some(id) = self.sel.iter().next().copied()
                    && crate::mapping::btn(ui, "Selected clip to WAV...").clicked() {
                        self.ask_export(Export::Clip(id));
                        ui.close();
                    }
                ui.separator();
                if crate::mapping::btn(ui, "Bounce mix to a new track").on_hover_text(crate::lang::tr("Render the selection (or everything) into one new track")).clicked() {
                    self.act_bounce(p, false);
                    ui.close();
                }
                if ui.add_enabled(self.sel_track.is_some(), egui::Button::new(crate::lang::tr("Bounce selected track"))).clicked() {
                    self.act_bounce(p, true);
                    ui.close();
                }
            });
            ui.menu_button(crate::lang::tr("Project"), |ui| {
                if ui.add_enabled(self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty() && self.rec.is_none(), egui::Button::new(crate::lang::tr("New"))).on_disabled_hover_text(crate::lang::tr("Wait for pending work and stop recording")).clicked() {
                    if self.rev != self.named_rev { self.pending_replace = Some(false); }
                    else { self.new_project(cx); *p = self.proj.lock().clone(); }
                    ui.close();
                }
                if ui.add_enabled(self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty() && self.rec.is_none() && self.play.is_none(), egui::Button::new(crate::lang::tr("Open..."))).on_disabled_hover_text(crate::lang::tr("Stop playback and recording; wait for pending work")).clicked() {
                    if self.rev != self.named_rev { self.pending_replace = Some(true); } else { self.open_clicked(); }
                    ui.close();
                }
                if ui.add_enabled(self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty(), egui::Button::new(crate::lang::tr("Save")).shortcut_text("Ctrl+S")).on_disabled_hover_text(crate::lang::tr("Wait for pending timeline work")).clicked() {
                    self.save_clicked(false);
                    ui.close();
                }
                if ui.add_enabled(self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty(), egui::Button::new(crate::lang::tr("Save as..."))).on_disabled_hover_text(crate::lang::tr("Wait for pending timeline work")).clicked() {
                    self.save_clicked(true);
                    ui.close();
                }
                ui.separator();
                if crate::mapping::btn(ui, "Add demo tracks").on_hover_text(crate::lang::tr("Tone, sweep, noise and kick: something to edit and analyse")).clicked() {
                    self.demo();
                    ui.close();
                }
                ui.label(RichText::new(format!("{} Hz project", p.sr)).small().weak());
                if let Some(path) = &self.path {
                    ui.label(RichText::new(path.display().to_string()).small().weak());
                }
            });
            ui.toggle_value(&mut self.an_open, crate::lang::tr("Analysis")).on_hover_text(if self.an_hidden { "Statistics, spectrum and spectrogram of the selection: the window is too small to show them, make it larger" } else { "Statistics, spectrum and spectrogram of the selection (below the tracks, or to their right in a low, wide window)" });
            crate::windows::sticky_menu(ui, crate::lang::tr("Output"), |ui| {
                if graph_on() {
                    ui.label(crate::lang::tr("The timeline plays into the Routing window:\nconnect its Timeline out node to an output or the analyser."));
                    ui.add(egui::DragValue::new(&mut self.rec_off_ms).range(-500.0..=500.0).speed(0.5).suffix(crate::lang::tr(" ms"))).on_hover_text(crate::lang::tr("Shift new recordings earlier by this much to make up for the interface latency"));
                    ui.label(RichText::new(crate::lang::tr("recording latency compensation")).small().weak());
                    return;
                }
                ui.checkbox(&mut self.sound, crate::lang::tr("Play through a sound device"));
                ui.add_enabled_ui(self.sound, |ui| {
                    let cur = cx.out_devs.iter().find(|d| d.id == self.mon_dev).or(cx.out_devs.first()).map(|d| d.label.clone()).unwrap_or_else(|| "no output".into());
                    egui::ComboBox::from_id_salt("daw_out").width(220.0).selected_text(cur).show_ui(ui, |ui| {
                        for d in cx.out_devs {
                            ui.selectable_value(&mut self.mon_dev, d.id.clone(), &d.label);
                        }
                    });
                    { let r = ui.add(egui::Slider::new(&mut self.mon_vol, 0.0..=1.0).text(crate::lang::tr("volume"))); crate::slider::reset(ui, &r, &mut self.mon_vol, 0.7); }
                });
                ui.separator();
                ui.checkbox(&mut self.feed_analysers, crate::lang::tr("Show the timeline in the analysers")).on_hover_text(crate::lang::tr("Off: playing leaves the live input in the spectrum, scope, loudness ... windows.\nOn: they follow the timeline while it plays (the live input pauses).\n'Loop & play' in the analysis panel always does this."));
                ui.add_enabled_ui(self.feed_analysers, |ui| {
                    ui.radio_value(&mut self.an_src, AnSrc::Master, crate::lang::tr("the master mix"));
                    ui.radio_value(&mut self.an_src, AnSrc::Track, crate::lang::tr("the selected track"));
                });
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.rec_off_ms).range(-500.0..=500.0).speed(0.5).suffix(crate::lang::tr(" ms"))).on_hover_text(crate::lang::tr("Shift new recordings earlier by this much to make up for the interface latency"));
                ui.label(RichText::new(crate::lang::tr("recording latency compensation")).small().weak());
                if crate::mapping::btn(ui, "Rescan input devices").clicked() {
                    let d2 = self.devs.clone();
                    std::thread::Builder::new().name("daw-probe".into()).spawn(move || *d2.lock() = Some(capture::probe_all())).ok();
                }
            });
            ui.label(if self.rev != self.named_rev { "Unsaved project changes" } else { "Project saved" });
            ui.label(if self.rev == self.saved_rev { "Recovery saved" } else { "Recovery pending" });
            if self.keyboard_active { ui.label(crate::lang::tr("Timeline shortcuts active")); }
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
        if item(ui, "New track from the selection", "", self.range.is_some()) {
            self.act_track_from_range(p);
            ui.close();
        }
        if item(ui, "Trim clips to the selection", "", self.range.is_some()) {
            self.act_trim_to_range(p);
            ui.close();
        }
        ui.menu_button(crate::lang::tr("Process"), |ui| {
            let ids: Vec<u32> = self.sel.iter().copied().collect();
            if ui.add_enabled(self.sel.len() == 1, egui::Button::new(crate::lang::tr("Spectral editor..."))).on_hover_text(crate::lang::tr("Mark regions on the spectrogram of the clip and attenuate or remove them")).clicked() {
                if let Some(id) = ids.first() {
                    self.open_spec(p, *id);
                }
                ui.close();
            }
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Time-stretch / pitch-shift..."))).clicked() {
                self.stretch = Some(edit_ui::StretchDlg { tempo_pct: 100.0, semis: 0.0, mode: 0 });
                ui.close();
            }
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Remove clicks"))).clicked() {
                self.act_process(p, ids.clone(), edit_ui::EditKind::Declick(12.0));
                ui.close();
            }
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Remove 50 Hz hum"))).clicked() {
                self.act_process(p, ids.clone(), edit_ui::EditKind::Dehum(50.0));
                ui.close();
            }
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Remove 60 Hz hum"))).clicked() {
                self.act_process(p, ids, edit_ui::EditKind::Dehum(60.0));
                ui.close();
            }
        });
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
        ui.menu_button(crate::lang::tr("Gain"), |ui| {
            for (t, d) in [("+1 dB", 1.0f32), ("-1 dB", -1.0), ("+6 dB", 6.0), ("-6 dB", -6.0)] {
                if ui.add_enabled(has_sel, egui::Button::new(t)).clicked() {
                    self.act_clip_prop(p, |c| c.gain_db = (c.gain_db + d).clamp(-90.0, 36.0));
                    ui.close();
                }
            }
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Reset to 0 dB"))).clicked() {
                self.act_clip_prop(p, |c| c.gain_db = 0.0);
                ui.close();
            }
        });
        ui.menu_button(crate::lang::tr("Crossfade length"), |ui| {
            ui.label(RichText::new(crate::lang::tr("Where this clip overlaps another one on the track")).small().weak());
            let sr = p.sr as f32;
            for (t, ms) in [("Default (12 ms)", 0.0f32), ("5 ms", 5.0), ("25 ms", 25.0), ("50 ms", 50.0), ("100 ms", 100.0), ("250 ms", 250.0), ("500 ms", 500.0), ("1 s", 1000.0), ("2 s", 2000.0)] {
                if ui.add_enabled(has_sel, egui::Button::new(t)).clicked() {
                    self.act_clip_prop(p, |c| c.xf = (ms * sr / 1000.0) as usize);
                    ui.close();
                }
            }
        });
        ui.menu_button(crate::lang::tr("Fades"), |ui| {
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
            if ui.add_enabled(has_sel, egui::Button::new(crate::lang::tr("Remove fades"))).clicked() {
                self.act_clip_prop(p, |c| {
                    c.fade_in = 0;
                    c.fade_out = 0;
                });
                ui.close();
            }
        });
    }

    // ------------------------------------------------------------------ keyboard

    fn keys(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &mut Project, _whole: Rect) {
        let ctx = ui.ctx().clone();
        if !self.keyboard_active || ctx.egui_wants_keyboard_input() || self.pending_replace.is_some() {
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
        // K: a marker at the playhead, or a region over the selected range; [ and ]: previous / next marker
        if key(none, Key::K) {
            self.checkpoint(p);
            match self.range.filter(|r| r.1 > r.0) {
                Some((a, b)) => p.add_mark(a, b - a, None),
                None => p.add_mark(self.pos(), 0, None),
            };
            self.touch();
        }
        if key(none, Key::OpenBracket)
            && let Some(f) = p.mark_near(self.pos(), false).map(|m| m.at) {
                self.seek(f);
            }
        if key(none, Key::CloseBracket)
            && let Some(f) = p.mark_near(self.pos(), true).map(|m| m.at) {
                self.seek(f);
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

    /// The nearest start / stop of sound (the beat markers of the waveforms) within reach of frame `f`.
    fn beat_snap(&self, p: &Project, view: &View, f: i64) -> Option<i64> {
        let tol = (14.0 / view.pps * view.sr) as i64;
        let mut best: Option<(i64, i64)> = None;
        for c in p.tracks.iter().filter(|t| !t.draw && t.beats).flat_map(|t| t.clips.iter()) {
            if c.end() < f - tol || c.start > f + tol {
                continue;
            }
            let Some(g) = c.src.grid(view.sr as u32) else { continue };
            for &b in &g.beats {
                if b < c.off || b >= c.off + c.len {
                    continue;
                }
                let rel = b - c.off;
                let t = c.start + if c.reverse { (c.len - 1 - rel) as i64 } else { rel as i64 };
                let d = (t - f).abs();
                if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, t));
                }
            }
        }
        best.map(|b| b.1)
    }

    fn snap_to(&self, view: &View, pts: &[i64], f: i64, alt: bool) -> i64 {
        if !self.snap || alt {
            return f;
        }
        let tol = (10.0 / view.pps * view.sr) as i64;
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
        // clips snap to each other, the playhead and the grid; holding Alt moves them freely
        let alt = ui.input(|i| i.modifiers.alt);
        let shift = ui.input(|i| i.modifiers.shift);
        let hover = lane_resp.hover_pos();
        // the two edges of the selected time range can be grabbed (gizmos at its top and bottom)
        let edge_near = |x: f32, range: Option<(i64, i64)>| -> Option<i64> {
            let (a, b) = range?;
            let (da, db) = ((x - view.x(a)).abs(), (x - view.x(b)).abs());
            if da.min(db) > 7.0 {
                None
            } else if da <= db {
                Some(b)
            } else {
                Some(a)
            }
        };
        if let Some(h) = hover.filter(|h| edge_near(h.x, self.range).is_some()) {
            let _ = h;
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if let Some(h) = hover
            && self.drag.is_none() {
                // only a clip that is selected can be moved, trimmed or faded: the others just show where a selection starts
                match self.hit(p, &view, &rows, h).filter(|k| self.sel.contains(&p.tracks[k.ti].clips[k.ci].id)) {
                    Some(Hit { kind: DragKind::TrimL | DragKind::TrimR, .. }) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                    Some(Hit { kind: DragKind::FadeIn | DragKind::FadeOut, .. }) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                    Some(_) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                    None => ui.ctx().set_cursor_icon(CursorIcon::Text),
                }
            }
        if lane_resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some((ti, _)) = ui.input(|i| i.pointer.press_origin()).and_then(|pos| Self::draw_row_at(p, &rows, pos)) {
                // on a draw track the mouse draws
                self.checkpoint(p);
                self.draw_drag = Some((ti, None));
                self.sel_track = Some(p.tracks[ti].id);
            } else if let Some(anchor) = ui.input(|i| i.pointer.press_origin()).and_then(|pos| edge_near(pos.x, self.range)) {
                // an edge of the range: the opposite one stays, this one follows the mouse
                self.range_anchor = Some(anchor);
            } else if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                let auto_grab = self.auto_hit(p, &view, &rows, pos);
                if let Some(g) = auto_grab {
                    self.checkpoint(p);
                    self.auto_drag = Some(g);
                }
                // a clip has to be selected (clicked) first; dragging an unselected one selects a time range instead
                match self.hit(p, &view, &rows, pos).filter(|k| auto_grab.is_none() && self.sel.contains(&p.tracks[k.ti].clips[k.ci].id)) {
                    Some(h) => {
                        let id = p.tracks[h.ti].clips[h.ci].id;
                        self.sel_track = Some(p.tracks[h.ti].id);
                        self.checkpoint(p);
                        let orig: Vec<(Clip, usize)> = if h.kind == DragKind::Move {
                            self.sel.iter().filter_map(|i| p.find_clip(*i).map(|(ti, ci)| (p.tracks[ti].clips[ci].clone(), ti))).collect()
                        } else {
                            vec![(p.tracks[h.ti].clips[h.ci].clone(), h.ti)]
                        };
                        self.drag = Some(ClipDrag { kind: h.kind, press_frame: view.frame(pos.x), press_track: h.ti, grab: id, orig });
                    }
                    None if auto_grab.is_some() => {}
                    None => {
                        let f = view.frame(pos.x);
                        let pts = self.snap_points(p, &[]);
                        let f = if shift { self.beat_snap(p, &view, f) } else { None }.unwrap_or_else(|| self.snap_to(&view, &pts, f, alt));
                        self.range_anchor = Some(f.max(0));
                        if let Some(ti) = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()) {
                            self.sel_track = Some(p.tracks[ti].id);
                        }
                    }
                }
            }
        }
        if lane_resp.dragged()
            && let Some(ptr) = lane_resp.interact_pointer_pos() {
                if let Some((ti, last)) = self.draw_drag {
                    let row = rows[ti];
                    let v = (((row.bottom() - 4.0 - ptr.y) / (row.height() - 8.0).max(1.0)).clamp(0.0, 1.0) * 2.0 - 1.0).clamp(-1.0, 1.0);
                    let f = view.frame(ptr.x).max(0);
                    // (a stroke is a point about every other pixel, not one for every mouse event)
                    let min_gap = (view.sr / view.pps * 1.5) as i64;
                    let t = &mut p.tracks[ti];
                    match last {
                        None => {
                            t.curve_set(f, v);
                            self.draw_drag = Some((ti, Some((f, v))));
                            self.touch();
                        }
                        Some((lf, lv)) if (f - lf).abs() >= min_gap.max(1) => {
                            t.curve_stroke(lf, lv, f, v);
                            self.draw_drag = Some((ti, Some((f, v))));
                            self.touch();
                        }
                        _ => {}
                    }
                } else if let Some((ti, pt)) = self.auto_drag {
                    // a point of the volume automation: along the time axis between its neighbours, and in level
                    if let Some(t) = p.tracks.get_mut(ti).filter(|t| pt < t.auto.len()) {
                        let lo = if pt > 0 { t.auto[pt - 1].0 + 1 } else { 0 };
                        let hi = if pt + 1 < t.auto.len() { t.auto[pt + 1].0 - 1 } else { i64::MAX / 4 };
                        let fr = self.snap_to(&view, &[], view.frame(ptr.x), alt).clamp(lo, hi.max(lo));
                        let row = rows[ti];
                        let u = ((row.bottom() - 4.0 - ptr.y) / (row.height() - 8.0).max(1.0)).clamp(0.0, 1.0);
                        t.auto[pt] = (fr, (AUTO_RANGE.0 + u * (AUTO_RANGE.1 - AUTO_RANGE.0)).clamp(AUTO_RANGE.0, AUTO_RANGE.1));
                        self.touch();
                    }
                } else if let Some(d) = self.drag.take() {
                    self.update_clip_drag(p, &view, &rows, &d, ptr, alt);
                    self.drag = Some(d);
                } else if let Some(a) = self.range_anchor {
                    let pts = self.snap_points(p, &[]);
                    let raw = view.frame(ptr.x);
                    let f = if shift { self.beat_snap(p, &view, raw) } else { None }.unwrap_or_else(|| self.snap_to(&view, &pts, raw, alt)).max(0);
                    self.range = if (f - a).abs() as f64 / sr * self.pps >= 3.0 { Some((a.min(f), a.max(f))) } else { None };
                    // keep the pointer in reach: scroll when dragging past the edges
                    if ptr.x > rect.right() - 12.0 {
                        self.t0 += 20.0 / self.pps;
                    } else if ptr.x < lanes_left + 12.0 {
                        self.t0 = (self.t0 - 20.0 / self.pps).max(0.0);
                    }
                }
            }
        if lane_resp.drag_stopped() {
            // a selection made during playback with the loop on: the playhead goes into it (it would only
            // reach the loop after playing up to its end, or never when it is further on)
            if self.range_anchor.is_some() && self.loop_on && self.play.is_some()
                && let Some((a, b)) = self.range.filter(|r| r.1 > r.0 + 64) {
                    if let Some(pl) = &self.play {
                        pl.loop_a.store(a, Relaxed);
                        pl.loop_b.store(b, Relaxed);
                    }
                    let pos = self.pos();
                    if pos < a || pos >= b {
                        self.seek(a);
                    }
                }
            self.auto_drag = None;
            self.drag = None;
            self.range_anchor = None;
        }
        if lane_resp.clicked()
            && let Some(pos) = lane_resp.interact_pointer_pos() {
                let on_row = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom());
                // the upper bar of a track (the clip's name strip) selects; the waveform below it places the playback cursor
                let in_bar = on_row.is_some_and(|ti| pos.y < rows[ti].top() + TRACK_BAR_H);
                match self.hit(p, &view, &rows, pos) {
                    // a click on an automation point of the edited track is not a click on the clip below it
                    _ if self.auto_hit(p, &view, &rows, pos).is_some() => {}
                    _ if on_row.is_some() && !in_bar => {
                        let pts = self.snap_points(p, &[]);
                        let f = self.snap_to(&view, &pts, view.frame(pos.x), alt).max(0);
                        self.seek(f);
                        if !shift {
                            self.range = None;
                        }
                    }
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
                        // the upper bar of an empty stretch of a track, or the space below the tracks
                        if !shift {
                            self.sel.clear();
                            self.range = None;
                        }
                        self.sel_track = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()).map(|ti| p.tracks[ti].id);
                    }
                }
            }
        if lane_resp.double_clicked()
            && let Some(pos) = lane_resp.interact_pointer_pos() {
                // while a track's automation is edited, a double-click on its lane makes a new point
                let on_auto = self.auto_edit.and_then(|id| p.track_index(id)).filter(|&ti| pos.y >= rows[ti].top() && pos.y < rows[ti].bottom());
                if let Some(ti) = on_auto {
                    if self.auto_hit(p, &view, &rows, pos).is_none() {
                        self.checkpoint(p);
                        let row = rows[ti];
                        let u = ((row.bottom() - 4.0 - pos.y) / (row.height() - 8.0).max(1.0)).clamp(0.0, 1.0);
                        let fr = self.snap_to(&view, &self.snap_points(p, &[]), view.frame(pos.x), alt).max(0);
                        p.tracks[ti].auto_set(fr, AUTO_RANGE.0 + u * (AUTO_RANGE.1 - AUTO_RANGE.0));
                        self.touch();
                    }
                } else if let Some(h) = self.hit(p, &view, &rows, pos) {
                    let c = &p.tracks[h.ti].clips[h.ci];
                    self.range = Some((c.start, c.end()));
                    self.sel.clear();
                    self.sel.insert(c.id);
                }
            }
        if lane_resp.drag_stopped() {
            self.draw_drag = None;
        }
        // holding the right button and moving pans the timeline (a right-click without moving still opens the menu)
        if lane_resp.dragged_by(egui::PointerButton::Secondary) {
            let dx = lane_resp.drag_delta().x;
            self.t0 = (self.t0 - dx as f64 / self.pps).max(0.0);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if lane_resp.secondary_clicked()
            && let Some(pos) = lane_resp.interact_pointer_pos() {
                self.auto_ctx = self.auto_hit(p, &view, &rows, pos);
                self.ctx_clip = if self.auto_ctx.is_some() { None } else { self.hit(p, &view, &rows, pos).map(|h| p.tracks[h.ti].clips[h.ci].id) };
                if let Some(id) = self.ctx_clip
                    && !self.sel.contains(&id) {
                        self.sel.clear();
                        self.sel.insert(id);
                    }
                if let Some(ti) = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom()) {
                    self.sel_track = Some(p.tracks[ti].id);
                }
                self.ctx_frame = self.snap_to(&view, &self.snap_points(p, &[]), view.frame(pos.x), alt).max(0);
            }
        lane_resp.context_menu(|ui| {
            if let Some((ti, pt)) = self.auto_ctx {
                if crate::mapping::btn(ui, "Delete this automation point").clicked() {
                    self.checkpoint(p);
                    if ti < p.tracks.len() && pt < p.tracks[ti].auto.len() {
                        p.tracks[ti].auto.remove(pt);
                    }
                    self.auto_ctx = None;
                    self.touch();
                    ui.close();
                }
            } else if self.ctx_clip.is_some() {
                self.edit_menu(ui, p);
                if let Some(id) = self.ctx_clip {
                    ui.separator();
                    if crate::mapping::btn(ui, "Select the clip's time range").clicked() {
                        if let Some((ti, ci)) = p.find_clip(id) {
                            self.range = Some((p.tracks[ti].clips[ci].start, p.tracks[ti].clips[ci].end()));
                        }
                        ui.close();
                    }
                }
            } else if let Some(ti) = self.sel_track.and_then(|t| p.track_index(t)).filter(|&ti| p.tracks[ti].draw) {
                if crate::mapping::btn(ui, "Clear the drawing").clicked() {
                    self.checkpoint(p);
                    p.tracks[ti].curve.clear();
                    self.touch();
                    ui.close();
                }
                if crate::mapping::btn(ui, "Smooth the drawing").on_hover_text(crate::lang::tr("Average every point with its neighbours")).clicked() {
                    self.checkpoint(p);
                    let c = p.tracks[ti].curve.clone();
                    for i in 1..c.len().saturating_sub(1) {
                        p.tracks[ti].curve[i].1 = (c[i - 1].1 + 2.0 * c[i].1 + c[i + 1].1) * 0.25;
                    }
                    self.touch();
                    ui.close();
                }
            } else {
                if let Some(ti) = self.sel_track.and_then(|t| p.track_index(t)) {
                    let (tid, f) = (p.tracks[ti].id, self.ctx_frame);
                    if crate::mapping::btn(ui, format!("Import audio file here ({})...", p.tracks[ti].name)).clicked() && self.open_dlg.is_none() {
                        self.import_target = Some((tid, f));
                        self.open_dlg = Some(dialogs::open_many("Add audio to the track", "Audio files", decode::EXTENSIONS));
                        ui.close();
                    }
                    if !graph_on() {
                        ui.menu_button(crate::lang::tr("Record input of this track"), |ui| self.input_menu(ui, p, ti));
                    }
                    ui.separator();
                }
                if crate::mapping::btn(ui, "Split everything here (S)").clicked() {
                    self.act_split(p);
                    ui.close();
                }
                if ui.add_enabled(!self.clip_buf.is_empty(), egui::Button::new(crate::lang::tr("Paste"))).clicked() {
                    self.act_paste(p);
                    ui.close();
                }
                if ui.add_enabled(self.range.is_some(), egui::Button::new(crate::lang::tr("Delete the selected time range"))).clicked() {
                    self.act_delete(p);
                    ui.close();
                }
                if ui.add_enabled(self.range.is_some(), egui::Button::new(crate::lang::tr("New track from the selection"))).on_hover_text(crate::lang::tr("The selected time range on a new track: the clips of the selected track cut to it, or with no track selected the mix of everything")).clicked() {
                    self.act_track_from_range(p);
                    ui.close();
                }
                if crate::mapping::btn(ui, "Select all (Ctrl+A)").clicked() {
                    self.act_select_all(p);
                    ui.close();
                }
            }
        });

        // ---- ruler interaction (seek, markers)
        let ruler_resp = ui.interact(ruler, ui.scope_id().with("daw_ruler"), Sense::click_and_drag());
        let mark_at = |x: f32, y: f32| -> Option<u32> {
            if y < ruler.center().y - 2.0 {
                return None;
            }
            p.marks.iter().map(|m| (m.id, (view.x(m.at) - x).abs())).filter(|(_, d)| *d <= 7.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(id, _)| id)
        };
        if ruler_resp.secondary_clicked()
            && let Some(pos) = ruler_resp.interact_pointer_pos() {
                self.ruler_ctx = (view.frame(pos.x.max(lanes_left)).max(0), mark_at(pos.x, pos.y));
            }
        let on_mark = ruler_resp.interact_pointer_pos().and_then(|pos| mark_at(pos.x, pos.y));
        if ruler_resp.clicked() && on_mark.is_some() {
            // a click on a flag selects its region (or moves the playhead to the marker)
            if let Some(m) = p.marks.iter().find(|m| Some(m.id) == on_mark).cloned() {
                self.range = if m.len > 0 { Some((m.at, m.at + m.len)) } else { None };
                self.seek(m.at);
            }
        } else if (ruler_resp.clicked() || (ruler_resp.dragged() && on_mark.is_none()))
            && let Some(pos) = ruler_resp.interact_pointer_pos() {
                let pts = self.snap_points(p, &[]);
                let f = self.snap_to(&view, &pts, view.frame(pos.x.max(lanes_left)), alt).max(0);
                // while the mouse rests on one spot during playback, the sound goes on from there: seeking again on every
                // frame would replay the same few milliseconds over and over
                let id = ui.scope_id().with("daw_scrub_last");
                let last: Option<i64> = ui.data(|d| d.get_temp(id));
                if ruler_resp.clicked() || last != Some(f) {
                    self.seek(f);
                    ui.data_mut(|d| d.insert_temp(id, f));
                }
            }
        if ruler_resp.drag_stopped() {
            let id = ui.scope_id().with("daw_scrub_last");
            ui.data_mut(|d| d.remove::<i64>(id));
        }
        egui::Popup::context_menu(&ruler_resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            let (frame, hit) = self.ruler_ctx;
            if crate::mapping::btn(ui, "Add a marker here").clicked() {
                self.checkpoint(p);
                p.add_mark(frame, 0, None);
                self.touch();
                ui.close();
            }
            if ui.add_enabled(self.range.is_some_and(|r| r.1 > r.0), egui::Button::new(crate::lang::tr("Add a region from the selection"))).clicked() {
                if let Some((a, b)) = self.range {
                    self.checkpoint(p);
                    p.add_mark(a, b - a, None);
                    self.touch();
                }
                ui.close();
            }
            if let Some(i) = hit.and_then(|id| p.marks.iter().position(|m| m.id == id)) {
                ui.separator();
                let mut name = p.marks[i].name.clone();
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(150.0)).changed() {
                    p.marks[i].name = name.chars().take(60).collect();
                    self.touch();
                }
                let m = p.marks[i].clone();
                if m.len > 0 {
                    if crate::mapping::btn(ui, "Select and loop this region").clicked() {
                        self.range = Some((m.at, m.at + m.len));
                        self.loop_on = true;
                        self.seek(m.at);
                        ui.close();
                    }
                    if crate::mapping::btn(ui, "Select this region").clicked() {
                        self.range = Some((m.at, m.at + m.len));
                        ui.close();
                    }
                } else if crate::mapping::btn(ui, "Move the playhead here").clicked() {
                    self.seek(m.at);
                    ui.close();
                }
                if crate::mapping::btn(ui, "Delete").clicked() {
                    self.checkpoint(p);
                    p.marks.remove(i);
                    self.touch();
                    ui.close();
                }
            }
        });

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
                self.draw_clip(&lp, &view, lane, c, col, selected_ids.contains(&c.id), chans, t.ch_mute, dimmed, t.beats);
            }
            // the shelved takes as thin bars along the bottom of the lane, one row per take
            if !t.shelf.is_empty() && self.takes_view.contains(&t.id) {
                let mut nums: Vec<u32> = t.shelf.iter().map(|(k, _)| *k).collect();
                nums.sort_unstable();
                nums.dedup();
                for (k, c) in &t.shelf {
                    let row = nums.iter().position(|n| n == k).unwrap_or(0).min(5) as f32;
                    let r = Rect::from_min_max(pos2(view.x(c.start), lane.bottom() - 6.0 * (row + 1.0) - 1.0), pos2(view.x(c.end()).max(view.x(c.start) + 2.0), lane.bottom() - 6.0 * row - 1.0));
                    lp.rect_filled(r, 1.0, col.gamma_multiply(0.75));
                    lp.rect_stroke(r, 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)), egui::StrokeKind::Inside);
                    if r.width() > 14.0 {
                        lp.text(r.left_center() + vec2(2.0, 0.0), Align2::LEFT_CENTER, format!("{k}"), FontId::proportional(8.0), Color32::from_rgb(10, 12, 16));
                    }
                }
            }
            if t.draw {
                self.draw_curve(&lp, &view, lane, row, t);
            }
            if !t.auto.is_empty() || self.auto_edit == Some(t.id) {
                self.draw_auto(&lp, &view, lane, row, t);
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
            // the gizmos: a tab at the top and the bottom of each edge
            for x in [xa, xb] {
                for (y, up) in [(lanes_area.top(), true), (lanes_area.bottom(), false)] {
                    let (w, h) = (7.0, 9.0);
                    let t = Rect::from_min_max(pos2(x - w * 0.5, if up { y } else { y - h }), pos2(x + w * 0.5, if up { y + h } else { y }));
                    pl.rect_filled(t, 2.0, Color32::from_rgb(120, 170, 255));
                }
            }
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
            if r.on_hover_text(crate::lang::tr("Add an empty track")).clicked() && p.tracks.len() < MAX_TRACKS {
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
        if let Some(tid) = self.split_req.take()
            && let Some(ti) = p.track_index(tid) {
                self.checkpoint(p);
                if let Some(n) = p.split_channels(ti) {
                    self.sel_track = Some(p.tracks[n].id);
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
        self.draw_ruler(&painter, &view, ruler, rect, &p.marks);
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
        // a white guide line where the ruler would put the playback cursor
        let over_wave = |h: Pos2| rows.iter().any(|r| h.y >= r.top() + TRACK_BAR_H && h.y < r.bottom());
        if let (Some(h), None) = (hover, &self.drag)
            && lanes_area.contains(h) && self.range_anchor.is_none() && over_wave(h) {
                let pts = self.snap_points(p, &[]);
                let f = self.snap_to(&view, &pts, view.frame(h.x), alt).max(0);
                let x = view.x(f);
                painter.with_clip_rect(lanes_area.union(ruler)).line_segment([pos2(x, ruler.top()), pos2(x, lanes_area.bottom())], Stroke::new(1.0, Color32::from_white_alpha(110)));
                painter.with_clip_rect(ruler).add(Shape::convex_polygon(vec![pos2(x - 4.0, ruler.bottom() - 7.0), pos2(x + 4.0, ruler.bottom() - 7.0), pos2(x, ruler.bottom())], Color32::from_white_alpha(170), Stroke::NONE));
            }
        // hover readout
        if let (Some(h), None) = (hover, &self.drag)
            && lanes_area.contains(h) && over_wave(h) {
                let t = view.frame(h.x).max(0) as f64 / sr;
                painter.text(pos2(h.x + 8.0, ruler.bottom() + 2.0), Align2::LEFT_TOP, clock(t as f32), FontId::monospace(9.5), Color32::from_white_alpha(120));
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

}

impl Daw {
    fn project_confirmation(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        if self.pending_replace.is_none() { return; }
        egui::Window::new(crate::lang::tr("Unsaved timeline changes")).collapsible(false).resizable(false).show(ui.ctx(), |ui| {
            ui.label(crate::lang::tr("Save changes to your project before replacing this timeline?"));
            ui.label(crate::lang::tr("Recovery autosave does not update your named project."));
            ui.horizontal(|ui| {
                let ready = self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty();
                if ui.add_enabled(ready, egui::Button::new(crate::lang::tr("Save"))).clicked() {
                    self.replace_after_save = true; self.save_clicked(false);
                }
                if ui.add_enabled(ready, egui::Button::new(crate::lang::tr("Discard"))).clicked() {
                    self.replace_after_save = false;
                    if let Some(open) = self.pending_replace.take() {
                        if open { self.open_clicked(); } else { self.new_project(cx); }
                    }
                }
                if ui.button(crate::lang::tr("Cancel")).clicked() { self.pending_replace = None; self.replace_after_save = false; }
            });
        });
    }
}
