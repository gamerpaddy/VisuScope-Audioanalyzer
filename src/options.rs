//! Options dialog: settings that are not needed while measuring, kept out of the side bar and the windows.

use crate::app::*;
use eframe::egui::{self, RichText};

impl App {
    /// Every frame: hand the generators to the routing graph and follow switching the graph on or off.
    pub fn sync_graph_mode(&mut self) {
        use crate::tools::ROUTING;
        // test hook: VISUSCOPE_GRAPH=demo (or 1) switches the graph on, demo also starts two generators
        if let Ok(v) = std::env::var("VISUSCOPE_GRAPH") {
            if !self.graph_hook {
                self.graph_hook = true;
                self.graph_on = true;
                if v == "demo" {
                    self.tg.on = true;
                    self.tg.level_db = [-14.0; 2];
                    self.gen_extra = vec![crate::tonegen::GenParams { on: true, wave: crate::tonegen::Wave::Square, freq: 220.0, level_db: [-20.0; 2], ..Default::default() }];
                }
            }
        }
        // test hook: VISUSCOPE_OPEN=Profiler,Timeline opens those tool windows once
        if self.layout_ready && !self.open_hook {
            self.open_hook = true;
            if let Ok(v) = std::env::var("VISUSCOPE_OPEN") {
                for name in v.split(',') {
                    if let Some(i) = (0..crate::windows::WIN_COUNT).find(|&i| crate::windows::win_short(i).eq_ignore_ascii_case(name.trim())) {
                        self.wins[i].open = true;
                        self.need_tile = true;
                    }
                }
            }
        }
        crate::prof::set_safety_ms(self.safety_ms);
        crate::tools::daw::IMPORT_AT_START.store(self.import_at_start, std::sync::atomic::Ordering::Relaxed);
        let mut all = vec![self.tg.clone()];
        all.extend(self.gen_extra.iter().cloned());
        if all != self.gens_sent {
            *self.gens_shared.lock() = all.clone();
            self.gens_sent = all;
        }
        let k = crate::windows::BUILTIN + ROUTING;
        if !self.graph_on {
            self.wins[k].open = false;
        }
        if self.graph_on != self.graph_applied {
            self.graph_applied = self.graph_on;
            if self.graph_on {
                // the graph opens the devices itself
                self.audio = None;
                self.gen_handle = None;
                self.wins[k].open = true;
                self.need_tile |= self.auto_tile;
            } else {
                self.gen_tab = 0;
                self.start_selected();
            }
        }
    }

    pub fn options_ui(&mut self, ctx: &egui::Context) {
        // test hook: VISUSCOPE_GALLERY=1 shows the layout previews in a window (for screenshots)
        if self.layout_ready && std::env::var_os("VISUSCOPE_GALLERY").is_some() {
            egui::Window::new("Layouts").order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.set_max_width(570.0);
                self.builtin_grid(ui);
            });
        }
        if !self.opts_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Options").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(360.0).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new("Windows").strong());
            ui.checkbox(&mut self.snap_on, "Snap windows to each other and to the edges").on_hover_text("Ctrl while dragging moves a window freely. Alt while resizing leaves the neighbours alone.");
            ui.checkbox(&mut self.auto_tile, "Re-tile automatically when a window is opened or closed");
            ui.horizontal(|ui| {
                if ui.add_enabled(self.lay_can_undo(), egui::Button::new("Undo layout change")).on_hover_text("Ctrl+Alt+Z").clicked() {
                    self.layout_undo();
                }
                if ui.add_enabled(self.lay_can_redo(), egui::Button::new("Redo layout change")).on_hover_text("Ctrl+Alt+Y").clicked() {
                    self.layout_redo();
                }
            });
            ui.separator();
            ui.label(RichText::new("Interface size").strong());
            // the size is applied with the button: the slider itself would grow or shrink under the pointer while it is dragged
            let mut v = self.ui_scale_edit.unwrap_or(self.ui_scale);
            ui.horizontal(|ui| {
                let sr = ui.add(egui::Slider::new(&mut v, 0.7..=3.0).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)).text("text and widgets"));
                let mut ch = sr.changed() | crate::slider::reset(ui, &sr, &mut v, 1.0);
                for (label, p) in [("100", 1.0f32), ("150", 1.5), ("200", 2.0), ("250", 2.5)] {
                    if crate::mapping::small_btn(ui, label).clicked() {
                        v = p;
                        ch = true;
                    }
                }
                if ch {
                    self.ui_scale_edit = Some(v);
                }
            });
            ui.horizontal(|ui| {
                let pending = self.ui_scale_edit.is_some_and(|e| (e - self.ui_scale).abs() > 1e-3);
                if ui.add_enabled(pending, egui::Button::new("Apply")).clicked() {
                    self.ui_scale = v;
                    self.ui_scale_edit = None;
                }
                if pending && crate::mapping::btn(ui, "Cancel").clicked() {
                    self.ui_scale_edit = None;
                }
                ui.label(RichText::new(format!("now {:.0} %", self.ui_scale * 100.0)).small().weak());
            });
            ui.label(RichText::new("Makes everything larger or smaller, for high-resolution screens (Ctrl + plus / Ctrl + minus do the same at once). The windows keep their arrangement.").small().weak());
            ui.separator();
            ui.label(RichText::new("Routing").strong());
            ui.label(RichText::new("Inputs, generators, the timeline and the output devices are nodes in the Routing window; the analysers read what you connect to the Analyzer node. The Presets menu there sets up a patch for a task.").small().weak());
            ui.separator();
            ui.label(RichText::new("Safety buffer").strong());
            ui.horizontal(|ui| {
                { let r = ui.add(egui::Slider::new(&mut self.safety_ms, 0..=500).suffix(" ms").text("extra delay")); crate::slider::reset(ui, &r, &mut self.safety_ms, 0); }
                for (label, v) in [("off", 0u32), ("40", 40), ("100", 100), ("250", 250)] {
                    if crate::mapping::small_btn(ui, label).clicked() {
                        self.safety_ms = v;
                    }
                }
            });
            ui.label(RichText::new("Holds back that much audio before it is played (output devices, monitor, timeline playback) and cushions the sources of the routing graph, so short stalls of the system do not become gaps or crackles. It adds the same amount of delay: raise it when the Profiler reports an output that ran dry.").small().weak());
            ui.separator();
            ui.label(RichText::new("Performance").strong());
            { let r = ui.add(egui::Slider::new(&mut self.fps_cap, 15.0..=241.0).text("UI frame rate cap").custom_formatter(|v, _| if v > 240.0 { "max".into() } else { format!("{v:.0}") })); crate::slider::reset(ui, &r, &mut self.fps_cap, 60.0); }
            { let r = ui.add(egui::Slider::new(&mut self.readout_hz, 0.5..=10.0).text("Readout update rate (Hz)")); crate::slider::reset(ui, &r, &mut self.readout_hz, 3.0); }
            ui.checkbox(&mut self.readout_hold, "Hold the numeric readouts");
            ui.separator();
            ui.label(RichText::new("Input mappings").strong());
            ui.horizontal(|ui| {
                if crate::mapping::btn(ui, "Keys, MIDI and controllers...").on_hover_text("Bind keys, MIDI notes and controllers, game controller and joystick buttons and axes to actions and sliders").clicked() {
                    self.map.open = true;
                }
                ui.label(RichText::new(format!("{} bound", self.map.bindings.len())).small().weak());
            });
            ui.separator();
            ui.label(RichText::new("Files").strong());
            ui.checkbox(&mut self.import_at_start, "Import audio into the timeline at its start, not at the cursor").on_hover_text("Files added to the timeline (the Import button, drag and drop, the track menus) begin at the leftmost position, so several imports line up. Off: they are placed at the cursor.");
            ui.horizontal(|ui| {
                let dir = crate::tools::dialogs::last_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "none yet (Documents)".into());
                ui.label(RichText::new(format!("Open / save dialogs start in: {dir}")).small());
                if crate::mapping::small_btn(ui, "Forget").clicked() {
                    crate::tools::dialogs::forget_dir();
                }
            });
            ui.separator();
            ui.label(RichText::new(format!("VisuScope {}", crate::VERSION_FULL)).small().weak());
        });
        self.opts_open = open;
    }
}
