//! ITU-R BS.1770 / EBU R128 loudness (momentary, short-term, integrated, LRA), true peak, PPM and VU.

use super::plot::{canvas, inset, readout, Plot};
use super::tap::RingTap;
use super::*;
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};
use std::collections::VecDeque;

const TARGETS: [(&str, f32); 5] = [("EBU R128 -23", -23.0), ("ATSC A/85 -24", -24.0), ("Podcast -16", -16.0), ("Streaming -14", -14.0), ("Custom", -23.0)];
const HIST_LEN: usize = 3000;

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// K-weighting: high-shelf (+4 dB) then RLB high-pass, derived for any sample rate.
fn k_weight(fs: f64) -> [Biquad; 2] {
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad { b: [1.0, -2.0, 1.0], a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0], z: [0.0; 2] };
    [shelf, hp]
}

/// 4x oversampling polyphase FIR for the true-peak estimate.
struct TruePeak {
    taps: [[f32; 12]; 4],
    hist: [[f32; 12]; 2],
    pos: usize,
}

impl TruePeak {
    fn new() -> Self {
        let mut taps = [[0f32; 12]; 4];
        let n = 48usize;
        let mut h = vec![0f64; n];
        for (m, v) in h.iter_mut().enumerate() {
            let x = (m as f64 - (n as f64 - 1.0) / 2.0) / 4.0;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
            let w = 0.42 - 0.5 * (std::f64::consts::TAU * m as f64 / (n as f64 - 1.0)).cos() + 0.08 * (2.0 * std::f64::consts::TAU * m as f64 / (n as f64 - 1.0)).cos();
            *v = sinc * w;
        }
        for p in 0..4 {
            let sum: f64 = (0..12).map(|j| h[4 * j + p]).sum();
            for j in 0..12 {
                taps[p][j] = (h[4 * j + p] / sum) as f32;
            }
        }
        Self { taps, hist: [[0.0; 12]; 2], pos: 0 }
    }

    /// Highest interpolated magnitude for the new samples of one channel.
    fn feed(&mut self, ch: usize, x: &[f32]) -> f32 {
        let mut peak = 0f32;
        let h = &mut self.hist[ch];
        for &s in x {
            h.rotate_right(1);
            h[0] = s;
            for t in &self.taps {
                let v: f32 = t.iter().zip(h.iter()).map(|(a, b)| a * b).sum();
                peak = peak.max(v.abs());
            }
        }
        let _ = self.pos;
        peak
    }
}

pub struct Engine {
    sr: u32,
    k: [[Biquad; 2]; 2],
    sub: [f64; 2],
    sub_n: usize,
    sub_len: usize,
    q: VecDeque<f64>,
    blocks: Vec<f32>,
    st_hist: Vec<f32>,
    pub hist: VecDeque<(f32, f32)>,
    pub momentary: f32,
    pub short: f32,
    pub tp_db: [f32; 2],
    tp: TruePeak,
    pub ppm: [f32; 2],
    pub ppm_hold: [(f32, f32); 2],
    pub vu: f32,
}

fn lufs(z: f64) -> f32 {
    (-0.691 + 10.0 * z.max(1e-12).log10()) as f32
}

impl Engine {
    pub fn new(sr: u32) -> Self {
        let kw = k_weight(sr as f64);
        Self {
            sr,
            k: [kw, kw],
            sub: [0.0; 2],
            sub_n: 0,
            sub_len: (sr / 10) as usize,
            q: VecDeque::with_capacity(32),
            blocks: Vec::new(),
            st_hist: Vec::new(),
            hist: VecDeque::new(),
            momentary: -120.0,
            short: -120.0,
            tp_db: [-120.0; 2],
            tp: TruePeak::new(),
            ppm: [0.0; 2],
            ppm_hold: [(0.0, 0.0); 2],
            vu: 0.0,
        }
    }

    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len();
        if n == 0 {
            return;
        }
        let dt = n as f32 / self.sr as f32;
        for (c, x) in [l, r].into_iter().enumerate() {
            let tp = self.tp.feed(c, x);
            self.tp_db[c] = self.tp_db[c].max(20.0 * tp.max(1e-9).log10());
            let pk = x.iter().fold(0f32, |m, v| m.max(v.abs()));
            self.ppm[c] = (self.ppm[c] * 10f32.powf(-dt / 1.7)).max(pk);
            if pk >= self.ppm_hold[c].0 || self.ppm_hold[c].1 > 2.0 {
                self.ppm_hold[c] = (pk.max(self.ppm[c]), 0.0);
            } else {
                self.ppm_hold[c].1 += dt;
            }
        }
        let ms: f32 = l.iter().zip(r).map(|(a, b)| a * a + b * b).sum::<f32>() / (2 * n) as f32;
        let a = 1.0 - (-dt / 0.15).exp();
        self.vu += (ms - self.vu) * a;
        for i in 0..n {
            for (c, s) in [l[i], r[i]].into_iter().enumerate() {
                let shelved = self.k[c][0].run(s as f64);
                let y = self.k[c][1].run(shelved);
                self.sub[c] += y * y;
            }
            self.sub_n += 1;
            if self.sub_n >= self.sub_len {
                let z = (self.sub[0] + self.sub[1]) / self.sub_n as f64;
                self.sub = [0.0; 2];
                self.sub_n = 0;
                self.q.push_back(z);
                if self.q.len() > 30 {
                    self.q.pop_front();
                }
                self.block_done();
            }
        }
    }

    fn block_done(&mut self) {
        let mean = |k: usize| self.q.iter().rev().take(k).sum::<f64>() / k.min(self.q.len()).max(1) as f64;
        if self.q.len() >= 4 {
            let m = mean(4);
            self.momentary = lufs(m);
            if self.blocks.len() < 1 << 20 {
                self.blocks.push(m as f32);
            }
        }
        let st = if self.q.len() >= 30 { Some(mean(30)) } else { None };
        self.short = lufs(mean(30));
        if let Some(s) = st {
            if self.st_hist.len() < 1 << 20 {
                self.st_hist.push(s as f32);
            }
        }
        self.hist.push_back((self.momentary, self.short));
        if self.hist.len() > HIST_LEN {
            self.hist.pop_front();
        }
    }

    /// Gated integrated loudness (absolute -70 LUFS, relative -10 LU).
    pub fn integrated(&self) -> Option<f32> {
        let abs = 10f64.powf((-70.0 + 0.691) / 10.0);
        let v: Vec<f64> = self.blocks.iter().map(|&b| b as f64).filter(|&b| b > abs).collect();
        if v.is_empty() {
            return None;
        }
        let rel = v.iter().sum::<f64>() / v.len() as f64 * 10f64.powf(-1.0);
        let g: Vec<f64> = v.into_iter().filter(|&b| b > rel).collect();
        (!g.is_empty()).then(|| lufs(g.iter().sum::<f64>() / g.len() as f64))
    }

    /// Loudness range (EBU Tech 3342): 10th to 95th percentile of the gated short-term values.
    pub fn lra(&self) -> Option<f32> {
        let abs = 10f64.powf((-70.0 + 0.691) / 10.0);
        let v: Vec<f64> = self.st_hist.iter().map(|&b| b as f64).filter(|&b| b > abs).collect();
        if v.len() < 5 {
            return None;
        }
        let rel = v.iter().sum::<f64>() / v.len() as f64 * 10f64.powf(-2.0);
        let mut g: Vec<f32> = v.into_iter().filter(|&b| b > rel).map(|b| lufs(b)).collect();
        if g.len() < 5 {
            return None;
        }
        g.sort_by(|a, b| a.total_cmp(b));
        let at = |p: f32| g[((g.len() - 1) as f32 * p).round() as usize];
        Some(at(0.95) - at(0.10))
    }

    pub fn reset(&mut self) {
        *self = Engine::new(self.sr);
    }
}

pub struct Loudness {
    tap: RingTap,
    eng: Option<Engine>,
    keep: bool,
    target_i: usize,
    custom: f32,
    was_running: bool,
    vu_ref: f32,
    summary: (Option<f32>, Option<f32>),
    sum_t: std::time::Instant,
}

impl Loudness {
    pub fn new() -> Self {
        Self { tap: RingTap::new(), eng: None, keep: false, target_i: 0, custom: -18.0, was_running: false, vu_ref: -18.0, summary: (None, None), sum_t: std::time::Instant::now() }
    }

    fn target(&self) -> f32 {
        if self.target_i == TARGETS.len() - 1 { self.custom } else { TARGETS[self.target_i].1 }
    }
}

fn bar(ui: &mut Ui, w: f32, h: f32) -> (Rect, egui::Painter) {
    let (r, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(r, 2.0, Color32::from_gray(24));
    (r, p)
}

fn fmt_l(v: Option<f32>, unit: &str) -> String {
    match v {
        Some(x) if x > -100.0 => format!("{x:.1}{unit}"),
        _ => "-".into(),
    }
}

impl Tool for Loudness {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        let running = visible || self.keep;
        let sr = cx.sh.sr.load(std::sync::atomic::Ordering::Relaxed);
        if !running || sr == 0 {
            self.was_running = false;
            return;
        }
        if !self.was_running {
            self.tap.sync(cx.sh);
            self.was_running = true;
            return;
        }
        if self.eng.as_ref().map(|e| e.sr) != Some(sr) {
            self.eng = Some(Engine::new(sr));
        }
        let (l, r) = self.tap.poll(cx.sh);
        let e = self.eng.as_mut().unwrap();
        e.process(l, r);
        if self.sum_t.elapsed().as_millis() > 500 {
            self.sum_t = std::time::Instant::now();
            self.summary = (e.integrated(), e.lra());
        }
    }

    fn ui(&mut self, ui: &mut Ui, _cx: &mut Ctx) {
        let target = self.target();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Reset").on_hover_text("Restart the integrated measurement and peaks").clicked() {
                if let Some(e) = &mut self.eng {
                    e.reset();
                }
                self.summary = (None, None);
            }
            egui::ComboBox::from_id_salt("ld_target").width(130.0).selected_text(format!("Target: {}", TARGETS[self.target_i].0)).show_ui(ui, |ui| {
                for (i, t) in TARGETS.iter().enumerate() {
                    ui.selectable_value(&mut self.target_i, i, t.0);
                }
            });
            if self.target_i == TARGETS.len() - 1 {
                ui.add(egui::DragValue::new(&mut self.custom).range(-60.0..=0.0).speed(0.1).suffix(" LUFS"));
            }
            ui.checkbox(&mut self.keep, "Keep measuring when closed");
        });
        let Some(e) = &self.eng else {
            ui.label(RichText::new("waiting for audio...").weak());
            return;
        };
        let (int, lra) = self.summary;
        let col_for = |v: f32| {
            let d = (v - target).abs();
            if v < -100.0 { Color32::from_gray(120) } else if d <= 1.0 { Color32::from_rgb(100, 230, 130) } else if d <= 2.5 { Color32::from_rgb(240, 200, 80) } else { Color32::from_rgb(240, 120, 100) }
        };
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            readout(ui, "Momentary LUFS", &fmt_l(Some(e.momentary), ""), col_for(e.momentary));
            readout(ui, "Short-term LUFS", &fmt_l(Some(e.short), ""), col_for(e.short));
            readout(ui, "Integrated LUFS", &fmt_l(int, ""), int.map(col_for).unwrap_or(Color32::from_gray(120)));
            readout(ui, "Range LU", &fmt_l(lra, ""), Color32::from_rgb(200, 210, 230));
            let tp = e.tp_db[0].max(e.tp_db[1]);
            readout(ui, "True peak dBTP", &fmt_l(Some(tp), ""), if tp > -1.0 { Color32::from_rgb(240, 100, 90) } else { Color32::from_rgb(200, 210, 230) });
        });
        ui.add_space(4.0);
        // loudness bar: momentary fill, short-term and integrated markers, target line
        let w = ui.available_width().min(700.0);
        let (r, p) = bar(ui, w, 16.0);
        let x_of = |v: f32| r.left() + ((v + 50.0) / 50.0).clamp(0.0, 1.0) * r.width();
        let fill = Rect::from_min_max(r.min, Pos2::new(x_of(e.momentary), r.bottom()));
        p.rect_filled(fill, 2.0, col_for(e.momentary).gamma_multiply(0.75));
        for v in (-50..=0).step_by(10) {
            p.text(Pos2::new(x_of(v as f32), r.bottom() + 1.0), Align2::CENTER_TOP, format!("{v}"), FontId::proportional(9.0), crate::views::TXT);
        }
        p.line_segment([Pos2::new(x_of(target), r.top() - 2.0), Pos2::new(x_of(target), r.bottom() + 2.0)], Stroke::new(2.0, Color32::from_rgb(255, 90, 120)));
        p.line_segment([Pos2::new(x_of(e.short), r.top()), Pos2::new(x_of(e.short), r.bottom())], Stroke::new(2.0, Color32::from_rgb(255, 255, 255)));
        if let Some(i) = int {
            p.line_segment([Pos2::new(x_of(i), r.top()), Pos2::new(x_of(i), r.bottom())], Stroke::new(2.0, Color32::from_rgb(90, 160, 255)));
        }
        ui.add_space(14.0);
        // PPM bars with hold, VU
        let w = ui.available_width().min(700.0);
        for (c, name) in ["L", "R"].iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(*name).monospace().color(crate::app::CH_COLORS[c]));
                let (r, p) = bar(ui, w - 30.0, 9.0);
                let x_of = |lin: f32| r.left() + ((20.0 * lin.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0) * r.width();
                let col = if e.ppm[c] > 0.95 { Color32::from_rgb(235, 70, 70) } else if e.ppm[c] > 0.25 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) };
                p.rect_filled(Rect::from_min_max(r.min, Pos2::new(x_of(e.ppm[c]), r.bottom())), 2.0, col);
                p.line_segment([Pos2::new(x_of(e.ppm_hold[c].0), r.top()), Pos2::new(x_of(e.ppm_hold[c].0), r.bottom())], Stroke::new(2.0, Color32::WHITE));
                ui.label(RichText::new(format!("{:.1}", 20.0 * e.ppm[c].max(1e-6).log10())).small().monospace());
            });
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("VU").monospace());
            let (r, p) = bar(ui, w - 30.0, 9.0);
            let db = 10.0 * e.vu.max(1e-12).log10() - self.vu_ref;
            let x = r.left() + ((db + 20.0) / 23.0).clamp(0.0, 1.0) * r.width();
            p.rect_filled(Rect::from_min_max(r.min, Pos2::new(x, r.bottom())), 2.0, if db > 0.0 { Color32::from_rgb(235, 120, 80) } else { Color32::from_rgb(150, 190, 255) });
            let z = r.left() + 20.0 / 23.0 * r.width();
            p.line_segment([Pos2::new(z, r.top()), Pos2::new(z, r.bottom())], Stroke::new(1.0, Color32::WHITE));
            ui.label(RichText::new(format!("{db:+.1}")).small().monospace());
            ui.add(egui::DragValue::new(&mut self.vu_ref).range(-30.0..=0.0).speed(0.1).prefix("0 VU = ").suffix(" dBFS"));
        });
        ui.add_space(4.0);
        // history
        let (_resp, painter, rect) = canvas(ui, 90.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let plot = Plot::new(pr, -(HIST_LEN as f32) / 10.0, 0.0, -50.0, 0.0, false);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|v| format!("{:.0}s", v));
        plot.frame(&painter);
        let n = e.hist.len();
        let t_of = |i: usize| -((n - 1 - i) as f32) / 10.0;
        let step = (n / (pr.width() as usize * 2).max(1)).max(1);
        plot.line(&painter, e.hist.iter().enumerate().step_by(step).map(|(i, h)| (t_of(i), h.0)), Stroke::new(1.0, Color32::from_rgb(90, 190, 255)));
        plot.line(&painter, e.hist.iter().enumerate().step_by(step).map(|(i, h)| (t_of(i), h.1)), Stroke::new(1.6, Color32::from_rgb(255, 170, 70)));
        let ty = plot.ty(target);
        painter.line_segment([Pos2::new(pr.left(), ty), Pos2::new(pr.right(), ty)], Stroke::new(1.0, Color32::from_rgb(255, 90, 120)));
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "LUFS", FontId::proportional(9.0), crate::views::TXT);
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("target", self.target_i);
        o.put("custom", self.custom);
        o.put("keep", self.keep);
        o.put("vu_ref", self.vu_ref);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("target", &mut self.target_i);
        i.set("custom", &mut self.custom);
        i.set("keep", &mut self.keep);
        i.set("vu_ref", &mut self.vu_ref);
        self.target_i = self.target_i.min(TARGETS.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sr: u32, f: f64, amp: f64, secs: f64) -> Vec<f32> {
        (0..(sr as f64 * secs) as usize).map(|i| (amp * (std::f64::consts::TAU * f * i as f64 / sr as f64).sin()) as f32).collect()
    }

    #[test]
    fn bs1770_reference_tone() {
        // EBU: 997 Hz stereo sine at -20 dBFS (peak) reads -20.0 LUFS ... but K-weighting is +0.7 dB at 1 kHz,
        // and a -20 dBFS stereo sine gives -20.0 + 0.0 (both channels) = -20.0 LUFS +/- 0.1
        let sr = 48000;
        let x = tone(sr, 997.0, 0.1, 6.0);
        let mut e = Engine::new(sr);
        for c in x.chunks(512) {
            e.process(c, c);
        }
        let i = e.integrated().unwrap();
        assert!((i + 20.0).abs() < 0.15, "integrated {i}");
        assert!((e.short + 20.0).abs() < 0.15);
        // true peak of a sine at fs/4 phase offset is above the sample peak
        assert!(e.tp_db[0] > -20.2 && e.tp_db[0] < -19.7, "tp {}", e.tp_db[0]);
    }

    #[test]
    fn gating_ignores_silence() {
        let sr = 48000;
        let mut e = Engine::new(sr);
        let loud = tone(sr, 1000.0, 0.1, 5.0);
        let quiet = vec![0f32; sr as usize * 5];
        for c in loud.chunks(480).chain(quiet.chunks(480)) {
            e.process(c, c);
        }
        let i = e.integrated().unwrap();
        assert!((i + 20.0).abs() < 0.2, "integrated {i}");
    }
}
