//! EQ window: fit parametric filters to the last sweep (or a stored trace) against a flat or tilted
//! target, preview the result and export Equalizer APO text, biquads or a FIR as WAV.

use super::plot::{canvas, inset, Plot};
use super::traces::smooth_db;
use super::*;
use crate::eq::{self, Filt};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const SMOOTH: [(f32, &str); 4] = [(12.0, "1/12 oct"), (6.0, "1/6 oct"), (3.0, "1/3 oct"), (1.0, "1 oct")];
const TAPS: [usize; 5] = [1024, 2048, 4096, 8192, 16384];
const RATES: [u32; 3] = [44100, 48000, 96000];
const GRID: usize = 400;

pub struct EqTool {
    /// 0 = last sweep, otherwise the name of a stored trace
    src: Option<String>,
    smooth: usize,
    band: (f32, f32),
    n_filters: usize,
    max_boost: f32,
    max_cut: f32,
    qmax: f32,
    tilt: f32,
    taps: usize,
    rate: usize,
    y_span: f32,
    // result
    f: Vec<f32>,
    measured: Vec<f32>,
    target: Vec<f32>,
    filts: Vec<Filt>,
    predicted: Vec<f32>,
    eqc: Vec<f32>,
    key: u64,
    status: String,
    saves: [Option<dialogs::Pending>; 5],
}

fn hash(parts: &[u64]) -> u64 {
    parts.iter().fold(0xcbf29ce484222325u64, |h, &p| (h ^ p).wrapping_mul(0x100000001b3))
}

impl EqTool {
    pub fn new() -> Self {
        Self {
            src: None,
            smooth: 1,
            band: (30.0, 500.0),
            n_filters: 8,
            max_boost: 3.0,
            max_cut: 12.0,
            qmax: 8.0,
            tilt: 0.0,
            taps: 3,
            rate: 1,
            y_span: 20.0,
            f: Vec::new(),
            measured: Vec::new(),
            target: Vec::new(),
            filts: Vec::new(),
            predicted: Vec::new(),
            eqc: Vec::new(),
            key: 0,
            status: String::new(),
            saves: Default::default(),
        }
    }

    fn design_sr(&self) -> f64 {
        RATES[self.rate] as f64
    }

    /// Measured magnitude on the log grid from the chosen source, smoothed.
    fn source_curve(&self, cx: &Ctx, f: &[f32]) -> Option<Vec<f32>> {
        let ppo = GRID as f32 / (f[f.len() - 1] / f[0]).log2();
        let raw: Vec<f32> = match &self.src {
            None => {
                let r = cx.sweep.result.as_ref()?;
                let h = &r.h;
                f.iter()
                    .map(|&x| {
                        let b = x / h.df;
                        let k = b.floor() as usize;
                        if k + 1 >= h.mag_db.len() {
                            return f32::NAN;
                        }
                        let t = b - k as f32;
                        h.mag_db[k] * (1.0 - t) + h.mag_db[k + 1] * t
                    })
                    .collect()
            }
            Some(name) => {
                let m = cx.lib.items.iter().find(|m| &m.name == name)?;
                f.iter().map(|&x| m.at(x)).collect()
            }
        };
        Some(smooth_db(&raw, ppo, SMOOTH[self.smooth].0))
    }

    fn design(&mut self, cx: &Ctx) {
        let sr = self.design_sr();
        let top = 20000f32.min(sr as f32 * 0.45);
        let f: Vec<f32> = (0..GRID).map(|i| 20.0 * (top / 20.0).powf(i as f32 / (GRID - 1) as f32)).collect();
        let Some(measured) = self.source_curve(cx, &f) else {
            self.f.clear();
            self.status = "no measurement: run a sweep in the Response window or store a trace".into();
            return;
        };
        let tilt = |x: f32| self.tilt * (x / 1000.0).log2();
        let inband: Vec<usize> = (0..GRID).filter(|&i| f[i] >= self.band.0 && f[i] <= self.band.1 && measured[i].is_finite()).collect();
        if inband.len() < 5 {
            self.f.clear();
            self.status = "the frequency band holds no data".into();
            return;
        }
        let level = inband.iter().map(|&i| measured[i] - tilt(f[i])).sum::<f32>() / inband.len() as f32;
        let target: Vec<f32> = f.iter().map(|&x| level + tilt(x)).collect();
        let desired: Vec<f32> = (0..GRID).map(|i| target[i] - measured[i]).collect();
        let filts = eq::fit(&f, &desired, self.n_filters, self.max_boost, self.max_cut, self.qmax, self.band, sr as f32);
        self.eqc = f.iter().map(|&x| eq::response_db(&filts, sr, x as f64) as f32).collect();
        self.predicted = (0..GRID).map(|i| measured[i] + self.eqc[i] - level).collect();
        self.measured = measured.iter().map(|m| m - level).collect();
        self.target = target.iter().map(|t| t - level).collect();
        self.filts = filts;
        self.f = f;
        self.status = String::new();
    }

    fn inputs_key(&self, cx: &Ctx) -> u64 {
        let src = match &self.src {
            None => cx.sweep.result.as_ref().map(|r| r.id).unwrap_or(0),
            Some(n) => cx.lib.items.iter().position(|m| &m.name == n).map(|i| 1 + i as u64 * 1000 + cx.lib.items[i].f.len() as u64).unwrap_or(0) + 1_000_000,
        };
        hash(&[src, self.smooth as u64, self.band.0.to_bits() as u64, self.band.1.to_bits() as u64, self.n_filters as u64, self.max_boost.to_bits() as u64, self.max_cut.to_bits() as u64, self.qmax.to_bits() as u64, self.tilt.to_bits() as u64, self.rate as u64])
    }
}

fn take(p: &mut Option<dialogs::Pending>) -> Option<std::path::PathBuf> {
    let d = p.as_ref()?;
    let res = d.take()?;
    *p = None;
    res
}

impl Tool for EqTool {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        for (kind, path) in (0..5).map(|k| (k, take(&mut self.saves[k]))).collect::<Vec<_>>() {
            let Some(path) = path else { continue };
            let sr = self.design_sr();
            let taps = TAPS[self.taps];
            let res = match kind {
                0 => std::fs::write(&path, eq::apo_text(&self.filts, sr)),
                1 => std::fs::write(&path, eq::biquad_csv(&self.filts, sr)),
                _ => {
                    let h = if kind == 2 { eq::fir_linear(&self.filts, sr, taps) } else { eq::fir_minphase(&self.filts, sr, taps) };
                    (|| {
                        let mut w = wavio::WavWriter::create(&path, wavio::Format::Float32, 1, sr as u32)?;
                        w.write(&h, &h)?;
                        w.finish()
                    })()
                }
            };
            cx.req.note = Some(match res {
                Ok(_) => format!("Saved {}", path.display()),
                Err(e) => format!("save failed: {e}"),
            });
        }
        if !visible {
            return;
        }
        let k = self.inputs_key(cx);
        if k != self.key {
            self.key = k;
            self.design(cx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Measurement").small().weak());
            let cur = self.src.clone().unwrap_or_else(|| "Last sweep".into());
            egui::ComboBox::from_id_salt("eq_src").width(130.0).selected_text(cur).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.src, None, "Last sweep");
                for m in &cx.lib.items {
                    ui.selectable_value(&mut self.src, Some(m.name.clone()), &m.name);
                }
            });
            ui.label(RichText::new("Smooth").small().weak());
            egui::ComboBox::from_id_salt("eq_sm").width(78.0).selected_text(SMOOTH[self.smooth].1).show_ui(ui, |ui| {
                for (i, s) in SMOOTH.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth, i, s.1);
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Correct").small().weak());
            ui.add(egui::DragValue::new(&mut self.band.0).range(20.0..=2000.0).speed(1.0).suffix(" Hz"));
            ui.label("to");
            ui.add(egui::DragValue::new(&mut self.band.1).range(100.0..=20000.0).speed(5.0).suffix(" Hz"));
            self.band.1 = self.band.1.max(self.band.0 * 1.5);
            ui.add(egui::DragValue::new(&mut self.n_filters).range(1..=20).suffix(" filters"));
            ui.add(egui::DragValue::new(&mut self.max_boost).range(0.0..=12.0).speed(0.1).prefix("boost +").suffix(" dB")).on_hover_text("Boosting a room null wastes headroom and rarely works: keep this small");
            ui.add(egui::DragValue::new(&mut self.max_cut).range(0.0..=24.0).speed(0.1).prefix("cut -").suffix(" dB"));
            ui.add(egui::DragValue::new(&mut self.qmax).range(1.0..=20.0).speed(0.1).prefix("Q max "));
            ui.add(egui::DragValue::new(&mut self.tilt).range(-6.0..=6.0).speed(0.05).prefix("tilt ").suffix(" dB/oct")).on_hover_text("Target slope around 1 kHz: negative rolls the highs off, as in a house curve");
        });
        let Some(&fmax) = self.f.last() else {
            ui.add_space(8.0);
            ui.label(RichText::new(if self.status.is_empty() { "designing..." } else { &self.status }).weak());
            return;
        };

        let (resp, painter, rect) = canvas(ui, 120.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let plot = Plot::new(pr, self.f[0], fmax, -self.y_span, self.y_span, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        let (b0, b1) = (plot.tx(self.band.0), plot.tx(self.band.1));
        painter.rect_filled(egui::Rect::from_min_max(Pos2::new(b0, pr.top()), Pos2::new(b1, pr.bottom())), 0.0, Color32::from_white_alpha(10));
        let curves: [(&str, Color32, &Vec<f32>, f32); 4] = [
            ("measured", Color32::from_rgb(90, 200, 255), &self.measured, 1.5),
            ("target", Color32::from_gray(150), &self.target, 1.0),
            ("corrected", Color32::from_rgb(120, 230, 150), &self.predicted, 1.8),
            ("EQ", Color32::from_rgb(255, 170, 70), &self.eqc, 1.2),
        ];
        for (i, (name, col, v, w)) in curves.iter().enumerate() {
            plot.line(&painter, self.f.iter().copied().zip(v.iter().copied()), Stroke::new(*w, *col));
            painter.text(Pos2::new(pr.left() + 6.0 + i as f32 * 70.0, pr.top() + 4.0), Align2::LEFT_TOP, *name, FontId::proportional(10.0), *col);
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
        for f in &self.filts {
            let x = plot.tx(f.f);
            painter.line_segment([Pos2::new(x, pr.bottom() - 6.0), Pos2::new(x, pr.bottom())], Stroke::new(1.5, Color32::from_rgb(255, 170, 70)));
        }
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let fr = plot.fx(h.x);
            let i = self.f.partition_point(|&x| x < fr).min(self.f.len() - 1);
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 18.0), format!("{}   measured {:.1}   corrected {:.1} dB", crate::app::fmt_hz(fr), self.measured[i], self.predicted[i]), Color32::WHITE, Align2::RIGHT_TOP);
        }
        // numbers
        let band: Vec<usize> = (0..self.f.len()).filter(|&i| self.f[i] >= self.band.0 && self.f[i] <= self.band.1).collect();
        let dev = |v: &Vec<f32>| (band.iter().map(|&i| (v[i] - self.target[i]).powi(2)).sum::<f32>() / band.len().max(1) as f32).sqrt();
        ui.label(RichText::new(format!("Deviation from the target in the band: {:.1} dB rms before, {:.1} dB after. Preamp {:.1} dB.", dev(&self.measured), dev(&self.predicted), -eq::peak_boost(&self.filts, self.design_sr()))).small());
        egui::CollapsingHeader::new(format!("Filters ({})", self.filts.len())).default_open(true).show(ui, |ui| {
            egui::Grid::new("eq_filters").num_columns(4).spacing([16.0, 2.0]).show(ui, |ui| {
                for (i, f) in self.filts.iter().enumerate() {
                    ui.label(format!("{}", i + 1));
                    ui.label(crate::app::fmt_hz(f.f));
                    ui.label(format!("{:+.1} dB", f.gain));
                    ui.label(format!("Q {:.2}", f.q));
                    ui.end_row();
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Export for").small().weak());
            egui::ComboBox::from_id_salt("eq_rate").width(78.0).selected_text(format!("{} Hz", RATES[self.rate])).show_ui(ui, |ui| {
                for (i, r) in RATES.iter().enumerate() {
                    ui.selectable_value(&mut self.rate, i, format!("{r} Hz"));
                }
            });
            egui::ComboBox::from_id_salt("eq_taps").width(78.0).selected_text(format!("{} taps", TAPS[self.taps])).show_ui(ui, |ui| {
                for (i, t) in TAPS.iter().enumerate() {
                    ui.selectable_value(&mut self.taps, i, format!("{t} taps"));
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            let has = !self.filts.is_empty();
            let b = |ui: &mut Ui, i: usize, label: &str, tip: &str, title: &'static str, kind: &'static str, ext: &'static [&'static str], name: &str, this: &mut Self| {
                if ui.add_enabled(has && this.saves[i].is_none(), egui::Button::new(label)).on_hover_text(tip).clicked() {
                    this.saves[i] = Some(dialogs::save(title, kind, ext, name.into()));
                }
            };
            b(ui, 0, "Equalizer APO", "Config.txt lines: Preamp and one PK filter each", "Export Equalizer APO config", "Text", &["txt"], "eq_apo.txt", self);
            b(ui, 1, "Biquads", "Coefficients for the chosen sample rate", "Export biquad coefficients", "CSV", &["csv"], "eq_biquads.csv", self);
            b(ui, 2, "FIR linear", "Linear-phase FIR WAV (latency of half the taps)", "Export FIR", "WAV", &["wav"], "eq_fir_linear.wav", self);
            b(ui, 3, "FIR min-phase", "Minimum-phase FIR WAV (least delay)", "Export FIR", "WAV", &["wav"], "eq_fir_minphase.wav", self);
        });
        ui.label(RichText::new("Low frequencies need long FIRs: 8192 taps or more at 48 kHz for filters below 100 Hz. Correct only what the room does at one position of the microphone: cuts of peaks work, boosts of nulls do not.").small().weak());
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("smooth", self.smooth);
        o.put("band_lo", self.band.0);
        o.put("band_hi", self.band.1);
        o.put("n", self.n_filters);
        o.put("boost", self.max_boost);
        o.put("cut", self.max_cut);
        o.put("qmax", self.qmax);
        o.put("tilt", self.tilt);
        o.put("taps", self.taps);
        o.put("rate", self.rate);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("smooth") {
            self.smooth = v.min(SMOOTH.len() - 1);
        }
        i.set("band_lo", &mut self.band.0);
        i.set("band_hi", &mut self.band.1);
        i.set("n", &mut self.n_filters);
        i.set("boost", &mut self.max_boost);
        i.set("cut", &mut self.max_cut);
        i.set("qmax", &mut self.qmax);
        i.set("tilt", &mut self.tilt);
        if let Some(v) = i.get::<usize>("taps") {
            self.taps = v.min(TAPS.len() - 1);
        }
        if let Some(v) = i.get::<usize>("rate") {
            self.rate = v.min(RATES.len() - 1);
        }
        self.band.0 = self.band.0.clamp(20.0, 2000.0);
        self.band.1 = self.band.1.clamp(self.band.0 * 1.5, 20000.0);
        self.n_filters = self.n_filters.clamp(1, 20);
        self.max_boost = self.max_boost.clamp(0.0, 12.0);
        self.max_cut = self.max_cut.clamp(0.0, 24.0);
        self.qmax = self.qmax.clamp(1.0, 20.0);
        self.tilt = self.tilt.clamp(-6.0, 6.0);
    }
}
