//! Floating, draggable, resizable, toggleable panels (built-in views and the pluggable tools).

use crate::app::*;
use crate::tools::{self, TOOL_COUNT};
use eframe::egui::{self, pos2, vec2, Rect, Ui};
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
    /// bumping this resets egui's stored window state so a new default rect applies
    pub epoch: u32,
    /// rect to return to after "maximize"
    pub prev: Option<Rect>,
}

impl WinState {
    pub fn new(i: usize) -> Self {
        Self { open: i < BUILTIN && OPEN[i], rect: None, epoch: 0, prev: None }
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
        let mut out: Vec<(usize, Rect)> = Vec::new();
        split_rects(&items, area.shrink(5.0), 5.0, &mut out);
        for (i, r) in out {
            if self.prof {
                eprintln!("tile {i} {}: {:?}", self.win_title(i), r);
            }
            self.wins[i].rect = Some(r);
            self.wins[i].epoch += 1;
            self.wins[i].prev = None;
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
            w.rect = Some(area.shrink(4.0));
        }
        w.epoch += 1;
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
            let casc = i as f32 * 24.0;
            let r = self.wins[i].rect.unwrap_or_else(|| Rect::from_min_size(area.min + vec2(30.0 + casc, 30.0 + casc), vec2(560.0, 380.0)));
            let id = egui::Id::new(("visuscope_window", i, self.wins[i].epoch));
            let mut open = true;
            let resp = egui::Window::new(self.win_title(i))
                .id(id)
                .open(&mut open)
                .resizable(true)
                .constrain_to(area)
                .min_size(vec2(240.0, 150.0))
                .default_pos(pos2(r.min.x, r.min.y))
                .default_size(r.size().max(vec2(240.0, 150.0)))
                .show(ctx, |ui: &mut Ui| self.window_content(ui, i));
            if let Some(ir) = resp {
                if self.prof && self.frames % 60 == 0 {
                    eprintln!("win {i} {}: want {:?} got {:?}", self.win_title(i), r, ir.response.rect);
                }
                self.wins[i].rect = Some(ir.response.rect);
            }
            if !open {
                self.wins[i].open = false;
            }
            self.ms[i] += 0.1 * (t0.elapsed().as_secs_f32() * 1000.0 - self.ms[i]);
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
                let open_tools = (BUILTIN..WIN_COUNT).filter(|&i| self.wins[i].open).count();
                let label = if open_tools > 0 { format!("Tools ({open_tools})") } else { "Tools".to_string() };
                ui.menu_button(label, |ui| {
                    for (g, (name, members)) in tools::GROUPS.iter().enumerate() {
                        if g > 0 {
                            ui.separator();
                        }
                        ui.label(egui::RichText::new(*name).small().weak());
                        for &k in members.iter() {
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
            });
        });
    }

    pub fn status_bar_ui(&mut self, ui: &mut Ui) {
        let statuses = self.tool_statuses();
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let xr = self.sh.xruns.load(std::sync::atomic::Ordering::Relaxed);
                ui.label(egui::RichText::new(format!("{}   xruns {xr}", self.sh.status.lock())).small());
                if self.sh.feeding_silence.load(std::sync::atomic::Ordering::Relaxed) {
                    ui.label(egui::RichText::new("no audio from device (silent loopback or stalled stream) - showing silence").small().color(egui::Color32::from_rgb(255, 190, 80)));
                }
                for (text, col) in statuses {
                    ui.separator();
                    ui.label(egui::RichText::new(text).small().strong().color(col));
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("UI {:.0} fps ({:.1} ms)   DSP {:.0} fps ({:.2} ms)", self.ui_fps, self.ms[WIN_COUNT], self.an.dsp_fps, self.an.dsp_ms)).small().weak());
                if !self.note.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new(&self.note).small());
                }
            });
        });
    }

    pub fn request_png(&mut self, ctx: &egui::Context) {
        let note = self.shot_note.clone();
        ctx.request_screenshot(move |img| {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let path = format!("visuscope_{ts}.png");
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
