//! Left settings bar: grouped, collapsible sections.

use crate::app::*;
use crate::audio;
use crate::colormap::Cmap;
use crate::dsp::Window;
use crate::shared::*;
use eframe::egui::{self, RichText, Ui};
use std::sync::atomic::Ordering::Relaxed;

fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, add);
}

fn pair_label(p: usize, chans: usize) -> String {
    let a = p * 2 + 1;
    if a < chans { format!("{a} + {}", a + 1) } else { format!("{a} (mono)") }
}

impl App {
    pub fn controls_ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            section(ui, "Input", true, |ui| self.input_section(ui));
            section(ui, "Analysis", true, |ui| self.analysis_section(ui));
            section(ui, "Spectrum traces", false, |ui| self.trace_section(ui));
            section(ui, "Spectrogram / waterfall", false, |ui| self.spectro_section(ui));
            section(ui, "3D view", false, |ui| self.view3d_section(ui));
            section(ui, "Distortion measurement", false, |ui| self.thd_section(ui));
            section(ui, "Voltage calibration", false, |ui| self.calibration_ui(ui));
            section(ui, "Performance", false, |ui| {
                ui.add(egui::Slider::new(&mut self.fps_cap, 15.0..=241.0).text("UI fps cap").custom_formatter(|v, _| if v > 240.0 { "max".into() } else { format!("{v:.0}") }));
                ui.add(egui::Slider::new(&mut self.readout_hz, 0.5..=10.0).text("Readout Hz"));
                ui.checkbox(&mut self.readout_hold, "Hold readouts");
            });
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
        let npairs = chans.div_ceil(2);
        if self.pair >= npairs {
            self.pair = 0;
            self.sh.pair.store(0, Relaxed);
        }
        ui.horizontal(|ui| {
            ui.label("Channels");
            egui::ComboBox::from_id_salt("pair").selected_text(pair_label(self.pair, chans)).show_ui(ui, |ui| {
                for p in 0..npairs {
                    if ui.selectable_value(&mut self.pair, p, pair_label(p, chans)).clicked() {
                        self.sh.pair.store(self.pair, Relaxed);
                    }
                }
            });
        })
        .response
        .on_hover_text("Which two input channels of the device are analysed as L and R.\nOnly matters for interfaces with more than 2 inputs.");
        ui.horizontal(|ui| {
            if ui.button("Restart").clicked() {
                restart = true;
            }
            if ui.add_enabled(!self.scanning, egui::Button::new(if self.scanning { "Scanning..." } else { "Rescan" })).clicked() {
                self.request_scan();
            }
        });
        ui.label(RichText::new("[Loopback] entries capture what a playback device is playing.").small().weak());
        if restart {
            self.start_selected();
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
