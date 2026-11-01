//! Left settings bar: grouped, collapsible sections.

use crate::app::*;
use crate::colormap::Cmap;
use crate::dsp::Window;
use crate::shared::*;
use eframe::egui::{self, RichText, Ui};
use std::sync::atomic::Ordering::Relaxed;

fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, add);
}

impl App {
    pub fn controls_ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            if self.graph_on {
                section(ui, "Input", true, |ui| {
                    ui.label(RichText::new("The routing graph is on: devices, channels, the monitor and the outputs are set in the Routing window.").small().weak());
                    let s = self.sh.status.lock().clone();
                    ui.label(RichText::new(s).small());
                });
            } else {
                section(ui, "Input", true, |ui| self.input_section(ui));
            }
            section(ui, "Analysis", true, |ui| self.analysis_section(ui));
            section(ui, "Spectrum traces", false, |ui| self.trace_section(ui));
            section(ui, "Spectrogram / waterfall", false, |ui| self.spectro_section(ui));
            section(ui, "3D view", false, |ui| self.view3d_section(ui));
            section(ui, "Distortion measurement", false, |ui| self.thd_section(ui));
            section(ui, "Voltage calibration", false, |ui| self.calibration_ui(ui));
            section(ui, "Microphone calibration", false, |ui| self.mic_section(ui));
        });
    }

    fn input_section(&mut self, ui: &mut Ui) {
        let mut restart = false;
        let cur = self.devices.get(self.sel_dev).map(|d| d.label.clone()).unwrap_or_else(|| "-".into());
        ui.label("Device");
        egui::ComboBox::from_id_salt("dev").width(ui.available_width() - 8.0).selected_text(cur).show_ui(ui, |ui| {
            for (i, d) in self.devices.iter().enumerate() {
                if ui.selectable_value(&mut self.sel_dev, i, &d.label).clicked() {
                    restart = true;
                }
            }
        });
        let chans = (self.sh.chans.load(Relaxed) as usize).max(1);
        if chans > 2 {
            self.channel_picker(ui, chans);
        }
        ui.horizontal(|ui| {
            if ui.button("Restart").clicked() {
                restart = true;
            }
            if ui.add_enabled(!self.scanning, egui::Button::new(if self.scanning { "Scanning..." } else { "Rescan" })).clicked() {
                self.request_scan();
            }
        });
        ui.label(RichText::new("[Loopback] entries capture what a playback device is playing.").small().weak());
        self.monitor_ui(ui);
        if restart {
            self.start_selected();
        }
    }

    /// Id of the output the monitor plays on (the default output when none is chosen).
    fn mon_output_id(&self) -> Option<String> {
        if !self.mon_dev.is_empty() {
            return Some(self.mon_dev.clone());
        }
        self.gen_devs.first().map(|d| d.id.clone())
    }

    /// True when the monitor would play into the device that is being captured.
    fn mon_feedback(&self) -> bool {
        match (self.devices.get(self.sel_dev), self.mon_output_id()) {
            (Some(i), Some(o)) => i.label.starts_with("[Loopback]") && i.id == o,
            _ => false,
        }
    }

    /// Start, stop or retarget the input monitor (called every frame).
    pub fn manage_monitor(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.mon_gain.store(10f32.powf(self.mon_gain_db / 20.0).to_bits(), Relaxed);
        let want = self.mon_on && !self.mon_feedback() && !self.graph_on;
        let dev = self.mon_output_id().unwrap_or_default();
        if let Some((_, d)) = &self.mon_handle {
            if !want || *d != dev {
                self.mon_handle = None;
            }
        }
        if want && self.mon_handle.is_none() {
            self.sh.mon_q.lock().buf.clear();
            let h = crate::tools::monitor::start(self.sh.mon_q.clone(), self.mon_gain.clone(), Some(dev.clone()).filter(|d| !d.is_empty()), self.mon_status.clone());
            self.mon_handle = Some((h, dev));
        }
        self.sh.mon_on.store(want, Relaxed);
        if !self.mon_on {
            self.mon_status.lock().clear();
        }
    }

    fn monitor_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.mon_on, "Monitor").on_hover_text("Play the analysed input (L and R) on an output device");
            ui.add_enabled(self.mon_on, egui::Slider::new(&mut self.mon_gain_db, -60.0..=6.0).suffix(" dB"));
        });
        let cur = self.gen_devs.iter().find(|d| d.id == self.mon_dev).map(|d| d.label.clone()).unwrap_or_else(|| self.gen_devs.first().map(|d| d.label.clone()).unwrap_or_else(|| "-".into()));
        egui::ComboBox::from_id_salt("mon_out").width(ui.available_width() - 8.0).selected_text(cur).show_ui(ui, |ui| {
            for (i, d) in self.gen_devs.iter().enumerate() {
                let id = if i == 0 { String::new() } else { d.id.clone() };
                ui.selectable_value(&mut self.mon_dev, id, &d.label);
            }
        });
        if self.mon_on {
            if self.mon_feedback() {
                ui.colored_label(egui::Color32::from_rgb(255, 130, 100), "This loopback input captures the monitor output: choose another output (feedback).");
            } else {
                let s = self.mon_status.lock().clone();
                if !s.is_empty() {
                    ui.label(RichText::new(s).small().weak());
                }
            }
        }
    }

    /// Pick any device channel as L and as R, with a level meter per channel (click = L, right-click = R).
    fn channel_picker(&mut self, ui: &mut Ui, chans: usize) {
        let n = chans.min(MAX_CH);
        self.ch_a = self.ch_a.min(n - 1);
        self.ch_b = self.ch_b.min(n - 1);
        let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.001, 0.1);
        for c in 0..n {
            let p = f32::from_bits(self.sh.ch_peak[c].swap(0, Relaxed));
            self.ch_lvl[c] = p.max(self.ch_lvl[c] * (-dt / 0.35f32).exp());
        }
        ui.horizontal(|ui| {
            for (name, which) in [("L", 0usize), ("R", 1)] {
                ui.label(RichText::new(name).color(CH_COLORS[which]));
                let cur = if which == 0 { self.ch_a } else { self.ch_b };
                let mut sel = cur;
                egui::ComboBox::from_id_salt(("chsel", which)).width(52.0).selected_text(format!("{}", cur + 1)).show_ui(ui, |ui| {
                    for c in 0..n {
                        ui.selectable_value(&mut sel, c, format!("{}", c + 1));
                    }
                });
                if which == 0 {
                    self.ch_a = sel;
                } else {
                    self.ch_b = sel;
                }
            }
        })
        .response
        .on_hover_text("Which input channels of the device are analysed as L and R.");
        let h = 46.0;
        let bw = ((ui.available_width() - 8.0) / n as f32).clamp(8.0, 26.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(bw * n as f32, h + 14.0), egui::Sense::click());
        let p = ui.painter_at(rect);
        for c in 0..n {
            let x0 = rect.min.x + c as f32 * bw;
            let bar = egui::Rect::from_min_size(egui::pos2(x0 + 2.0, rect.min.y), egui::vec2(bw - 4.0, h));
            p.rect_filled(bar, 1.0, ui.visuals().extreme_bg_color);
            let db = 20.0 * self.ch_lvl[c].max(1e-6).log10();
            let f = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let col = if db > -1.0 {
                egui::Color32::from_rgb(255, 70, 70)
            } else if db > -12.0 {
                egui::Color32::from_rgb(240, 200, 60)
            } else {
                egui::Color32::from_rgb(80, 200, 110)
            };
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(bar.min.x, bar.max.y - h * f), bar.max), 1.0, col);
            let sel = if c == self.ch_a {
                Some(CH_COLORS[0])
            } else if c == self.ch_b {
                Some(CH_COLORS[1])
            } else {
                None
            };
            if let Some(sc) = sel {
                p.rect_stroke(bar.expand(1.0), 1.0, egui::Stroke::new(1.5, sc), egui::StrokeKind::Outside);
            }
            let tc = if sel.is_some() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
            p.text(egui::pos2(x0 + bw * 0.5, rect.max.y - 1.0), egui::Align2::CENTER_BOTTOM, format!("{}", c + 1), egui::FontId::proportional(10.0), tc);
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            let c = (((pos.x - rect.min.x) / bw) as usize).min(n - 1);
            if resp.clicked() {
                self.ch_a = c;
            } else if resp.secondary_clicked() {
                self.ch_b = c;
            }
        }
        ui.label(RichText::new("Click a meter = L, right-click = R").small().weak());
        // the audio thread reads these every callback
        self.sh.ch_a.store(self.ch_a, Relaxed);
        self.sh.ch_b.store(self.ch_b, Relaxed);
    }

    /// Read a calibration file into slot 0 (0 degrees) or 1 (90 degrees; chosen from the file name when `slot` is `None`).
    pub fn load_mic_file(&mut self, path: &std::path::Path, slot: Option<usize>, announce: bool) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let slot = slot.unwrap_or_else(|| usize::from(name.to_lowercase().contains("90")));
        let parsed = std::fs::read(path).ok().and_then(|b| crate::cal::parse_mic_cal(&name, &String::from_utf8_lossy(&b)));
        match parsed {
            Some(c) => {
                if announce {
                    self.note = format!("Loaded {} ({} points)", c.name, c.f.len());
                    self.mic_on = true;
                    self.mic_angle = slot;
                }
                self.mic[slot] = Some(c);
                self.mic_paths[slot] = path.to_string_lossy().to_string();
            }
            None if announce => self.note = format!("{name}: no frequency / dB pairs found"),
            None => {}
        }
    }

    pub fn push_mic(&mut self) {
        let cur = self.mic[self.mic_angle].as_ref().or(self.mic[0].as_ref()).or(self.mic[1].as_ref());
        let mut m = self.sh.mic.lock();
        m.rev = m.rev.wrapping_add(1);
        m.mask = self.mic_mask;
        match cur {
            Some(c) if self.mic_on => {
                m.f = c.f.clone();
                m.db = c.db.clone();
            }
            _ => {
                m.f.clear();
                m.db.clear();
            }
        }
    }

    pub fn poll_mic_dialog(&mut self) {
        let Some(d) = &self.mic_dialog else { return };
        if let Some(res) = d.take() {
            self.mic_dialog = None;
            if let Some(path) = res {
                self.load_mic_file(&path, None, true);
                self.push_mic();
            }
        }
    }

    fn mic_section(&mut self, ui: &mut Ui) {
        let mut changed = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(self.mic_dialog.is_none(), egui::Button::new("Load file...")).on_hover_text("Calibration file of the microphone (frequency and dB pairs). A name containing 90 is stored as the 90° file.").clicked() {
                self.mic_dialog = Some(crate::tools::dialogs::open("Load microphone calibration", "Calibration", &["txt", "cal", "csv", "frd"]));
            }
            if ui.button("Clear").clicked() {
                self.mic = [None, None];
                self.mic_paths = Default::default();
                changed = true;
            }
        });
        for (i, deg) in ["0", "90"].iter().enumerate() {
            if let Some(c) = &self.mic[i] {
                ui.label(RichText::new(format!("{deg}°: {} ({} points, {:.0} Hz - {:.0} Hz)", c.name, c.f.len(), c.f[0], c.f[c.f.len() - 1])).small());
            }
        }
        let cur = self.mic[self.mic_angle].as_ref().or(self.mic[0].as_ref()).or(self.mic[1].as_ref());
        let Some(c) = cur else {
            ui.label(RichText::new("No file loaded. The spectrum then shows what the input hears, including the microphone's own response.").small().weak());
            if changed {
                self.push_mic();
            }
            return;
        };
        if let Some(s) = c.sens {
            ui.label(RichText::new(format!("File header: Sens Factor {s:+.2} dB{}", c.again.map(|g| format!(", analog gain {g:.0} dB")).unwrap_or_default())).small().weak());
        }
        changed |= ui.checkbox(&mut self.mic_on, "Correct the spectrum").changed();
        if self.mic[0].is_some() && self.mic[1].is_some() {
            ui.horizontal(|ui| {
                ui.label("Angle");
                changed |= ui.selectable_value(&mut self.mic_angle, 0, "0° (on axis)").changed();
                changed |= ui.selectable_value(&mut self.mic_angle, 1, "90°").changed();
            });
        }
        ui.horizontal(|ui| {
            ui.label("Apply to");
            changed |= ui.checkbox(&mut self.mic_mask[0], RichText::new("L").color(CH_COLORS[0])).changed();
            changed |= ui.checkbox(&mut self.mic_mask[1], RichText::new("R").color(CH_COLORS[1])).changed();
        });
        ui.label(RichText::new("Corrects spectrum, spectrogram, 3D and snapshots. Measurements and the SPL meter use the raw signal.").small().weak());
        if changed {
            self.push_mic();
        }
    }

    fn analysis_section(&mut self, ui: &mut Ui) {
        let srv = self.sr();
        egui::Grid::new("an_grid").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
            ui.label("FFT size");
            egui::ComboBox::from_id_salt("fft").selected_text(format!("{}", self.p.fft_size)).show_ui(ui, |ui| {
                for s in [1024usize, 2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144] {
                    ui.selectable_value(&mut self.p.fft_size, s, format!("{s}  ({:.2} Hz/bin){}", srv / s as f32, if s >= 131072 { "  heavy" } else { "" }));
                }
            });
            ui.end_row();
            ui.label("Window");
            egui::ComboBox::from_id_salt("win").selected_text(self.p.window.name()).show_ui(ui, |ui| {
                for w in Window::ALL {
                    ui.selectable_value(&mut self.p.window, w, w.name());
                }
            });
            ui.end_row();
            ui.label("FFT rate");
            ui.add(egui::Slider::new(&mut self.p.update_hz, 10.0..=240.0).suffix(" Hz"));
            ui.end_row();
            ui.label("Average");
            ui.add(egui::Slider::new(&mut self.p.avg_count, 1.0..=512.0).logarithmic(true));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let mut log = self.p.log_freq;
            if ui.checkbox(&mut log, "Log frequency").changed() {
                self.p.log_freq = log;
                self.f_lo = 0.0;
                self.f_hi = 1.0;
            }
            if ui.button("Reset avg/max/min").clicked() {
                self.p.reset_gen = self.p.reset_gen.wrapping_add(1);
            }
        });
        if self.an.n > 0 {
            ui.label(RichText::new(format!("bin {:.3} Hz  ENBW {:.2} bins  CG {:.3}", self.an.sr / self.an.n as f32, self.an.enbw_bins, self.an.coherent_gain)).small().weak());
            ui.label(
                RichText::new(format!(
                    "Scalloping loss up to {:.2} dB: a tone between two bins reads that much low on the spectrum. The Measurements level is window-independent.",
                    self.an.scallop_db
                ))
                .small()
                .weak(),
            );
        }
    }

    fn trace_section(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_live, "Live");
            ui.checkbox(&mut self.show_avg, "Avg");
            ui.checkbox(&mut self.show_max, "Max");
            ui.checkbox(&mut self.show_min, "Min");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_ch[0], RichText::new("L").color(CH_COLORS[0]));
            ui.checkbox(&mut self.show_ch[1], RichText::new("R").color(CH_COLORS[1]));
            ui.checkbox(&mut self.fill, "Fill");
        });
        ui.horizontal(|ui| {
            ui.label("Smoothing");
            egui::ComboBox::from_id_salt("spec_smooth").width(80.0).selected_text(SMOOTH_OCT[self.smooth_oct].1).show_ui(ui, |ui| {
                for (i, s) in SMOOTH_OCT.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth_oct, i, s.1);
                }
            });
        })
        .response
        .on_hover_text("Fractional-octave smoothing of the displayed spectrum (power average). Cursor readouts stay unsmoothed.");
        ui.add(egui::Slider::new(&mut self.line_w, 0.5..=3.0).text("Line width"));
    }

    fn spectro_section(&mut self, ui: &mut Ui) {
        egui::Grid::new("sg_grid").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
            ui.label("Colormap");
            egui::ComboBox::from_id_salt("cmap").selected_text(self.cmap.name()).show_ui(ui, |ui| {
                for c in Cmap::ALL {
                    if ui.selectable_value(&mut self.cmap, c, c.name()).clicked() {
                        self.lut = c.lut();
                        self.rebuild_dyn_lut();
                    }
                }
            });
            ui.end_row();
            ui.label("Floor");
            let a = ui.add(egui::Slider::new(&mut self.col_lo, -160.0..=-20.0).suffix(" dB")).changed();
            ui.end_row();
            ui.label("Ceiling");
            let b = ui.add(egui::Slider::new(&mut self.col_hi, -80.0..=10.0).suffix(" dB")).changed();
            ui.end_row();
            if a || b {
                self.col_hi = self.col_hi.max(self.col_lo + 10.0);
                self.rebuild_dyn_lut();
            }
            ui.label("Columns/s");
            ui.add(egui::Slider::new(&mut self.p.cols_per_sec, 2.0..=240.0).logarithmic(true));
            ui.end_row();
            ui.label("Source");
            egui::ComboBox::from_id_salt("sgsrc").selected_text(["Left", "Right", "Max(L,R)"][self.p.spectro_src as usize]).show_ui(ui, |ui| {
                for (i, n) in ["Left", "Right", "Max(L,R)"].iter().enumerate() {
                    ui.selectable_value(&mut self.p.spectro_src, i as u8, *n);
                }
            });
            ui.end_row();
        });
    }

    fn view3d_section(&mut self, ui: &mut Ui) {
        egui::Grid::new("d3_grid").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
            ui.label("Depth rows");
            ui.add(egui::Slider::new(&mut self.d3.rows, 32..=crate::view3d::D3_MAX));
            ui.end_row();
            ui.label("Freq detail");
            ui.add(egui::Slider::new(&mut self.d3.cols, 64..=1024));
            ui.end_row();
            ui.label("Rows/s");
            ui.add(egui::Slider::new(&mut self.d3.rate, 2.0..=60.0));
            ui.end_row();
            ui.label("Height");
            ui.add(egui::Slider::new(&mut self.d3.hscale, 0.2..=2.0));
            ui.end_row();
        });
        ui.checkbox(&mut self.d3.wire, "Wire overlay");
    }

    fn thd_section(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new(format!("Own f64 FFT ({} pts), 7-term Blackman-Harris (-180 dB). Independent of the display window.", self.an.meas_n))
                .small()
                .weak(),
        );
        ui.horizontal(|ui| {
            ui.label("Band");
            ui.add(egui::DragValue::new(&mut self.p.bw_lo).range(1.0..=2000.0).suffix(" Hz"));
            ui.label("-");
            ui.add(egui::DragValue::new(&mut self.p.bw_hi).range(100.0..=96000.0).suffix(" Hz"));
        });
        ui.add(egui::Slider::new(&mut self.p.harmonics, 2..=MAXH).text("Harmonics"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.p.manual_fund, "Manual f0");
            ui.add_enabled(self.p.manual_fund, egui::DragValue::new(&mut self.p.fund_hz).range(10.0..=96000.0).suffix(" Hz"));
        });
        ui.horizontal(|ui| {
            ui.label("Channel");
            ui.selectable_value(&mut self.meas_ch, 0, "L");
            ui.selectable_value(&mut self.meas_ch, 1, "R");
        });
    }
}
