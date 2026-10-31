//! Live transfer function window: magnitude, phase and coherence of measured / reference while any
//! programme material plays (see `crate::tf` for the estimator).

use super::plot::{canvas, inset, Plot};
use super::response::smooth_log;
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const SMOOTH: [(f32, &str); 5] = [(0.0, "off"), (24.0, "1/24 oct"), (12.0, "1/12 oct"), (6.0, "1/6 oct"), (3.0, "1/3 oct")];
const LOWER: [&str; 3] = ["Phase", "Coherence", "None"];
const LOG_N: usize = 900;

struct Curve {
    f: Vec<f32>,
    /// unsmoothed, unblanked magnitude, what "Store" keeps
    raw: Vec<f32>,
    mag: Vec<f32>,
    /// phase with NaN gaps at the +-180 wraps so the line does not draw vertical strokes
    phase: Vec<(f32, f32)>,
    phase_at: Vec<f32>,
    coh: Vec<f32>,
}

pub struct LiveTf {
    p: TfParams,
    hold: bool,
    smooth: usize,
    lower: usize,
    blank: bool,
    coh_thr: f32,
    y_lo: f32,
    y_hi: f32,
    curve: Option<Curve>,
    key: (u64, usize, bool, u32),
    delay_ms: f32,
    frames: u32,
}

impl LiveTf {
    pub fn new() -> Self {
        Self {
            p: TfParams::default(),
            hold: false,
            smooth: 2,
            lower: 0,
            blank: true,
            coh_thr: 0.6,
            y_lo: -30.0,
            y_hi: 15.0,
            curve: None,
            key: (0, 0, false, 0),
            delay_ms: 0.0,
            frames: 0,
        }
    }

    fn build(&mut self, o: &TfResult) {
        if o.n == 0 || o.mag.len() < 4 {
            return;
        }
        let df = o.sr / o.n as f32;
        let nyq = o.sr * 0.5;
        let (f0, f1) = (20.0f32.max(df * 2.0), 20000.0f32.min(nyq * 0.98));
        let ppo = LOG_N as f32 / (f1 / f0).log2();
        let last = o.mag.len() - 2;
        let at = |data: &[f32], f: f32| -> f32 {
            let b = f / df;
            let k = (b.floor() as usize).min(last);
            let t = b - k as f32;
            data[k] * (1.0 - t) + data[k + 1] * t
        };
        let f: Vec<f32> = (0..LOG_N).map(|i| f0 * (f1 / f0).powf(i as f32 / (LOG_N - 1) as f32)).collect();
        let raw: Vec<f32> = f.iter().map(|&x| at(&o.mag, x)).collect();
        let nan: Vec<bool> = raw.iter().map(|v| !v.is_finite()).collect();
        let clean: Vec<f32> = raw.iter().map(|v| if v.is_finite() { *v } else { -200.0 }).collect();
        let mut mag = smooth_log(&clean, ppo, SMOOTH[self.smooth].0);
        let raw_store = raw.clone();
        let coh: Vec<f32> = f.iter().map(|&x| at(&o.coh, x)).collect();
        let phase_at: Vec<f32> = f.iter().map(|&x| at(&o.phase, x)).collect();
        let mut phase = Vec::with_capacity(LOG_N + 16);
        for i in 0..LOG_N {
            let bad = nan[i] || (self.blank && coh[i] < self.coh_thr);
            if bad {
                mag[i] = f32::NAN;
            }
            let p = phase_at[i];
            if bad || !p.is_finite() || (i > 0 && (p - phase_at[i - 1]).abs() > 180.0) {
                phase.push((f32::NAN, f32::NAN));
            }
            if !bad && p.is_finite() {
                phase.push((f[i], p));
            }
        }
        self.curve = Some(Curve { f, raw: raw_store, mag, phase, phase_at, coh });
        self.delay_ms = o.delay_ms;
        self.frames = o.frames;
    }
}

impl Tool for LiveTf {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        // the estimator only runs while the window is open and not held
        let on = visible && !self.hold;
        if on != self.p.on {
            self.p.on = on;
        }
        {
            let mut g = cx.sh.tf_params.lock();
            if *g != self.p {
                *g = self.p.clone();
            }
        }
        if !visible {
            return;
        }
        let key_now = (self.key.0, self.smooth, self.blank, self.coh_thr.to_bits());
        let o = cx.sh.tf_out.try_lock().and_then(|o| (o.seq != self.key.0 || key_now != self.key).then(|| o.clone()));
        if let Some(o) = o {
            self.build(&o);
            self.key = (o.seq, self.smooth, self.blank, self.coh_thr.to_bits());
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Reference").small().weak());
            ui.selectable_value(&mut self.p.ref_r, false, "L").on_hover_text("L carries the reference (system input), R the measured signal");
            ui.selectable_value(&mut self.p.ref_r, true, "R").on_hover_text("R carries the reference (system input), L the measured signal");
            ui.separator();
            ui.checkbox(&mut self.p.auto_delay, "Auto delay").on_hover_text("Find the latency between the channels and remove it, so music or noise works as a stimulus");
            if self.p.auto_delay {
                ui.label(RichText::new(format!("{:.2} ms", self.delay_ms)).monospace());
            } else {
                ui.add(egui::DragValue::new(&mut self.p.delay_ms).speed(0.01).range(-500.0..=500.0).suffix(" ms"));
            }
            if ui.button("Reset").on_hover_text("Restart the averaging").clicked() {
                self.p.reset = self.p.reset.wrapping_add(1);
            }
            ui.checkbox(&mut self.hold, "Freeze");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Smooth").small().weak());
            egui::ComboBox::from_id_salt("tf_sm").width(78.0).selected_text(SMOOTH[self.smooth].1).show_ui(ui, |ui| {
                for (i, s) in SMOOTH.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth, i, s.1);
                }
            });
            ui.label(RichText::new("Below").small().weak());
            egui::ComboBox::from_id_salt("tf_lo").width(92.0).selected_text(LOWER[self.lower]).show_ui(ui, |ui| {
                for (i, s) in LOWER.iter().enumerate() {
                    ui.selectable_value(&mut self.lower, i, *s);
                }
            });
            ui.checkbox(&mut self.blank, "Blank").on_hover_text("Hide the trace where the coherence is low (no usable signal or too much noise)");
            ui.add_enabled(self.blank, egui::DragValue::new(&mut self.coh_thr).speed(0.01).range(0.05..=0.99).fixed_decimals(2));
            ui.add(egui::DragValue::new(&mut self.y_lo).range(-200.0..=self.y_hi - 5.0).speed(0.5).prefix("min "));
            ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 5.0..=100.0).speed(0.5).prefix("max "));
        });

        cx.lib.ui(ui, self.curve.as_ref().map(|c| (c.f.clone(), c.raw.clone(), c.phase_at.clone())));
        let Some(c) = &self.curve else {
            ui.add_space(8.0);
            ui.label(RichText::new("Needs a signal on both channels: the system input on the reference channel, its output on the other.\nPlay music or noise through the system; the averaged result appears here (uses the Average setting in the sidebar).").weak());
            return;
        };
        let has_lower = self.lower < 2;
        let (resp, painter, rect) = canvas(ui, 140.0);
        let split = if has_lower { rect.height() * 0.68 } else { rect.height() };
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split));
        let pr = inset(top);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (fa, fb) = (c.f[0], *c.f.last().unwrap());
        let plot = Plot::new(pr, fa, fb, self.y_lo, self.y_hi, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        let ppo = LOG_N as f32 / (fb / fa).log2();
        let (layers, rel) = cx.lib.layers(&c.f, ppo, SMOOTH[self.smooth].0);
        for (_, col, v) in &layers {
            plot.line(&painter, c.f.iter().copied().zip(v.iter().copied()), Stroke::new(1.2, *col));
        }
        let rel_at = |i: usize| rel.as_ref().map(|r| r[i]).unwrap_or(0.0);
        plot.line(&painter, c.f.iter().copied().zip(c.mag.iter().enumerate().map(|(i, m)| m - rel_at(i))), Stroke::new(1.6, Color32::from_rgb(90, 200, 255)));
        for (i, (name, col, _)) in layers.iter().enumerate() {
            painter.text(Pos2::new(pr.right() - 6.0, pr.bottom() - 6.0 - i as f32 * 12.0), Align2::RIGHT_BOTTOM, name, FontId::proportional(10.0), *col);
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);

        if has_lower {
            let low = egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max);
            let lp = inset(low);
            let (lo, hi, unit) = if self.lower == 0 { (-180.0, 180.0, "deg") } else { (0.0, 1.05, "coh") };
            let p2 = Plot::new(lp, plot.x0, plot.x1, lo, hi, true);
            p2.grid_y(&painter, &|v| format!("{v:.1}").trim_end_matches(".0").to_string());
            p2.grid_x(&painter, &|_| String::new());
            p2.frame(&painter);
            if self.lower == 0 {
                p2.line(&painter, c.phase.iter().copied(), Stroke::new(1.4, Color32::from_rgb(255, 170, 70)));
            } else {
                p2.line(&painter, c.f.iter().copied().zip(c.coh.iter().copied()), Stroke::new(1.4, Color32::from_rgb(230, 130, 230)));
            }
            painter.text(Pos2::new(rect.left() + 2.0, lp.top()), Align2::LEFT_TOP, unit, FontId::proportional(9.0), crate::views::TXT);
        }
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right() && p.y >= rect.top()) {
            let f = plot.fx(h.x);
            let i = c.f.partition_point(|&x| x < f).min(c.f.len() - 1);
            let mut txt = format!("{}   {}", crate::app::fmt_hz(f), if c.mag[i].is_finite() { format!("{:.2} dB", c.mag[i]) } else { "-".into() });
            if c.phase_at[i].is_finite() {
                txt += &format!("   {:.0}°", c.phase_at[i]);
            }
            txt += &format!("   coh {:.2}", c.coh[i]);
            painter.line_segment([Pos2::new(h.x, pr.top()), Pos2::new(h.x, rect.bottom() - 18.0)], Stroke::new(1.0, Color32::from_white_alpha(50)));
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
        }
        painter.text(
            Pos2::new(pr.left() + 6.0, rect.bottom() - 20.0),
            Align2::LEFT_BOTTOM,
            format!("{} averages, delay {:.2} ms", self.frames, self.delay_ms),
            FontId::proportional(9.5),
            Color32::from_white_alpha(90),
        );
    }

    fn report(&self, _cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        let Some(c) = &self.curve else { return Vec::new() };
        let x = (c.f[0], *c.f.last().unwrap());
        let mag = vec![Series { name: format!("magnitude, smoothing {}", SMOOTH[self.smooth].1), color: COLORS[0], pts: c.f.iter().copied().zip(c.mag.iter().copied()).collect() }];
        let y = auto_range(&mag, 5.0, 20.0);
        let mut pts = Vec::new();
        for (i, p) in c.phase_at.iter().enumerate() {
            if i > 0 && (p - c.phase_at[i - 1]).abs() > 180.0 {
                pts.push((f32::NAN, f32::NAN));
            }
            pts.push((c.f[i], *p));
        }
        vec![Section {
            id: "Transfer",
            title: "Live transfer function".into(),
            blocks: vec![
                Block::Text(format!("{} averages, delay between the channels {:.2} ms.", self.frames, self.delay_ms)),
                Block::Plot(PlotSpec { title: "Magnitude".into(), xlog: true, xlabel: "Hz".into(), ylabel: "dB".into(), x, y, series: mag }),
                Block::Plot(PlotSpec { title: "Phase".into(), xlog: true, xlabel: "Hz".into(), ylabel: "degrees".into(), x, y: (-180.0, 180.0), series: vec![Series { name: String::new(), color: COLORS[1], pts }] }),
                Block::Plot(PlotSpec { title: "Coherence".into(), xlog: true, xlabel: "Hz".into(), ylabel: String::new(), x, y: (0.0, 1.0), series: vec![Series { name: String::new(), color: COLORS[3], pts: c.f.iter().copied().zip(c.coh.iter().copied()).collect() }] }),
            ],
        }]
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("smooth", self.smooth);
        o.put("lower", self.lower);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
        o.put("ref_r", self.p.ref_r);
        o.put("auto", self.p.auto_delay);
        o.put("delay_ms", self.p.delay_ms);
        o.put("blank", self.blank);
        o.put("coh_thr", self.coh_thr);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("smooth") {
            self.smooth = v.min(SMOOTH.len() - 1);
        }
        if let Some(v) = i.get::<usize>("lower") {
            self.lower = v.min(LOWER.len() - 1);
        }
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        i.set("ref_r", &mut self.p.ref_r);
        i.set("auto", &mut self.p.auto_delay);
        i.set("delay_ms", &mut self.p.delay_ms);
        i.set("blank", &mut self.blank);
        i.set("coh_thr", &mut self.coh_thr);
        self.y_hi = self.y_hi.clamp(-50.0, 100.0);
        self.y_lo = self.y_lo.clamp(-200.0, self.y_hi - 5.0);
        self.coh_thr = self.coh_thr.clamp(0.05, 0.99);
        self.p.delay_ms = self.p.delay_ms.clamp(-500.0, 500.0);
    }
}
