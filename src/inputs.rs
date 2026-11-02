//! Input mappings in the application: the keyboard feeds the mapper, the actions it fires are carried out here, and the editor
//! window for the bindings. The MIDI and controller sources are in `mapping`.

use crate::app::App;
use crate::mapping::{self, Src};
use crate::windows::{win_short, WIN_COUNT};
use eframe::egui::{self, Color32, RichText};

impl App {
    /// Once per frame: keys into the mapper, fired actions carried out, the editor window.
    pub fn inputs_frame(&mut self, ctx: &egui::Context) {
        mapping::start_sources();
        // the keyboard: key presses with their modifiers (while typing into a field only when a binding is being learned)
        if self.map.learning.is_some() || !ctx.egui_wants_keyboard_input() {
            let events = ctx.input(|i| i.events.clone());
            for e in events {
                if let egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } = e {
                    mapping::push_event(Src::Key { key: key.name().to_string(), ctrl: modifiers.command, shift: modifiers.shift, alt: modifiers.alt }, 1.0);
                }
            }
        }
        let defs = std::mem::take(&mut self.map_defs);
        let fired = self.map.poll(&defs);
        self.map_defs = defs;
        for f in fired {
            self.run_action(ctx, &f.action, f.value);
        }
        if self.map.dirty {
            self.map.save();
        }
        // while the editor learns, the window stays current even when nothing else moves
        if self.map.open {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.mappings_ui(ctx);
    }

    fn tool_command(&mut self, short: &str, cmd: &str) {
        if let Some(k) = (0..crate::tools::TOOL_COUNT).find(|&k| crate::tools::INFO[k].short == short) {
            self.tools[k].command(cmd);
        }
    }

    /// Carry out an action that a binding fired; `v` is the value of a slider's setting (a press gives 1).
    pub fn run_action(&mut self, ctx: &egui::Context, id: &str, v: f32) {
        match id {
            "freeze" => self.p.paused = !self.p.paused,
            "reset_minmax" => self.p.reset_gen = self.p.reset_gen.wrapping_add(1),
            "png" => self.request_png(ctx),
            "monitor" => self.mon_on = !self.mon_on,
            "generator" => {
                self.tg.on = !self.tg.on;
                if self.tg.on && self.gen_handle.is_none() {
                    self.start_gen();
                }
            }
            "mon_gain" => self.mon_gain_db = v.clamp(-60.0, 6.0),
            "gen_level" => self.tg.level_db = [v.clamp(-90.0, 0.0); 2],
            "gen_freq" => self.tg.freq = v.clamp(1.0, 96000.0),
            "tile" => self.need_tile = true,
            "assistant" => self.assist.open = !self.assist.open,
            "daw_play" => self.tool_command("Timeline", "play"),
            "daw_record" => self.tool_command("Timeline", "record"),
            "daw_loop" => self.tool_command("Timeline", "loop"),
            "daw_marker" => self.tool_command("Timeline", "marker"),
            "daw_split" => self.tool_command("Timeline", "split"),
            "daw_undo" => self.tool_command("Timeline", "undo"),
            "daw_redo" => self.tool_command("Timeline", "redo"),
            "daw_master" => self.tool_command("Timeline", &format!("master\t{v}")),
            "graph_probe" => self.tool_command("Routing", "probe"),
            "graph_ab" => self.tool_command("Routing", "ab"),
            "graph_undo" => self.tool_command("Routing", "undo"),
            "graph_redo" => self.tool_command("Routing", "redo"),
            x if x.starts_with("layout_") => {
                if let Some(n) = x[7..].parse::<usize>().ok().filter(|n| *n >= 1) {
                    self.apply_preset_index(n - 1);
                }
            }
            x if x.starts_with("win:") => {
                if let Some(i) = (0..WIN_COUNT).find(|&i| win_short(i) == &x[4..]) {
                    self.wins[i].open = !self.wins[i].open;
                }
            }
            _ => {}
        }
    }

    /// The window in which the bindings are made.
    pub fn mappings_ui(&mut self, ctx: &egui::Context) {
        if !self.map.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Input mappings").order(egui::Order::Foreground).open(&mut open).default_width(560.0).default_height(520.0).resizable(true).show(ctx, |ui| {
            let devs = mapping::devices();
            ui.label(RichText::new(if devs.is_empty() { "No MIDI input or controller found (they are looked for every few seconds).".to_string() } else { devs.join("   |   ") }).small().weak());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Last input:").small().weak());
                ui.label(RichText::new(if self.map.last.is_empty() { "-" } else { &self.map.last }).small().monospace());
            });
            ui.label(RichText::new("Press Learn, then press the key or move the control. A key, a MIDI note or a button starts an action; a MIDI controller, a fader, knob or joystick axis drives a setting between the two values.").small().weak());
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let mut group = "";
                for i in 0..self.map_defs.len() {
                    let (id, label, g, slider) = {
                        let d = &self.map_defs[i];
                        (d.id.clone(), d.label.clone(), d.group, d.slider)
                    };
                    if g != group {
                        group = g;
                        ui.add_space(4.0);
                        ui.label(RichText::new(g).strong().color(Color32::from_rgb(150, 170, 200)));
                    }
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(230.0, 18.0), egui::Sense::hover());
                        ui.painter().with_clip_rect(r).text(r.left_center(), egui::Align2::LEFT_CENTER, &label, egui::FontId::proportional(11.0), Color32::from_gray(200));
                        let waiting = self.map.learning.as_deref() == Some(id.as_str());
                        let bound = self.map.binding(&id).map(|b| b.src.label());
                        if waiting {
                            ui.label(RichText::new(if slider.is_some() { "move a knob, fader or axis ..." } else { "press a key or a button ..." }).small().color(Color32::from_rgb(255, 200, 90)));
                            if ui.small_button("Cancel").clicked() {
                                self.map.learning = None;
                            }
                        } else {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(190.0, 18.0), egui::Sense::hover());
                            ui.painter().with_clip_rect(r).text(r.left_center(), egui::Align2::LEFT_CENTER, bound.clone().unwrap_or_else(|| "-".into()), egui::FontId::monospace(10.5), if bound.is_some() { Color32::from_gray(220) } else { Color32::from_gray(110) });
                            if ui.small_button("Learn").clicked() {
                                self.map.learning = Some(id.clone());
                            }
                            if bound.is_some() && ui.small_button("Clear").clicked() {
                                self.map.clear(&id);
                            }
                        }
                    });
                    // a slider's range, and whether the control works the other way round
                    if let (Some((lo, hi, _, unit)), Some(bi)) = (slider, self.map.bindings.iter().position(|b| b.action == id)) {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            let b = &mut self.map.bindings[bi];
                            let mut ch = false;
                            ui.label(RichText::new("from").small().weak());
                            ch |= ui.add(egui::DragValue::new(&mut b.min).range(lo..=hi).speed((hi - lo) * 0.002).suffix(unit)).changed();
                            ui.label(RichText::new("to").small().weak());
                            ch |= ui.add(egui::DragValue::new(&mut b.max).range(lo..=hi).speed((hi - lo) * 0.002).suffix(unit)).changed();
                            ch |= ui.checkbox(&mut b.invert, "reversed").changed();
                            if ch {
                                self.map.dirty = true;
                            }
                        });
                    }
                }
                ui.add_space(6.0);
                ui.label(RichText::new("The keys the windows already use (space, R, S, M, K, Ctrl+1 to 9 ...) keep working: avoid binding them to something else.").small().weak());
            });
        });
        if !open {
            self.map.open = false;
            self.map.learning = None;
        }
    }
}
