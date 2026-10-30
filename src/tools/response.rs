//! Frequency-response window: sweep controls plus magnitude / phase / group delay / coherence.

use super::plot::{canvas, inset, Plot};
use super::sweep::{State, SweepResult};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const SMOOTH: [(f32, &str); 5] = [(0.0, "off"), (24.0, "1/24 oct"), (12.0, "1/12 oct"), (6.0, "1/6 oct"), (3.0, "1/3 oct")];
const LOWER: [&str; 4] = ["Phase", "Group delay", "Coherence", "None"];
const LOG_N: usize = 900;

struct Curve {
    f: Vec<f32>,
    mag: Vec<f32>,
    phase_deg: Vec<f32>,
    gd_ms: Vec<f32>,
    coh: Option<Vec<f32>>,
}

pub struct Response {
    smooth: usize,
    lower: usize,
    y_lo: f32,
    y_hi: f32,
    normalize: bool,
    curve: Option<Curve>,
    key: (u64, usize),
    held: Option<(String, Vec<f32>, Vec<f32>)>,
    save: Option<dialogs::Pending>,
}

/// 1/N-octave smoothing of a power quantity sampled on a log grid.
fn smooth_log(v: &[f32], pts_per_oct: f32, n: f32) -> Vec<f32> {
    if n <= 0.0 {
        return v.to_vec();
    }
    let half = (pts_per_oct / n * 0.5).max(0.0);
    let mut pre = vec![0f64; v.len() + 1];
    for (i, x) in v.iter().enumerate() {
        pre[i + 1] = pre[i] + 10f64.powf(*x as f64 / 10.0);
    }
    (0..v.len())
        .map(|i| {
            let a = (i as f32 - half).floor().max(0.0) as usize;
            let b = ((i as f32 + half).ceil() as usize + 1).min(v.len());
            (10.0 * ((pre[b] - pre[a]) / (b - a).max(1) as f64).max(1e-30).log10()) as f32
        })
        .collect()
}

impl Response {
    pub fn new() -> Self {
        Self { smooth: 3, lower: 0, y_lo: -40.0, y_hi: 20.0, normalize: false, curve: None, key: (0, usize::MAX), held: None, save: None }
    }

    fn build(&mut self, r: &SweepResult) {
        let h = &r.h;
        let nyq = r.sr * 0.5;
        let (f0, f1) = (10.0f32.max(h.df * 2.0), nyq.min(r.params.f1 * 1.2).min(24000.0).max(100.0));
        let ppo = LOG_N as f32 / (f1 / f0).log2();
        let at = |data: &[f32], f: f32| -> f32 {
            let b = f / h.df;
            let k = (b.floor() as usize).min(data.len() - 2);
            let t = b - k as f32;
            data[k] * (1.0 - t) + data[k + 1] * t
        };
        let f: Vec<f32> = (0..LOG_N).map(|i| f0 * (f1 / f0).powf(i as f32 / (LOG_N - 1) as f32)).collect();
        let mag_raw: Vec<f32> = f.iter().map(|&x| at(&h.mag_db, x)).collect();
        let mag = smooth_log(&mag_raw, ppo, SMOOTH[self.smooth].0);
        // group delay from the unwrapped phase of the full-resolution response
        let gd_full: Vec<f32> = (1..h.phase.len() - 1).map(|k| -(h.phase[k + 1] - h.phase[k - 1]) / (2.0 * std::f32::consts::TAU * h.df) * 1000.0).collect();
        let gd_raw: Vec<f32> = f.iter().map(|&x| at(&gd_full, (x - h.df).max(0.0))).collect();
        let gd_ms = if SMOOTH[self.smooth].0 > 0.0 {
            // smooth linearly (delay is not a power)
            let half = (ppo / SMOOTH[self.smooth].0 * 0.5) as usize;
            (0..LOG_N).map(|i| gd_raw[i.saturating_sub(half)..(i + half + 1).min(LOG_N)].iter().sum::<f32>() / (i + half + 1).min(LOG_N).saturating_sub(i.saturating_sub(half)).max(1) as f32).collect()
        } else {
            gd_raw
        };
        let phase_deg = f.iter().map(|&x| at(&h.phase, x).to_degrees()).collect();
        let coh = h.coh.as_ref().map(|c| f.iter().map(|&x| at(c, x)).collect());
        self.curve = Some(Curve { f, mag, phase_deg, gd_ms, coh });
        self.key = (r.id, self.smooth);
    }

    fn controls(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let sw = &mut *cx.sweep;
        let busy = sw.busy();
        ui.horizontal(|ui| {
            let b = egui::Button::new(RichText::new(if busy { "Stop" } else { "Measure" }).strong()).min_size(egui::vec2(96.0, 26.0));
            if ui.add(b).clicked() {
                if busy {
                    sw.cancel();
                } else {
                    sw.start();
                }
            }
            if busy {
                ui.add(egui::ProgressBar::new(sw.progress()).desired_width(120.0).show_percentage());
            }
            match &sw.state {
                State::Arming(_) => {
                    ui.label(RichText::new("starting output...").small().weak());
                }
                State::Computing { .. } => {
                    ui.label(RichText::new("deconvolving...").small().weak());
                }
                State::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(255, 130, 100), e);
                }
                _ => {}
            }
        });
        egui::CollapsingHeader::new("Sweep settings").default_open(false).show(ui, |ui| {
            let p = &mut sw.p;
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("sweep_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Level");
                    ui.add(egui::Slider::new(&mut p.amp_db, -60.0..=0.0).suffix(" dBFS"));
                    ui.end_row();
                    ui.label("Range");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut p.f0).range(5.0..=2000.0).speed(1.0).suffix(" Hz"));
                        ui.label("to");
                        ui.add(egui::DragValue::new(&mut p.f1).range(1000.0..=96000.0).speed(10.0).suffix(" Hz"));
                    });
                    ui.end_row();
                    ui.label("Length");
                    ui.add(egui::Slider::new(&mut p.secs, 0.5..=12.0).suffix(" s"));
                    ui.end_row();
                    ui.label("Tail");
                    ui.add(egui::Slider::new(&mut p.tail, 0.3..=4.0).suffix(" s")).on_hover_text("Recording time after the sweep ends: must cover latency and the decay of the room");
                    ui.end_row();
                    ui.label("Averages");
                    ui.add(egui::Slider::new(&mut p.averages, 1..=16)).on_hover_text("More runs lower the noise; 2 or more also give the coherence");
                    ui.end_row();
                    ui.label("Response window");
                    ui.add(egui::Slider::new(&mut p.resp_ms, 20.0..=2000.0).logarithmic(true).suffix(" ms")).on_hover_text("Short = gated (ignores late reflections), long = more resolution");
                    ui.end_row();
                    ui.label("IR length");
                    ui.add(egui::Slider::new(&mut p.ir_ms, 200.0..=4000.0).suffix(" ms"));
                    ui.end_row();
                    ui.label("Reference");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut p.loopback, false, "ideal").on_hover_text("Assume a perfect sweep: the result includes the sound card");
                        ui.selectable_value(&mut p.loopback, true, "R input").on_hover_text("L is the device under test, R carries the reference (loopback of the output): the sound card cancels");
                    });
                    ui.end_row();
                    if !p.loopback {
                        ui.label("Input channel");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut p.dut_ch, 0, "L");
                            ui.selectable_value(&mut p.dut_ch, 1, "R");
                        });
                        ui.end_row();
                    }
                });
            });
        });
        let _ = cx.gen_running;
    }
}

impl Tool for Response {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        if let Some(d) = &self.save {
            if let Some(res) = d.take() {
                self.save = None;
                if let (Some(path), Some(c)) = (res, &self.curve) {
                    let mut s = String::from("freq_hz,mag_db,phase_deg,group_delay_ms,coherence\n");
                    for i in 0..c.f.len() {
                        s += &format!("{:.3},{:.3},{:.2},{:.4},{}\n", c.f[i], c.mag[i], c.phase_deg[i], c.gd_ms[i], c.coh.as_ref().map(|v| format!("{:.4}", v[i])).unwrap_or_default());
                    }
                    cx.req.note = Some(match std::fs::write(&path, s) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        self.controls(ui, cx);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Smooth").small().weak());
            egui::ComboBox::from_id_salt("rsp_sm").width(78.0).selected_text(SMOOTH[self.smooth].1).show_ui(ui, |ui| {
                for (i, s) in SMOOTH.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth, i, s.1);
                }
            });
            ui.label(RichText::new("Below").small().weak());
            egui::ComboBox::from_id_salt("rsp_lo").width(92.0).selected_text(LOWER[self.lower]).show_ui(ui, |ui| {
                for (i, s) in LOWER.iter().enumerate() {
                    ui.selectable_value(&mut self.lower, i, *s);
                }
            });
            ui.checkbox(&mut self.normalize, "Normalise").on_hover_text("Shift the magnitude so the 1 kHz value reads 0 dB");
            if ui.button("Hold").on_hover_text("Keep the current magnitude trace as a comparison").clicked() {
                self.held = self.curve.as_ref().map(|c| ("held".to_string(), c.f.clone(), c.mag.clone()));
            }
            if self.held.is_some() && ui.small_button("Clear hold").clicked() {
                self.held = None;
            }
            if ui.add_enabled(self.curve.is_some() && self.save.is_none(), egui::Button::new("CSV")).on_hover_text("Export magnitude, phase, delay and coherence").clicked() {
                self.save = Some(dialogs::save("Export response", "CSV", &["csv"], "response.csv".into()));
            }
            ui.add(egui::DragValue::new(&mut self.y_lo).range(-200.0..=self.y_hi - 5.0).speed(0.5).prefix("min "));
            ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 5.0..=100.0).speed(0.5).prefix("max "));
        });

        // rebuild the display curve if there is a new result or a new smoothing
        if let Some(r) = &cx.sweep.result {
            if self.key != (r.id, self.smooth) {
                self.build(r);
            }
        }
        let Some(c) = &self.curve else {
            ui.add_space(8.0);
            ui.label(RichText::new("Press Measure. The generator plays a log sweep while the input records it.\nUse a cable or a [Loopback] input; set Reference to \"R input\" to cancel the sound card.").weak());
            return;
        };
        let norm = if self.normalize { -c.f.iter().zip(&c.mag).min_by(|a, b| (a.0 - 1000.0).abs().total_cmp(&(b.0 - 1000.0).abs())).map(|x| *x.1).unwrap_or(0.0) } else { 0.0 };
        let has_lower = self.lower < 3 && !(self.lower == 2 && c.coh.is_none());

        let (resp, painter, rect) = canvas(ui, 140.0);
        let split = if has_lower { rect.height() * 0.68 } else { rect.height() };
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split));
        let pr = inset(top);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (fa, fb) = (c.f[0], *c.f.last().unwrap());
        let plot = Plot::new(pr, fa.max(10.0), fb, self.y_lo, self.y_hi, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        if let Some((_, f, m)) = &self.held {
            plot.line(&painter, f.iter().copied().zip(m.iter().copied()), Stroke::new(1.0, Color32::from_rgb(150, 150, 160)));
        }
        plot.line(&painter, c.f.iter().copied().zip(c.mag.iter().map(|m| m + norm)), Stroke::new(1.6, Color32::from_rgb(90, 200, 255)));
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);

        let mut lower_plot: Option<Plot> = None;
        if has_lower {
            let low = egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max);
            let lp = inset(low);
            let (ys, lo, hi, unit): (Box<dyn Iterator<Item = f32>>, f32, f32, &str) = match self.lower {
                0 => (Box::new(c.phase_deg.iter().copied()), -360.0, 360.0, "deg"),
                1 => {
                    let m = c.gd_ms.iter().fold(0f32, |a, b| a.max(b.abs())).max(0.5).min(200.0);
                    (Box::new(c.gd_ms.iter().copied()), -m * 0.25, m * 1.1, "ms")
                }
                _ => (Box::new(c.coh.as_ref().unwrap().iter().copied()), 0.0, 1.05, "coh"),
            };
            let p2 = Plot::new(lp, plot.x0, plot.x1, lo, hi, true);
            p2.grid_y(&painter, &|v| format!("{v:.1}").trim_end_matches(".0").to_string());
            p2.grid_x(&painter, &|_| String::new());
            p2.frame(&painter);
            p2.line(&painter, c.f.iter().copied().zip(ys), Stroke::new(1.4, [Color32::from_rgb(255, 170, 70), Color32::from_rgb(120, 230, 150), Color32::from_rgb(230, 130, 230)][self.lower]));
            painter.text(Pos2::new(rect.left() + 2.0, lp.top()), Align2::LEFT_TOP, unit, FontId::proportional(9.0), crate::views::TXT);
            lower_plot = Some(p2);
        }
        // hover read-out
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right() && p.y >= rect.top()) {
            let f = plot.fx(h.x);
            let i = c.f.partition_point(|&x| x < f).min(c.f.len() - 1);
            let mut txt = format!("{}   {:.2} dB", crate::app::fmt_hz(f), c.mag[i] + norm);
            if self.lower == 0 {
                txt += &format!("   {:.0}°", c.phase_deg[i]);
            }
            if self.lower == 1 {
                txt += &format!("   {:.3} ms", c.gd_ms[i]);
            }
            if let (2, Some(co)) = (self.lower, &c.coh) {
                txt += &format!("   coh {:.3}", co[i]);
            }
            painter.line_segment([Pos2::new(h.x, pr.top()), Pos2::new(h.x, rect.bottom() - 18.0)], Stroke::new(1.0, Color32::from_white_alpha(50)));
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
        }
        let _ = lower_plot;
        if let Some(r) = &cx.sweep.result {
            painter.text(Pos2::new(pr.left() + 6.0, rect.bottom() - 20.0 - if has_lower { 0.0 } else { 0.0 }), Align2::LEFT_BOTTOM, format!("{} run(s), input peak {:.1} dBFS, {} ms window", r.runs, r.capture_peak_db, r.params.resp_ms as i32), FontId::proportional(9.5), Color32::from_white_alpha(90));
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("smooth", self.smooth);
        o.put("lower", self.lower);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
        o.put("norm", self.normalize);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("smooth") {
            self.smooth = v.min(SMOOTH.len() - 1);
        }
        if let Some(v) = i.get::<usize>("lower") {
            self.lower = v.min(3);
        }
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        i.set("norm", &mut self.normalize);
        self.y_hi = self.y_hi.clamp(-50.0, 100.0);
        self.y_lo = self.y_lo.clamp(-200.0, self.y_hi - 5.0);
    }
}

