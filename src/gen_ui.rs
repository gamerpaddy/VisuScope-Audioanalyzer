//! Signal generator window.

use crate::app::*;
use crate::tonegen::{self, Dither, NoiseKind, Wave};
use eframe::egui::{self, vec2, Color32, RichText, Ui};

fn sub(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, add);
}

impl App {
    pub fn start_gen(&mut self) {
        self.gen_handle = None;
        if let Some(d) = self.gen_devs.get(self.gen_sel) {
            *self.gen_shared.lock() = self.tg.clone();
            self.gen_handle = Some(tonegen::start(self.gen_shared.clone(), d.id.clone(), self.gen_status.clone()));
        } else {
            *self.gen_status.lock() = "No output devices".into();
        }
    }

    pub fn generator_ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            // output on/off + device
            ui.horizontal(|ui| {
                let on = self.tg.on;
                let btn = egui::Button::new(RichText::new(if on { "OUTPUT ON" } else { "OUTPUT OFF" }).strong())
                    .fill(if on { Color32::from_rgb(30, 120, 60) } else { Color32::from_gray(55) })
                    .min_size(vec2(120.0, 26.0));
                if ui.add(btn).clicked() {
                    self.tg.on = !self.tg.on;
                    if self.tg.on && self.gen_handle.is_none() {
                        self.start_gen();
                    }
                }
                ui.label(RichText::new(self.gen_status.lock().clone()).small().weak());
            });
            let cur = self.gen_devs.get(self.gen_sel).map(|d| d.label.clone()).unwrap_or_else(|| "-".into());
            let mut restart = false;
            ui.horizontal(|ui| {
                ui.label("Output");
                egui::ComboBox::from_id_salt("gendev").width(ui.available_width() - 70.0).selected_text(cur).show_ui(ui, |ui| {
                    for (i, d) in self.gen_devs.iter().enumerate() {
                        if ui.selectable_value(&mut self.gen_sel, i, &d.label).clicked() {
                            restart = true;
                        }
                    }
                });
                if ui.add_enabled(!self.scanning, egui::Button::new(if self.scanning { "Scanning..." } else { "Rescan" })).clicked() {
                    self.request_scan();
                }
            });
            if restart && (self.tg.on || self.gen_handle.is_some()) {
                self.start_gen();
            }
            ui.label(RichText::new("Plays to the chosen device. To measure it, pick its [Loopback] entry as the input. Keep levels low near speakers or a live mic.").small().weak());

            // levels
            sub(ui, "Level", true, |ui| {
                let g = &mut self.tg;
                egui::Grid::new("gen_lvl").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Left");
                    let l = ui.add(egui::Slider::new(&mut g.level_db[0], -120.0..=0.0).suffix(" dBFS")).changed();
                    ui.checkbox(&mut g.mute[0], "mute");
                    ui.end_row();
                    ui.label("Right");
                    let r = ui.add_enabled(!self.gen_link, egui::Slider::new(&mut g.level_db[1], -120.0..=0.0).suffix(" dBFS")).changed();
                    ui.checkbox(&mut g.mute[1], "mute");
                    ui.end_row();
                    let _ = r;
                    if l && self.gen_link {
                        g.level_db[1] = g.level_db[0];
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.gen_link, "Link L/R");
                    ui.label(RichText::new("peak for tones, RMS for noise / multitone").small().weak());
                });
                if self.gen_link {
                    self.tg.level_db[1] = self.tg.level_db[0];
                }
            });

            // signal
            sub(ui, "Signal", true, |ui| {
                let g = &mut self.tg;
                egui::ComboBox::from_id_salt("genwave").width(ui.available_width() - 8.0).selected_text(g.wave.name()).show_ui(ui, |ui| {
                    for w in Wave::ALL {
                        ui.selectable_value(&mut g.wave, w, w.name());
                    }
                });
                match g.wave {
                    Wave::Sine | Wave::Square | Wave::Triangle | Wave::Saw | Wave::Pulse | Wave::Impulse => {
                        ui.add(egui::Slider::new(&mut g.freq, 1.0..=96000.0).logarithmic(true).suffix(" Hz").text("Frequency"));
                        ui.horizontal(|ui| {
                            for f in [50.0, 100.0, 440.0, 1000.0, 5000.0, 10000.0, 19000.0] {
                                if ui.small_button(fmt_hz_short(f)).clicked() {
                                    g.freq = f;
                                }
                            }
                        });
                        if g.wave == Wave::Pulse {
                            ui.add(egui::Slider::new(&mut g.duty, 0.01..=0.99).text("Duty"));
                        }
                    }
                    Wave::SweepLog | Wave::SweepLin => {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut g.sweep_f0).range(1.0..=96000.0).suffix(" Hz"));
                            ui.label("to");
                            ui.add(egui::DragValue::new(&mut g.sweep_f1).range(2.0..=96000.0).suffix(" Hz"));
                        });
                        ui.add(egui::Slider::new(&mut g.sweep_time, 0.2..=60.0).logarithmic(true).suffix(" s").text("Sweep time"));
                    }
                    Wave::Multitone => {
                        ui.add(egui::Slider::new(&mut g.mt_count, 2..=32).text("Tones"));
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut g.mt_f0).range(1.0..=96000.0).suffix(" Hz"));
                            ui.label("to");
                            ui.add(egui::DragValue::new(&mut g.mt_f1).range(2.0..=96000.0).suffix(" Hz"));
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut g.mt_log, "Log spacing");
                            ui.checkbox(&mut g.mt_schroeder, "Schroeder phases (low crest)");
                        });
                    }
                    Wave::Smpte | Wave::Ccif => {
                        ui.label(RichText::new("Fixed twin-tone IMD test signal; level is the peak of the sum.").small().weak());
                    }
                    Wave::Noise => {
                        egui::ComboBox::from_id_salt("gennoise").width(ui.available_width() - 8.0).selected_text(g.noise.name()).show_ui(ui, |ui| {
                            for k in NoiseKind::ALL {
                                ui.selectable_value(&mut g.noise, k, k.name());
                            }
                        });
                        ui.checkbox(&mut g.noise_corr, "Correlated L/R (mono noise)");
                    }
                }
            });

            sub(ui, "Harmonic distortion", false, |ui| {
                let g = &mut self.tg;
                ui.checkbox(&mut g.harm_on, "Add harmonics (tones / sweeps)");
                ui.add_enabled_ui(g.harm_on, |ui| {
                    for h in 0..5 {
                        ui.add(egui::Slider::new(&mut g.harm_db[h], -140.0..=-20.0).suffix(" dBc").text(format!("H{}", h + 2)));
                    }
                });
            });

            sub(ui, "Phase / polarity", false, |ui| {
                let g = &mut self.tg;
                ui.add(egui::Slider::new(&mut g.phase_deg, -180.0..=180.0).suffix(" deg").text("R vs L phase"));
                ui.horizontal(|ui| {
                    if ui.small_button("0").clicked() {
                        g.phase_deg = 0.0;
                    }
                    if ui.small_button("90").clicked() {
                        g.phase_deg = 90.0;
                    }
                    if ui.small_button("180").clicked() {
                        g.phase_deg = 180.0;
                    }
                    ui.checkbox(&mut g.invert_r, "Invert R");
                });
                ui.label(RichText::new("Shifts the right channel's tone phase (watch the goniometer and correlation).").small().weak());
            });

            sub(ui, "Jitter", false, |ui| {
                let g = &mut self.tg;
                ui.checkbox(&mut g.jitter_on, "Sample-clock jitter (tones, sweeps, multitone)");
                ui.add_enabled_ui(g.jitter_on, |ui| {
                    ui.add(egui::Slider::new(&mut g.jit_rms_ns, 0.0..=1000.0).logarithmic(true).suffix(" ns").text("Random RMS"));
                    ui.add(egui::Slider::new(&mut g.jit_sine_ns, 0.0..=1000.0).suffix(" ns").text("Sine amplitude"));
                    ui.add(egui::Slider::new(&mut g.jit_sine_hz, 1.0..=20000.0).logarithmic(true).suffix(" Hz").text("Sine rate"));
                });
            });

            sub(ui, "Word length and dither", false, |ui| {
                let g = &mut self.tg;
                ui.checkbox(&mut g.quant_on, "Quantise output");
                ui.add_enabled_ui(g.quant_on, |ui| {
                    ui.add(egui::Slider::new(&mut g.bits, 4..=24).suffix(" bit").text("Word length"));
                    egui::ComboBox::from_id_salt("gendither").selected_text(g.dither.name()).show_ui(ui, |ui| {
                        for d in Dither::ALL {
                            ui.selectable_value(&mut g.dither, d, d.name());
                        }
                    });
                    ui.add(egui::Slider::new(&mut g.dither_lsb, 0.0..=2.0).suffix(" LSB").text("Dither amount"));
                });
                ui.label(RichText::new("Quantising a low-level tone shows how dither trades distortion for noise on the THD+N / ENOB readout.").small().weak());
            });
        });
    }
}
