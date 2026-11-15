//! Device tests that need the generator and both input channels, or a test tone from the device itself:
//! crosstalk and channel matching (stepped sine, one channel driven at a time), wow and flutter (demodulation of a 3 or
//! 3.15 kHz tone from a tape or record) and multitone distortion (everything between the tones of the generator's multitone).

use super::plot::{canvas, inset, Plot};
use super::*;
use crate::tonegen::Wave;
use eframe::egui::{self, Align2, FontId, Pos2, RichText, Stroke};
use rustfft::num_complex::Complex32 as C;
use std::time::Instant;

const TABS: [&str; 3] = ["Crosstalk & matching", "Wow & flutter", "Multitone distortion"];
const XT_N: usize = 16384;
/// Weighting of IEC 386 / DIN 45507 for wow and flutter: (Hz, dB), peak at 4 Hz.
const WF_W: [(f32, f32); 16] = [(0.2, -30.6), (0.315, -19.7), (0.4, -15.0), (0.63, -8.4), (0.8, -6.0), (1.0, -4.2), (1.6, -1.8), (2.0, -0.9), (4.0, 0.0), (6.3, -0.9), (10.0, -2.1), (20.0, -5.9), (40.0, -10.4), (63.0, -14.2), (100.0, -17.3), (200.0, -23.0)];

/// Complex amplitude of the tone `f` in `x` (Hann window, scaled so a sine of amplitude 1 gives 1).
pub fn tone(x: &[f32], f: f32, sr: f32) -> C {
    let n = x.len() as f32;
    let w = std::f32::consts::TAU * f / sr;
    let mut s = C::new(0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let win = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n).cos();
        s += C::from_polar(v * win, -w * i as f32);
    }
    s * (4.0 / n)
}

fn wf_weight(f: f32) -> f32 {
    if !(0.2..=200.0).contains(&f) {
        return 0.0;
    }
    let j = WF_W.partition_point(|p| p.0 < f).clamp(1, WF_W.len() - 1);
    let t = (f.ln() - WF_W[j - 1].0.ln()) / (WF_W[j].0.ln() - WF_W[j - 1].0.ln());
    10f32.powf((WF_W[j - 1].1 + t * (WF_W[j].1 - WF_W[j - 1].1)) / 20.0)
}

pub struct WfOut {
    /// mean frequency of the tone
    pub f_mean: f32,
    /// weighted: 2 sigma peak and RMS; unweighted RMS (0.2 to 200 Hz); all in percent
    pub w_peak: f32,
    pub w_rms: f32,
    pub u_rms: f32,
    /// spectrum of the frequency deviation: (Hz, percent peak)
    pub spec: Vec<(f32, f32)>,
    pub secs: f32,
    /// the weighted deviation over time, in percent, and the time between its points
    pub trace: Vec<f32>,
    pub trace_dt: f32,
}

/// Wow and flutter of the tone near `f0` in `x`: the tone is mixed down, its frequency followed at about 2 kHz, and the
/// deviation from the mean weighted in the frequency domain.
pub fn wow_flutter(x: &[f32], sr: f32, f0: f32) -> Option<WfOut> {
    let d = ((sr / 2000.0).round() as usize).max(1);
    let fs = sr / d as f32;
    let w = std::f64::consts::TAU * f0 as f64 / sr as f64;
    let z: Vec<C> = x.chunks_exact(d).enumerate().map(|(m, c)| c.iter().enumerate().map(|(i, v)| { let a = -w * (m * d + i) as f64; C::new(*v * a.cos() as f32, *v * a.sin() as f32) }).sum::<C>() / d as f32).collect();
    let amp = z.iter().map(|c| c.norm()).sum::<f32>() / z.len().max(1) as f32;
    if z.len() < 1100 || amp < 1e-5 || z.iter().any(|c| c.norm() < amp * 0.1) {
        return None;
    }
    let dev: Vec<f32> = z.windows(2).map(|p| (p[1] * p[0].conj()).arg() * fs / std::f32::consts::TAU).collect();
    let n = 1usize << (usize::BITS - 1 - dev.len().leading_zeros());
    let dev = &dev[dev.len() - n..];
    let mean = dev.iter().sum::<f32>() / n as f32;
    let f_mean = f0 + mean;
    let mut buf: Vec<C> = dev.iter().map(|v| C::new((v - mean) / f_mean * 100.0, 0.0)).collect();
    let mut pl = rustfft::FftPlanner::new();
    pl.plan_fft_forward(n).process(&mut buf);
    let df = fs / n as f32;
    let fk = |k: usize| if k <= n / 2 { k as f32 * df } else { (n - k) as f32 * df };
    let spec: Vec<(f32, f32)> = (1..n / 2).map(|k| (k as f32 * df, buf[k].norm() * 2.0 / n as f32)).filter(|p| p.0 <= 200.0).collect();
    let u_rms = ((0..n).filter(|&k| (0.2..=200.0).contains(&fk(k))).map(|k| buf[k].norm_sqr()).sum::<f32>()).sqrt() / n as f32;
    for (k, v) in buf.iter_mut().enumerate() {
        *v *= wf_weight(fk(k));
    }
    pl.plan_fft_inverse(n).process(&mut buf);
    // (the ends carry the wrap-around of the filter: leave a tenth out on each side)
    let step = (n / 1024).max(1);
    let trace: Vec<f32> = buf[n / 10..n - n / 10].iter().step_by(step).map(|c| c.re / n as f32).collect();
    let mut t: Vec<f32> = buf[n / 10..n - n / 10].iter().map(|c| (c.re / n as f32).abs()).collect();
    let w_rms = (t.iter().map(|v| v * v).sum::<f32>() / t.len() as f32).sqrt();
    t.sort_by(|a, b| a.total_cmp(b));
    Some(WfOut { f_mean, w_peak: t[t.len() * 95 / 100], w_rms, u_rms, spec, secs: n as f32 / fs, trace, trace_dt: step as f32 / fs })
}

pub struct MtOut {
    /// distortion and noise between the tones relative to the tones (dB)
    pub tdn_db: f32,
    pub tones: Vec<f32>,
    /// spectrum relative to the strongest tone: (Hz, dB), and the same without the tones
    pub spec: Vec<(f32, f32)>,
    pub found: usize,
}

/// The frequencies the generator's multitone plays.
pub fn mt_freqs(p: &crate::tonegen::GenParams) -> Vec<f32> {
    let n = p.mt_count.clamp(1, 64);
    (0..n)
        .map(|k| {
            let x = if n > 1 { k as f32 / (n - 1) as f32 } else { 0.0 };
            if p.mt_log { p.mt_f0 * (p.mt_f1 / p.mt_f0).powf(x) } else { p.mt_f0 + (p.mt_f1 - p.mt_f0) * x }
        })
        .collect()
}

/// Power between the tones against the power of the tones, from 20 Hz to 20 kHz (or the top of the sample rate).
pub fn multitone(x: &[f32], sr: f32, tones: &[f32]) -> MtOut {
    let n = x.len();
    // Blackman-Harris: the skirts of the tones are 92 dB down, so they do not count as distortion
    let bh = |i: usize| { let a = std::f32::consts::TAU * i as f32 / n as f32; 0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos() };
    let mut buf: Vec<C> = x.iter().enumerate().map(|(i, v)| C::new(v * bh(i), 0.0)).collect();
    rustfft::FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    let df = sr / n as f32;
    let p: Vec<f32> = buf[..n / 2].iter().map(|c| c.norm_sqr()).collect();
    let mut is_tone = vec![false; n / 2];
    for &f in tones {
        let k = (f / df).round() as usize;
        for j in k.saturating_sub(6)..(k + 7).min(n / 2) {
            is_tone[j] = true;
        }
    }
    let (lo, hi) = ((20.0 / df).ceil() as usize, ((20000.0f32.min(sr * 0.48)) / df) as usize);
    let (mut sig, mut rest) = (0f64, 0f64);
    for k in lo..hi.min(n / 2) {
        if is_tone[k] { sig += p[k] as f64 } else { rest += p[k] as f64 }
    }
    let top = p.iter().copied().fold(1e-30, f32::max);
    let found = tones.iter().filter(|&&f| p.get((f / df).round() as usize).is_some_and(|v| *v > top * 1e-3)).count();
    // the display: the strongest bin of each of 600 log steps
    let mut spec: Vec<(f32, f32)> = Vec::new();
    let steps = 600;
    for s in 0..steps {
        let (a, b) = (20.0 * (hi as f32 * df / 20.0).powf(s as f32 / steps as f32), 20.0 * (hi as f32 * df / 20.0).powf((s + 1) as f32 / steps as f32));
        let (ka, kb) = ((a / df) as usize, ((b / df) as usize).max((a / df) as usize + 1).min(n / 2));
        if ka < kb {
            spec.push(((a * b).sqrt(), 10.0 * (p[ka..kb].iter().copied().fold(0.0, f32::max) / top).max(1e-20).log10()));
        }
    }
    MtOut { tdn_db: (10.0 * (rest / sig.max(1e-30)).max(1e-30).log10()) as f32, tones: tones.to_vec(), spec, found }
}

#[derive(Clone, Copy, Default)]
struct XtPt {
    f: f32,
    /// what leaks from the left into the right channel, and from the right into the left (dB)
    l2r: f32,
    r2l: f32,
    /// level and phase of the left against the right channel with both driven
    match_db: f32,
    match_deg: f32,
}

struct Run {
    pass: usize,
    idx: usize,
    since: Instant,
    sent: bool,
    mute0: [bool; 2],
}

pub struct DevTests {
    tab: usize,
    xt_level: f32,
    xt_points: usize,
    run: Option<Run>,
    xt: Vec<XtPt>,
    xt_done: bool,
    wf_nominal: usize,
    wf: Option<WfOut>,
    mt: Option<MtOut>,
    last: Instant,
    mt_level: f32,
}

impl DevTests {
    pub fn new() -> Self {
        Self { tab: 0, xt_level: -10.0, xt_points: 31, run: None, xt: Vec::new(), xt_done: false, wf_nominal: 0, wf: None, mt: None, last: Instant::now(), mt_level: -12.0 }
    }

    fn grab(cx: &Ctx, n: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        cx.sh.ring.lock().copy_last(&mut l, &mut r);
        (l, r)
    }

    fn xt_step(&mut self, cx: &mut Ctx) {
        let Some(run) = &mut self.run else { return };
        let f = self.xt[run.idx].f;
        if !run.sent {
            cx.req.gen_cmd = Some(GenCmd { wave: Some(Wave::Sine), freq: Some(f), level_db: Some(self.xt_level), on: Some(true), mute: Some([run.pass == 1, run.pass == 0]) });
            run.sent = true;
            run.since = Instant::now();
            return;
        }
        // let the tone settle, then a full block of it must be in the ring
        if run.since.elapsed().as_secs_f32() < 0.3 + XT_N as f32 / cx.sr {
            return;
        }
        let (l, r) = Self::grab(cx, XT_N);
        let (a, b) = (tone(&l, f, cx.sr), tone(&r, f, cx.sr));
        let db = |x: C, y: C| 20.0 * (x.norm().max(1e-9) / y.norm().max(1e-9)).log10();
        let p = &mut self.xt[run.idx];
        match run.pass {
            0 => p.l2r = db(b, a),
            1 => p.r2l = db(a, b),
            _ => (p.match_db, p.match_deg) = (db(a, b), (a * b.conj()).arg().to_degrees()),
        }
        run.sent = false;
        run.idx += 1;
        if run.idx >= self.xt.len() {
            run.idx = 0;
            run.pass += 1;
            if run.pass > 2 {
                cx.req.gen_cmd = Some(GenCmd { on: Some(false), mute: Some(run.mute0), ..Default::default() });
                self.run = None;
                self.xt_done = true;
            }
        }
    }

    fn xt_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            let running = self.run.is_some();
            if ui.add(egui::Button::new(RichText::new(if running { "Stop" } else { "Measure" }).strong()).min_size(egui::vec2(96.0, 26.0))).clicked() {
                if let Some(r) = self.run.take() {
                    cx.req.gen_cmd = Some(GenCmd { on: Some(false), mute: Some(r.mute0), ..Default::default() });
                } else {
                    let top = 20000f32.min(cx.sr * 0.45);
                    let n = self.xt_points.max(2);
                    self.xt = (0..n).map(|i| XtPt { f: 20.0 * (top / 20.0).powf(i as f32 / (n - 1) as f32), l2r: f32::NAN, r2l: f32::NAN, match_db: f32::NAN, match_deg: f32::NAN }).collect();
                    self.xt_done = false;
                    self.run = Some(Run { pass: 0, idx: 0, since: Instant::now(), sent: false, mute0: cx.tg.mute });
                }
            }
            ui.add_enabled_ui(!running, |ui| {
                ui.add(egui::DragValue::new(&mut self.xt_level).range(-60.0..=0.0).speed(0.2).prefix("level ").suffix(" dBFS"));
                ui.add(egui::DragValue::new(&mut self.xt_points).range(5..=120).prefix("points "));
            });
            if let Some(r) = &self.run {
                ui.add(egui::ProgressBar::new((r.pass * self.xt.len() + r.idx) as f32 / (3 * self.xt.len()) as f32).desired_width(120.0).show_percentage());
                ui.label(RichText::new(["left driven", "right driven", "both driven"][r.pass.min(2)]).small().weak());
            }
        });
        if self.xt.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Connect both outputs of the generator through the device to both inputs (left to left, right to right).\nThree passes of a stepped sine: left alone, right alone, both. The first two give what leaks into the silent channel,\nthe third the level and phase of left against right. The generator is used and switched off afterwards.").weak());
            return;
        }
        let worst = |v: &dyn Fn(&XtPt) -> f32| self.xt.iter().map(v).filter(|x| x.is_finite()).fold(f32::MIN, f32::max);
        if self.xt_done {
            let dev = self.xt.iter().map(|p| p.match_db.abs()).filter(|x| x.is_finite()).fold(0.0, f32::max);
            ui.label(RichText::new(format!("worst crosstalk  L>R {:.1} dB   R>L {:.1} dB      matching: up to {:.2} dB apart", worst(&|p| p.l2r), worst(&|p| p.r2l), dev)).monospace());
        }
        let (resp, painter, rect) = canvas(ui, 150.0);
        let split = rect.height() * 0.64;
        let pr = inset(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split)));
        let lr = inset(egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max));
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (f0, f1) = (self.xt[0].f, self.xt[self.xt.len() - 1].f);
        let lo = ((worst(&|p| -p.l2r).max(worst(&|p| -p.r2l)).max(60.0) + 10.0) / 10.0).ceil() * 10.0;
        let p1 = Plot::new(pr, f0, f1, -lo, 0.0, true);
        p1.grid_y(&painter, &|v| format!("{v:.0}"));
        p1.grid_x(&painter, &|_| String::new());
        p1.frame(&painter);
        let (cl, cr) = (Color32::from_rgb(90, 220, 255), Color32::from_rgb(255, 110, 190));
        p1.line(&painter, self.xt.iter().map(|p| (p.f, p.l2r)), Stroke::new(1.5, cl));
        p1.line(&painter, self.xt.iter().map(|p| (p.f, p.r2l)), Stroke::new(1.5, cr));
        painter.text(pr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, "crosstalk, dB", FontId::proportional(9.5), crate::views::TXT);
        painter.text(pr.right_top() + egui::vec2(-6.0, 4.0), Align2::RIGHT_TOP, "left into right", FontId::proportional(10.0), cl);
        painter.text(pr.right_top() + egui::vec2(-6.0, 16.0), Align2::RIGHT_TOP, "right into left", FontId::proportional(10.0), cr);
        let m = (self.xt.iter().map(|p| p.match_db.abs()).filter(|x| x.is_finite()).fold(0.0, f32::max) * 1.3).max(0.5);
        let p2 = Plot::new(lr, f0, f1, -m, m, true);
        p2.grid_y(&painter, &|v| format!("{v:.2}"));
        p2.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        p2.frame(&painter);
        p2.line(&painter, self.xt.iter().map(|p| (p.f, p.match_db)), Stroke::new(1.4, Color32::from_rgb(255, 190, 80)));
        painter.text(lr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, "left minus right, dB (both driven)", FontId::proportional(9.5), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right()) {
            let f = p1.fx(h.x);
            if let Some(p) = self.xt.iter().min_by(|a, b| (a.f / f).ln().abs().total_cmp(&(b.f / f).ln().abs())) {
                p1.label(&painter, Pos2::new(pr.center().x, pr.top() + 4.0), format!("{}   L>R {:.1}   R>L {:.1}   L-R {:.2} dB  {:.1}°", crate::app::fmt_hz(p.f), p.l2r, p.r2l, p.match_db, p.match_deg), Color32::WHITE, Align2::CENTER_TOP);
            }
        }
    }

    fn wf_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        const NOM: [(&str, f32); 3] = [("as measured", 0.0), ("3000 Hz", 3000.0), ("3150 Hz", 3150.0)];
        ui.horizontal_wrapped(|ui| {
            ui.label("Test tone");
            for (i, n) in NOM.iter().enumerate() {
                ui.selectable_value(&mut self.wf_nominal, i, n.0);
            }
            ui.label(RichText::new(format!("input channel {}", if cx.meas_ch == 0 { "L" } else { "R" })).small().weak());
        });
        if self.last.elapsed().as_secs_f32() > 0.2 {
            self.last = Instant::now();
            let m = &cx.an.metrics[cx.meas_ch.min(1)];
            let n = (crate::shared::RING_CAP).min((cx.sr * 9.0) as usize);
            // the lock is let go before the block is read: a lock held through the closure would wait for itself
            let have = (cx.sh.ring.lock().total.min(n as u64)) as usize;
            self.wf = None;
            if m.valid && m.fund_hz > 200.0 {
                let (l, r) = Self::grab(cx, have);
                let x = if cx.meas_ch == 0 { &l } else { &r };
                // live: the longest stretch up to now that holds the tone without a gap. It starts at half a second
                // and grows to nine, so the numbers are there at once and get finer (and reach lower wow) with time
                let mut len = have;
                while len as f32 >= cx.sr * 0.6 {
                    if let Some(o) = wow_flutter(&x[have - len..], cx.sr, m.fund_hz) {
                        self.wf = Some(o);
                        break;
                    }
                    len /= 2;
                }
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        let Some(o) = &self.wf else {
            ui.add_space(6.0);
            ui.label(RichText::new("Play the 3 kHz or 3.15 kHz tone of a test tape or test record into the input (the values show after half a second and get finer over 9 seconds).\nThe tone is followed in frequency: the slow changes are wow, the fast ones flutter. No steady tone is on the input now.").weak());
            return;
        };
        let nom = NOM[self.wf_nominal].1;
        ui.horizontal_wrapped(|ui| {
            let c = Color32::from_rgb(120, 230, 150);
            super::plot::readout(ui, "weighted peak (2 sigma), IEC 386", &format!("{:.3} %", o.w_peak), c);
            ui.add_space(12.0);
            super::plot::readout(ui, "weighted RMS", &format!("{:.3} %", o.w_rms), c);
            ui.add_space(12.0);
            super::plot::readout(ui, "unweighted RMS, 0.2 to 200 Hz", &format!("{:.3} %", o.u_rms), Color32::from_rgb(255, 190, 80));
            ui.add_space(12.0);
            super::plot::readout(ui, "mean frequency", &format!("{:.2} Hz", o.f_mean), Color32::WHITE);
            if nom > 0.0 {
                ui.add_space(12.0);
                super::plot::readout(ui, "speed error (drift)", &format!("{:+.3} %", (o.f_mean / nom - 1.0) * 100.0), Color32::WHITE);
            }
        });
        // the deviation as it happens: the newest on the right
        let (_, painter, rect) = canvas(ui, 80.0);
        let pr = inset(rect);
        if pr.width() > 40.0 && pr.height() > 20.0 && o.trace.len() > 1 {
            let span = o.trace.len() as f32 * o.trace_dt;
            let top = (o.trace.iter().fold(0.0f32, |a, v| a.max(v.abs())) * 1.2).max(0.005);
            let plot = Plot::new(pr, -span, 0.0, -top, top, false);
            plot.grid_y(&painter, &|v| format!("{v:.3}"));
            plot.frame(&painter);
            plot.line(&painter, o.trace.iter().enumerate().map(|(i, v)| (i as f32 * o.trace_dt - span, *v)), Stroke::new(1.2, Color32::from_rgb(120, 230, 150)));
            painter.text(pr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, format!("weighted deviation in %, the last {span:.1} s"), FontId::proportional(9.5), crate::views::TXT);
        }
        let (resp, painter, rect) = canvas(ui, 120.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let top = (o.spec.iter().map(|p| p.1).fold(0.0, f32::max) * 1.2).max(0.005);
        let plot = Plot::new(pr, 0.2, 200.0, 0.0, top, true);
        plot.grid_y(&painter, &|v| format!("{v:.3}"));
        plot.grid_x(&painter, &|f| format!("{f}"));
        plot.frame(&painter);
        plot.line(&painter, o.spec.iter().copied(), Stroke::new(1.4, Color32::from_rgb(90, 200, 255)));
        painter.text(pr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, format!("frequency deviation, % peak, against its rate in Hz ({:.1} s)", o.secs), FontId::proportional(9.5), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let f = plot.fx(h.x);
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{f:.2} Hz = once in {:.2} s", 1.0 / f), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn mt_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let is_mt = cx.tg.wave == Wave::Multitone && cx.tg.on;
        ui.horizontal_wrapped(|ui| {
            if ui.button(if is_mt { "Stop multitone" } else { "Start multitone" }).on_hover_text("Switch the signal generator to its multitone (number of tones, range and spacing are set in the Signal generator window)").clicked() {
                cx.req.gen_cmd = Some(if is_mt { GenCmd { on: Some(false), ..Default::default() } } else { GenCmd { wave: Some(Wave::Multitone), level_db: Some(self.mt_level), on: Some(true), ..Default::default() } });
            }
            ui.add(egui::DragValue::new(&mut self.mt_level).range(-60.0..=0.0).speed(0.2).prefix("level ").suffix(" dBFS RMS"));
            ui.label(RichText::new(format!("{} tones, {} to {}, input channel {}", cx.tg.mt_count, crate::app::fmt_hz(cx.tg.mt_f0), crate::app::fmt_hz(cx.tg.mt_f1), if cx.meas_ch == 0 { "L" } else { "R" })).small().weak());
        });
        if self.last.elapsed().as_secs_f32() > 0.4 {
            self.last = Instant::now();
            let n = if cx.sr > 60000.0 { 131072 } else { 65536 };
            // the lock is let go before the block is read: a lock held through the closure would wait for itself
            let filled = cx.sh.ring.lock().total >= n as u64;
            self.mt = filled.then(|| {
                let (l, r) = Self::grab(cx, n);
                multitone(if cx.meas_ch == 0 { &l } else { &r }, cx.sr, &mt_freqs(cx.tg))
            });
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        let Some(o) = &self.mt else { return };
        ui.horizontal_wrapped(|ui| {
            if o.found * 2 < o.tones.len() {
                ui.colored_label(Color32::from_rgb(255, 170, 90), "The tones of the generator are not on the input. Start the multitone and connect the device.");
            } else {
                super::plot::readout(ui, "distortion + noise between the tones, 20 Hz to 20 kHz", &format!("{:.1} dB   ({:.4} %)", o.tdn_db, 100.0 * 10f32.powf(o.tdn_db / 20.0)), Color32::from_rgb(120, 230, 150));
            }
        })
        .response
        .on_hover_text("A music-like load: all tones play at once, and everything the device adds between them (harmonics, intermodulation, noise) is summed and set against the tones. THD in the Measurements window does this for one tone, the Distortion sweeps for one tone or two against frequency or level.");
        let (_, painter, rect) = canvas(ui, 120.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 || o.spec.len() < 2 {
            return;
        }
        let plot = Plot::new(pr, 20.0, o.spec[o.spec.len() - 1].0, -140.0, 5.0, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        for &f in &o.tones {
            let x = plot.tx(f);
            if x >= pr.left() && x <= pr.right() {
                painter.line_segment([Pos2::new(x, pr.top()), Pos2::new(x, pr.top() + 5.0)], Stroke::new(1.5, Color32::from_rgb(255, 214, 10)));
            }
        }
        plot.line(&painter, o.spec.iter().copied(), Stroke::new(1.2, Color32::from_rgb(90, 200, 255)));
        painter.text(pr.left_top() + egui::vec2(6.0, 8.0), Align2::LEFT_TOP, "dB relative to the strongest tone (yellow marks: the tones)", FontId::proportional(9.5), crate::views::TXT);
    }
}

impl Tool for DevTests {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        self.xt_step(cx);
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal(|ui| {
            for (i, t) in TABS.iter().enumerate() {
                ui.selectable_value(&mut self.tab, i, *t);
            }
        });
        ui.separator();
        match self.tab {
            0 => self.xt_ui(ui, cx),
            1 => self.wf_ui(ui, cx),
            _ => self.mt_ui(ui, cx),
        }
    }

    fn status(&self) -> Option<(String, Color32)> {
        self.run.as_ref().map(|_| ("crosstalk test running".to_string(), Color32::from_rgb(255, 214, 10)))
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("tab", self.tab);
        o.put("xt_level", self.xt_level);
        o.put("xt_points", self.xt_points);
        o.put("wf_nominal", self.wf_nominal);
        o.put("mt_level", self.mt_level);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("tab", &mut self.tab);
        i.set("xt_level", &mut self.xt_level);
        i.set("xt_points", &mut self.xt_points);
        i.set("wf_nominal", &mut self.wf_nominal);
        i.set("mt_level", &mut self.mt_level);
        self.tab = self.tab.min(2);
        self.wf_nominal = self.wf_nominal.min(2);
        self.xt_level = self.xt_level.clamp(-60.0, 0.0);
        self.mt_level = self.mt_level.clamp(-60.0, 0.0);
        self.xt_points = self.xt_points.clamp(5, 120);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_level_and_wow() {
        let sr = 48000.0f32;
        let x: Vec<f32> = (0..XT_N).map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / sr).sin()).collect();
        assert!((tone(&x, 1000.0, sr).norm() - 0.5).abs() < 0.005);
        // 3150 Hz with 0.1 % peak of wow at 4 Hz (where the weighting is 0 dB): about 0.1 % weighted peak, 0.07 % RMS
        let mut ph = 0f64;
        let x: Vec<f32> = (0..(sr as usize * 9))
            .map(|i| {
                ph += std::f64::consts::TAU * 3150.0 * (1.0 + 0.001 * (std::f64::consts::TAU * 4.0 * i as f64 / sr as f64).sin()) / sr as f64;
                ph.sin() as f32 * 0.3
            })
            .collect();
        let o = wow_flutter(&x, sr, 3148.0).unwrap();
        assert!((o.f_mean - 3150.0).abs() < 0.2, "{}", o.f_mean);
        assert!((o.w_rms - 0.0707).abs() < 0.008, "{}", o.w_rms);
        assert!((o.w_peak - 0.1).abs() < 0.015, "{}", o.w_peak);
    }

    #[test]
    fn multitone_residual() {
        // three tones plus a product 60 dB under one of them
        let sr = 48000.0f32;
        let tones = [500.0f32, 1000.0, 2000.0];
        let x: Vec<f32> = (0..65536).map(|i| { let t = i as f32 / sr; tones.iter().map(|f| 0.1 * (std::f32::consts::TAU * f * t).sin()).sum::<f32>() + 0.0001 * (std::f32::consts::TAU * 3000.0 * t).sin() }).collect();
        let o = multitone(&x, sr, &tones);
        assert_eq!(o.found, 3);
        // 1e-8 against 3e-2 in power: -64.8 dB
        assert!((o.tdn_db + 64.8).abs() < 0.5, "{}", o.tdn_db);
    }
}
