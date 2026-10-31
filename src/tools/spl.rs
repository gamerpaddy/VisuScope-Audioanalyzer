//! Sound level meter: A / C / Z weighting, Fast / Slow / Impulse time weighting, Leq, Lmax, peak,
//! percentile levels and a level history. Runs in the time domain on the samples of the input ring.

use super::plot::{canvas, heading, inset, readout, Plot};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const WEIGHT: [&str; 3] = ["A", "C", "Z"];
const TIME: [(&str, &str); 3] = [("F", "Fast, 125 ms"), ("S", "Slow, 1 s"), ("I", "Impulse, 35 ms rise / 1.5 s fall")];
const HIST_BINS: usize = 1600;
const HIST_N: usize = 600;
const SINE_RMS_DB: f32 = -3.0103;

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// Bilinear transform of (b2 s^2 + b1 s + b0) / (a2 s^2 + a1 s + a0).
    fn from_analog(num: [f64; 3], den: [f64; 3], sr: f64) -> Self {
        let c = 2.0 * sr;
        let (b2, b1, b0) = (num[0], num[1], num[2]);
        let (a2, a1, a0) = (den[0], den[1], den[2]);
        let big_a0 = a2 * c * c + a1 * c + a0;
        Self {
            b: [(b2 * c * c + b1 * c + b0) / big_a0, 2.0 * (b0 - b2 * c * c) / big_a0, (b2 * c * c - b1 * c + b0) / big_a0],
            a: [2.0 * (a0 - a2 * c * c) / big_a0, (a2 * c * c - a1 * c + a0) / big_a0],
            z: [0.0; 2],
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    fn mag(&self, f: f64, sr: f64) -> f64 {
        let w = std::f64::consts::TAU * f / sr;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b[0] + self.b[1] * c1 + self.b[2] * c2;
        let ni = -(self.b[1] * s1 + self.b[2] * s2);
        let dr = 1.0 + self.a[0] * c1 + self.a[1] * c2;
        let di = -(self.a[0] * s1 + self.a[1] * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// IEC 61672 A or C weighting as a cascade of biquads, normalised to 0 dB at 1 kHz.
struct Weighting {
    secs: Vec<Biquad>,
    gain: f64,
}

impl Weighting {
    fn new(a_weighting: bool, sr: f64) -> Self {
        // pole frequencies pre-warped so the bilinear transform puts them where the analogue filter has them
        let w = |f: f64| 2.0 * sr * (std::f64::consts::PI * f / sr).tan();
        let (w1, w2, w3, w4) = (w(20.598997), w(107.65265), w(737.86223), w(12194.217));
        let mut secs = vec![Biquad::from_analog([1.0, 0.0, 0.0], [1.0, 2.0 * w1, w1 * w1], sr), Biquad::from_analog([0.0, 0.0, 1.0], [1.0, 2.0 * w4, w4 * w4], sr)];
        if a_weighting {
            secs.push(Biquad::from_analog([1.0, 0.0, 0.0], [1.0, w2 + w3, w2 * w3], sr));
        }
        let m: f64 = secs.iter().map(|s| s.mag(1000.0, sr)).product();
        Self { secs, gain: 1.0 / m }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let mut y = x;
        for s in self.secs.iter_mut() {
            y = s.run(y);
        }
        y * self.gain
    }

    fn reset(&mut self) {
        for s in self.secs.iter_mut() {
            s.z = [0.0; 2];
        }
    }
}

pub struct Spl {
    ch: usize,
    weighting: usize,
    time: usize,
    /// dB SPL of a full-scale sine
    fs_sine_spl: f32,
    calibrated: bool,
    cal_ref: f32,
    sr: f32,
    wa: Weighting,
    wc: Weighting,
    last_total: u64,
    /// time-weighted mean square of the chosen weighting
    p: f64,
    /// unweighted mean square, slow: used by the calibration button
    pz: f64,
    pmax: f64,
    peak_c: f64,
    sum: [f64; 3],
    cnt: u64,
    hist: Vec<f32>,
    hist_total: f32,
    history: Vec<f32>,
    hist_t: f32,
    frame_t: Option<std::time::Instant>,
    buf: (Vec<f32>, Vec<f32>),
    lp: f32,
}

impl Spl {
    pub fn new() -> Self {
        let sr = 48000.0;
        Self {
            ch: 0,
            weighting: 0,
            time: 0,
            fs_sine_spl: SINE_RMS_DB,
            calibrated: false,
            cal_ref: 94.0,
            sr,
            wa: Weighting::new(true, sr as f64),
            wc: Weighting::new(false, sr as f64),
            last_total: 0,
            p: 0.0,
            pz: 0.0,
            pmax: 0.0,
            peak_c: 0.0,
            sum: [0.0; 3],
            cnt: 0,
            hist: vec![0.0; HIST_BINS],
            hist_total: 0.0,
            history: Vec::new(),
            hist_t: 0.0,
            frame_t: None,
            buf: (Vec::new(), Vec::new()),
            lp: -200.0,
        }
    }

    /// dB SPL added to a digital RMS level in dB.
    fn offset(&self) -> f32 {
        self.fs_sine_spl - SINE_RMS_DB
    }

    fn db(&self, ms: f64) -> f32 {
        10.0 * (ms.max(1e-20) as f32).log10() + self.offset()
    }

    fn reset_stats(&mut self) {
        self.pmax = 0.0;
        self.peak_c = 0.0;
        self.sum = [0.0; 3];
        self.cnt = 0;
        self.hist.fill(0.0);
        self.hist_total = 0.0;
        self.history.clear();
    }

    fn reset_filters(&mut self, sr: f32) {
        if (sr - self.sr).abs() > 0.5 {
            self.sr = sr;
            self.wa = Weighting::new(true, sr as f64);
            self.wc = Weighting::new(false, sr as f64);
        } else {
            self.wa.reset();
            self.wc.reset();
        }
        self.p = 0.0;
        self.pz = 0.0;
    }

    fn process(&mut self, cx: &Ctx) {
        let sr = cx.sr;
        if (sr - self.sr).abs() > 0.5 {
            self.reset_filters(sr);
            self.reset_stats();
        }
        let total = cx.sh.ring.lock().total;
        if total < self.last_total || total - self.last_total > sr as u64 {
            // restart of the input or the window was closed for a while
            self.last_total = total;
            self.reset_filters(sr);
            return;
        }
        let n = (total - self.last_total) as usize;
        if n == 0 {
            return;
        }
        self.buf.0.resize(n, 0.0);
        self.buf.1.resize(n, 0.0);
        {
            let ring = cx.sh.ring.lock();
            ring.copy_abs(self.last_total as usize, &mut self.buf.0, &mut self.buf.1);
        }
        self.last_total = total;

        let srd = sr as f64;
        let (tau_rise, tau_fall) = match self.time {
            0 => (0.125, 0.125),
            1 => (1.0, 1.0),
            _ => (0.035, 1.5),
        };
        let (kr, kf) = (1.0 - (-1.0 / (srd * tau_rise)).exp(), 1.0 - (-1.0 / (srd * tau_fall)).exp());
        let kz = 1.0 - (-1.0 / srd).exp();
        let src = if self.ch == 0 { &self.buf.0 } else { &self.buf.1 };
        for &s in src.iter() {
            let x = s as f64;
            let a = self.wa.run(x);
            let c = self.wc.run(x);
            let sel = match self.weighting {
                0 => a,
                1 => c,
                _ => x,
            };
            let e = sel * sel;
            self.p += (e - self.p) * if e > self.p { kr } else { kf };
            self.pz += (x * x - self.pz) * kz;
            self.pmax = self.pmax.max(self.p);
            self.peak_c = self.peak_c.max(c.abs());
            self.sum[0] += a * a;
            self.sum[1] += c * c;
            self.sum[2] += x * x;
        }
        self.cnt += n as u64;
        self.lp = self.db(self.p);
    }

    /// Level exceeded for `pct` percent of the time.
    fn percentile(&self, pct: f32) -> Option<f32> {
        if self.hist_total < 1.0 {
            return None;
        }
        let target = self.hist_total * pct / 100.0;
        let mut acc = 0.0;
        for i in (0..HIST_BINS).rev() {
            acc += self.hist[i];
            if acc >= target {
                return Some(i as f32 * 0.1 - 60.0 + 0.05);
            }
        }
        None
    }
}

impl Tool for Spl {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        if !visible {
            self.last_total = u64::MAX;
            self.frame_t = None;
            return;
        }
        self.process(cx);
        let now = std::time::Instant::now();
        let dt = self.frame_t.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0).min(0.25);
        self.frame_t = Some(now);
        // percentile statistics are kept on the displayed (calibrated) level in 0.1 dB bins from -60 dB
        let bin = ((self.lp + 60.0) * 10.0).round();
        if bin >= 0.0 && (bin as usize) < HIST_BINS && self.cnt > 0 {
            self.hist[bin as usize] += dt;
            self.hist_total += dt;
        }
        self.hist_t += dt;
        if self.hist_t >= 0.1 {
            self.hist_t = 0.0;
            self.history.push(self.lp);
            if self.history.len() > HIST_N {
                self.history.remove(0);
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Weighting").small().weak());
            let w0 = self.weighting;
            for (i, n) in WEIGHT.iter().enumerate() {
                ui.selectable_value(&mut self.weighting, i, *n);
            }
            ui.label(RichText::new("Time").small().weak());
            let t0 = self.time;
            for (i, n) in TIME.iter().enumerate() {
                ui.selectable_value(&mut self.time, i, n.0).on_hover_text(n.1);
            }
            if w0 != self.weighting || t0 != self.time {
                self.p = 0.0;
                self.pmax = 0.0;
            }
            ui.label(RichText::new("Input").small().weak());
            ui.selectable_value(&mut self.ch, 0, "L");
            ui.selectable_value(&mut self.ch, 1, "R");
            if ui.button("Reset").on_hover_text("Restart Leq, Lmax, peak and the percentile levels").clicked() {
                self.reset_stats();
            }
        });
        let unit = format!("dB({}) {}", WEIGHT[self.weighting], TIME[self.time].0);
        let col = if self.calibrated { Color32::from_rgb(120, 230, 150) } else { Color32::from_rgb(230, 200, 110) };
        let leq = |s: f64| if self.cnt > 0 { format!("{:.1}", self.db(s / self.cnt as f64)) } else { "-".into() };
        let pc = |p: f32| self.percentile(p).map(|v| format!("{v:.1}")).unwrap_or("-".into());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add(egui::Label::new(RichText::new(format!("{:.1}", self.lp)).size(44.0).strong().color(col).monospace()).wrap_mode(egui::TextWrapMode::Extend));
                ui.add(egui::Label::new(RichText::new(&unit).small().weak()).wrap_mode(egui::TextWrapMode::Extend));
            });
            readout(ui, &format!("L{}eq", WEIGHT[self.weighting]), &leq(self.sum[self.weighting]), Color32::from_rgb(150, 200, 255));
            readout(ui, &format!("L{}max {}", WEIGHT[self.weighting], TIME[self.time].0), &if self.pmax > 0.0 { format!("{:.1}", self.db(self.pmax)) } else { "-".into() }, Color32::from_rgb(255, 170, 90));
            readout(ui, "LCpeak", &if self.peak_c > 0.0 { format!("{:.1}", self.db(self.peak_c * self.peak_c * 0.5)) } else { "-".into() }, Color32::from_rgb(255, 120, 120));
            readout(ui, "L10", &pc(10.0), Color32::from_rgb(190, 190, 200));
            readout(ui, "L50", &pc(50.0), Color32::from_rgb(190, 190, 200));
            readout(ui, "L90", &pc(90.0), Color32::from_rgb(190, 190, 200));
        });
        if !self.calibrated {
            ui.label(RichText::new("Not calibrated: levels are dB relative to full scale. Open Calibration below.").small().color(Color32::from_rgb(230, 200, 110)));
        }
        ui.label(RichText::new(format!("Leq over {}   all weightings: LAeq {}  LCeq {}  LZeq {}", super::fmt_time(self.cnt as f32 / self.sr), leq(self.sum[0]), leq(self.sum[1]), leq(self.sum[2]))).small().weak());

        egui::CollapsingHeader::new("Calibration").default_open(!self.calibrated).show(ui, |ui| {
            heading(ui, "With an acoustic calibrator");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.cal_ref).speed(0.1).range(40.0..=140.0).suffix(" dB SPL"));
                if ui.button("Calibrate now").on_hover_text("Put the calibrator on the microphone, then click: the current unweighted 1 s level is set to this value").clicked() {
                    if self.pz > 1e-16 {
                        let off = self.cal_ref - 10.0 * (self.pz as f32).log10();
                        self.fs_sine_spl = off + SINE_RMS_DB;
                        self.calibrated = true;
                        self.reset_stats();
                    } else {
                        cx.req.note = Some("Calibration failed: no signal on the input".into());
                    }
                }
            });
            heading(ui, "Or from the specification");
            ui.horizontal(|ui| {
                ui.label("Full-scale sine =");
                if ui.add(egui::DragValue::new(&mut self.fs_sine_spl).speed(0.1).range(-20.0..=200.0).suffix(" dB SPL")).changed() {
                    self.calibrated = true;
                }
                if ui.small_button("Uncalibrated").clicked() {
                    self.fs_sine_spl = SINE_RMS_DB;
                    self.calibrated = false;
                }
            });
            ui.label(RichText::new("The SPL of a sine at digital full scale. Microphone calibration files change only the spectrum views, not this meter.").small().weak());
        });

        if self.history.len() < 2 {
            return;
        }
        let (resp, painter, rect) = canvas(ui, 90.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (mn, mx) = self.history.iter().fold((f32::MAX, f32::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
        let lo = ((mn - 4.0) / 10.0).floor() * 10.0;
        let hi = (((mx + 4.0) / 10.0).ceil() * 10.0).max(lo + 30.0);
        let plot = Plot::new(pr, -(HIST_N as f32) * 0.1, 0.0, lo, hi, false);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|v| format!("{:.0}s", v));
        plot.frame(&painter);
        let n = self.history.len();
        plot.line(&painter, self.history.iter().enumerate().map(|(i, &v)| (-((n - 1 - i) as f32) * 0.1, v)), Stroke::new(1.5, col));
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let t = plot.fx(h.x);
            let i = (n as f32 - 1.0 + t * 10.0).round().clamp(0.0, n as f32 - 1.0) as usize;
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{:.0} s   {:.1} dB", t, self.history[i]), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn report(&self, _cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        if self.cnt == 0 {
            return Vec::new();
        }
        let leq = |s: f64| format!("{:.1} dB", self.db(s / self.cnt as f64));
        let pc = |p: f32| self.percentile(p).map(|v| format!("{v:.1} dB")).unwrap_or("-".into());
        let w = WEIGHT[self.weighting];
        let mut rows = vec![
            vec![format!("Current level L{w}{}", TIME[self.time].0), format!("{:.1} dB", self.lp)],
            vec!["LAeq".into(), leq(self.sum[0])],
            vec!["LCeq".into(), leq(self.sum[1])],
            vec!["LZeq".into(), leq(self.sum[2])],
            vec![format!("L{w}max {}", TIME[self.time].0), format!("{:.1} dB", self.db(self.pmax))],
            vec!["LCpeak".into(), format!("{:.1} dB", self.db(self.peak_c * self.peak_c * 0.5))],
            vec!["L10 / L50 / L90".into(), format!("{} / {} / {}", pc(10.0), pc(50.0), pc(90.0))],
            vec!["Measured over".into(), super::fmt_time(self.cnt as f32 / self.sr)],
        ];
        rows.push(vec!["Calibration".into(), if self.calibrated { format!("full-scale sine = {:.1} dB SPL", self.fs_sine_spl) } else { "none (levels are dB re full scale)".into() }]);
        let mut blocks = vec![Block::Table { head: vec!["Quantity".into(), "Value".into()], rows }];
        if self.history.len() > 2 {
            let pts: Vec<(f32, f32)> = self.history.iter().enumerate().map(|(i, v)| ((i as f32 - self.history.len() as f32 + 1.0) * 0.1, *v)).collect();
            let ser = vec![Series { name: String::new(), color: COLORS[0], pts }];
            let y = auto_range(&ser, 10.0, 30.0);
            blocks.push(Block::Plot(PlotSpec { title: "Level history".into(), xlog: false, xlabel: "s".into(), ylabel: "dB".into(), x: (-(self.history.len() as f32) * 0.1, 0.0), y, series: ser }));
        }
        vec![Section { id: "SPL", title: "Sound level".into(), blocks }]
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("ch", self.ch);
        o.put("weighting", self.weighting);
        o.put("time", self.time);
        o.put("fs_sine_spl", self.fs_sine_spl);
        o.put("calibrated", self.calibrated);
        o.put("cal_ref", self.cal_ref);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("ch") {
            self.ch = v.min(1);
        }
        if let Some(v) = i.get::<usize>("weighting") {
            self.weighting = v.min(2);
        }
        if let Some(v) = i.get::<usize>("time") {
            self.time = v.min(2);
        }
        i.set("fs_sine_spl", &mut self.fs_sine_spl);
        i.set("calibrated", &mut self.calibrated);
        i.set("cal_ref", &mut self.cal_ref);
        self.fs_sine_spl = self.fs_sine_spl.clamp(-20.0, 200.0);
        self.cal_ref = self.cal_ref.clamp(40.0, 140.0);
        if !self.calibrated {
            self.fs_sine_spl = SINE_RMS_DB;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_through(w: &mut Weighting, f: f64, sr: f64) -> f64 {
        let n = (sr * 0.5) as usize;
        let mut s = 0.0;
        for i in 0..n {
            let y = w.run((std::f64::consts::TAU * f * i as f64 / sr).sin());
            if i > n / 2 {
                s += y * y;
            }
        }
        20.0 * (s / (n / 2) as f64 * 2.0).sqrt().log10() // sine amplitude in dB
    }

    #[test]
    fn weighting_matches_iec_table() {
        let sr = 96000.0;
        // IEC 61672: A -19.1 dB @ 100 Hz, +1.2 dB @ 2 kHz, -1.1 dB @ 8 kHz, -6.6 dB @ 16 kHz; C -0.3 dB @ 100 Hz, -3.0 dB @ 31.5 Hz
        let mut a = Weighting::new(true, sr);
        assert!((rms_through(&mut a, 100.0, sr) + 19.1).abs() < 0.3);
        let mut a = Weighting::new(true, sr);
        assert!((rms_through(&mut a, 2000.0, sr) - 1.2).abs() < 0.3);
        let mut a = Weighting::new(true, sr);
        assert!((rms_through(&mut a, 8000.0, sr) + 1.1).abs() < 0.3);
        let mut a = Weighting::new(true, sr);
        let v16 = rms_through(&mut a, 16000.0, sr);
        assert!((v16 + 6.6).abs() < 0.8, "16k: {v16}");
        let mut c = Weighting::new(false, sr);
        assert!((rms_through(&mut c, 100.0, sr) + 0.3).abs() < 0.3);
        let mut c = Weighting::new(false, sr);
        assert!((rms_through(&mut c, 31.5, sr) + 3.0).abs() < 0.4);
    }
}
