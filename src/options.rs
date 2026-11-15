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
            egui::Window::new(crate::lang::tr("Layouts")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.set_max_width(570.0);
                self.builtin_grid(ui);
            });
        }
        if !self.opts_open {
            return;
        }
        let mut open = true;
        egui::Window::new(crate::lang::tr("Options")).open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(360.0).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(crate::lang::tr("Windows")).strong());
            ui.checkbox(&mut self.snap_on, crate::lang::tr("Snap windows to each other and to the edges")).on_hover_text(crate::lang::tr("Ctrl while dragging moves a window freely. Alt while resizing leaves the neighbours alone."));
            ui.checkbox(&mut self.auto_tile, crate::lang::tr("Re-tile automatically when a window is opened or closed"));
            ui.horizontal(|ui| {
                if ui.add_enabled(self.lay_can_undo(), egui::Button::new(crate::lang::tr("Undo layout change"))).on_hover_text(crate::lang::tr("Ctrl+Alt+Z")).clicked() {
                    self.layout_undo();
                }
                if ui.add_enabled(self.lay_can_redo(), egui::Button::new(crate::lang::tr("Redo layout change"))).on_hover_text(crate::lang::tr("Ctrl+Alt+Y")).clicked() {
                    self.layout_redo();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(crate::lang::tr("Language"));
                let cur = crate::lang::current();
                egui::ComboBox::from_id_salt("opt_lang").selected_text(cur.clone()).show_ui(ui, |ui| {
                    for l in crate::lang::available() {
                        if ui.selectable_label(l == cur, &l).clicked() {
                            crate::lang::set(&l);
                        }
                    }
                });
            })
            .response
            .on_hover_text(crate::lang::tr("Language of the interface. Text that is built from numbers stays English for now."));
            ui.checkbox(&mut self.light, crate::lang::tr("Light interface")).on_hover_text(crate::lang::tr("Light menus, panels and window frames. The plots stay dark, like the screen of a scope"));
            ui.label(RichText::new(crate::lang::tr("Interface size")).strong());
            // the size is applied with the button: the slider itself would grow or shrink under the pointer while it is dragged
            let mut v = self.ui_scale_edit.unwrap_or(self.ui_scale);
            ui.horizontal(|ui| {
                let sr = ui.add(egui::Slider::new(&mut v, 0.7..=3.0).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)).text(crate::lang::tr("text and widgets")));
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
                if ui.add_enabled(pending, egui::Button::new(crate::lang::tr("Apply scale"))).clicked() {
                    self.ui_scale = v;
                    self.ui_scale_edit = None;
                }
                if pending && crate::mapping::btn(ui, "Revert scale").clicked() {
                    self.ui_scale_edit = None;
                }
                ui.label(RichText::new(format!("now {:.0} %", self.ui_scale * 100.0)).small().weak());
            });
            ui.label(RichText::new(crate::lang::tr("Makes everything larger or smaller, for high-resolution screens (Ctrl + plus / Ctrl + minus do the same at once). The windows keep their arrangement.")).small().weak());
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Routing")).strong());
            ui.label(RichText::new(crate::lang::tr("Inputs, generators, the timeline and the output devices are nodes in the Routing window; the analysers read what you connect to the Analyzer node. The Presets menu there sets up a patch for a task.")).small().weak());
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Safety buffer")).strong());
            ui.horizontal(|ui| {
                { let r = ui.add(egui::Slider::new(&mut self.safety_ms, 0..=500).suffix(crate::lang::tr(" ms")).text(crate::lang::tr("extra delay"))); crate::slider::reset(ui, &r, &mut self.safety_ms, 0); }
                for (label, v) in [("off", 0u32), ("40", 40), ("100", 100), ("250", 250)] {
                    if crate::mapping::small_btn(ui, label).clicked() {
                        self.safety_ms = v;
                    }
                }
            });
            ui.label(RichText::new(crate::lang::tr("Holds back that much audio before it is played (output devices, monitor, timeline playback) and cushions the sources of the routing graph, so short stalls of the system do not become gaps or crackles. It adds the same amount of delay: raise it when the Profiler reports an output that ran dry.")).small().weak());
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Performance")).strong());
            { let r = ui.add(egui::Slider::new(&mut self.fps_cap, 15.0..=241.0).text(crate::lang::tr("UI frame rate cap")).custom_formatter(|v, _| if v > 240.0 { "max".into() } else { format!("{v:.0}") })); crate::slider::reset(ui, &r, &mut self.fps_cap, 60.0); }
            { let r = ui.add(egui::Slider::new(&mut self.readout_hz, 0.5..=10.0).text(crate::lang::tr("Readout update rate (Hz)"))); crate::slider::reset(ui, &r, &mut self.readout_hz, 3.0); }
            ui.checkbox(&mut self.readout_hold, crate::lang::tr("Hold the numeric readouts"));
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Input mappings")).strong());
            ui.horizontal(|ui| {
                if crate::mapping::btn(ui, "Keys, MIDI and controllers...").on_hover_text(crate::lang::tr("Bind keys, MIDI notes and controllers, game controller and joystick buttons and axes to actions and sliders")).clicked() {
                    self.map.open = true;
                }
                ui.label(RichText::new(format!("{} bound", self.map.bindings.len())).small().weak());
            });
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Serial port (COM)")).strong()).on_hover_text(crate::lang::tr("Text commands for a controller on a serial port: an Arduino that turns a table, switches relays ... Each command is a line of text you define. \\n, \\r, \\t and \\xNN stand for control characters. Send one with its button here, from the command palette (Ctrl+K), by a key or MIDI binding (Keys, MIDI and controllers), or in a test sequence with the line: serial <text>. The turntable of the Speaker window uses the same port. The program only sends. Most boards restart when the port is opened, so the first command waits for the time set here."));
            {
                let mut c = crate::serial::cfg();
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("opt_ser_port").width(80.0).selected_text(if c.port.is_empty() { crate::lang::tr("off").to_string() } else { c.port.clone() }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut c.port, String::new(), crate::lang::tr("off"));
                        for p in crate::serial::ports() {
                            ui.selectable_value(&mut c.port, p.clone(), p);
                        }
                    });
                    egui::ComboBox::from_id_salt("opt_ser_baud").width(70.0).selected_text(c.baud.to_string()).show_ui(ui, |ui| {
                        for b in crate::serial::BAUDS {
                            ui.selectable_value(&mut c.baud, b, b.to_string());
                        }
                    });
                    egui::ComboBox::from_id_salt("opt_ser_eol").width(60.0).selected_text(crate::serial::EOL[c.eol].0).show_ui(ui, |ui| {
                        for (k, e) in crate::serial::EOL.iter().enumerate() {
                            ui.selectable_value(&mut c.eol, k, e.0);
                        }
                    })
                    .response
                    .on_hover_text(crate::lang::tr("What is added to the end of every command"));
                    ui.add(egui::DragValue::new(&mut c.boot_ms).range(0..=20000).speed(20).suffix(" ms")).on_hover_text(crate::lang::tr("Wait after opening the port, before the first command (a board that restarts needs about two seconds)"));
                    ui.label(RichText::new(crate::serial::status()).small().weak());
                });
                let mut del = None;
                for (k, (name, text)) in c.cmds.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(name).desired_width(110.0).hint_text(crate::lang::tr("name")));
                        ui.add(egui::TextEdit::singleline(text).desired_width(180.0).hint_text(crate::lang::tr("text to send")));
                        if ui.small_button(crate::lang::tr("Send")).clicked() {
                            crate::serial::send(text, &[]);
                        }
                        if ui.small_button("x").clicked() {
                            del = Some(k);
                        }
                    });
                }
                if let Some(k) = del {
                    c.cmds.remove(k);
                }
                if c.cmds.len() < 64 && ui.small_button(crate::lang::tr("+ command")).clicked() {
                    c.cmds.push((format!("Command {}", c.cmds.len() + 1), String::new()));
                }
                crate::serial::set_cfg(c);
            }
            ui.separator();
            ui.label(RichText::new(crate::lang::tr("Files")).strong());
            ui.checkbox(&mut self.import_at_start, crate::lang::tr("Import audio into the timeline at its start, not at the cursor")).on_hover_text(crate::lang::tr("Files added to the timeline (the Import button, drag and drop, the track menus) begin at the leftmost position, so several imports line up. Off: they are placed at the cursor."));
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
