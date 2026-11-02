//! Null test: two signals (input channels or files) are lined up in time and level, then one is subtracted from
//! the other. What is left is the difference between them, shown on a scope and a spectrum and playable, so it can
//! be heard as well as seen. The delay between the signals is found automatically and can be fine tuned by hand.

use super::dialogs::{self, Pending};
use super::monitor::{self, Queue};
use super::plot::{canvas_h, inset, readout_sized, Plot};
use super::*;
use crate::graph::dsp::{db_to_lin, Rs};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};
use parking_lot::Mutex;
use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

/// Half width of the fractional delay filter (windowed sinc).
const SINC_K: isize = 16;
const SPEC_N: usize = 8192;
const CURVE_N: usize = 700;
const COL_A: Color32 = Color32::from_rgb(90, 190, 255);
const COL_B: Color32 = Color32::from_rgb(255, 170, 70);
const COL_R: Color32 = Color32::from_rgb(255, 90, 100);

#[derive(Clone, PartialEq)]
enum Src {
    InL,
    InR,
    File,
}

impl Src {
    fn label(&self) -> &'static str {
        match self {
            Src::InL => "Input left",
            Src::InR => "Input right",
            Src::File => "File",
        }
    }
}

#[derive(Clone)]
struct Sig {
    name: String,
    sr: u32,
    x: Arc<Vec<f32>>,
}

#[derive(Default, Clone)]
struct Stats {
    /// residual RMS relative to the RMS of A, dB (more negative = better null)
    depth_db: f32,
    res_peak_db: f32,
    corr: f32,
    valid: usize,
}

struct Curves {
    f: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
    r: Vec<f32>,
}

type Loading = Arc<Mutex<Option<Result<(String, u32, Vec<f32>), String>>>>;

struct PlayShared {
    sigs: [Arc<Vec<f32>>; 3],
    mode: usize,
    boost: f32,
    pos: usize,
    sr: u32,
}

struct Player {
    sh: Arc<Mutex<PlayShared>>,
    stop: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
    dev: String,
    _h: monitor::Handle,
    status: Arc<Mutex<String>>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

impl Player {
    fn start(dev: &str, sh: PlayShared) -> Player {
        let q = Arc::new(Mutex::new(Queue { buf: Default::default(), src_sr: sh.sr }));
        let gain = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let status = Arc::new(Mutex::new(String::new()));
        let h = monitor::start(q.clone(), gain.clone(), Some(dev.to_string()).filter(|d| !d.is_empty()), status.clone());
        let sh = Arc::new(Mutex::new(sh));
        let stop = Arc::new(AtomicBool::new(false));
        let (sh2, stop2) = (sh.clone(), stop.clone());
        std::thread::Builder::new()
            .name("nulltest-play".into())
            .spawn(move || {
                let mut l = vec![0f32; 512];
                while !stop2.load(Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(8));
                    let mut q = q.lock();
                    let mut s = sh2.lock();
                    q.src_sr = s.sr;
                    let target = s.sr as usize / 4;
                    while q.buf.len() < target {
                        let sig = s.sigs[s.mode.min(2)].clone();
                        if sig.is_empty() {
                            break;
                        }
                        let boost = if s.mode == 2 { s.boost } else { 1.0 };
                        for v in l.iter_mut() {
                            *v = (sig[s.pos % sig.len()] * boost).clamp(-1.0, 1.0);
                            s.pos = (s.pos + 1) % sig.len();
                        }
                        q.push(&l, &l);
                    }
                }
            })
            .ok();
        Player { sh, stop, gain, dev: dev.to_string(), _h: h, status }
    }
}

pub struct NullTest {
    sa: Src,
    sb: Src,
    a: Option<Sig>,
    b: Option<Sig>,
    loading: Option<(bool, Loading)>,
    dlg: Option<(bool, Pending)>,
    secs: f32,
    auto: bool,
    /// B is delayed by this many samples (may be fractional or negative)
    delay: f64,
    gain_db: f32,
    invert: bool,
    adj: Arc<Vec<f32>>,
    res: Arc<Vec<f32>>,
    /// length of B on A's sample rate
    b_len: usize,
    sr: u32,
    stats: Option<Stats>,
    curves: Option<Curves>,
    spec_dirty: bool,
    view_start: f32,
    /// seconds shown; 0 = everything
    view_span: f32,
    mode: usize,
    boost_db: f32,
    vol_db: f32,
    out_dev: String,
    playing: bool,
    player: Option<Player>,
    msg: String,
}

impl NullTest {
    pub fn new() -> Self {
        Self {
            sa: Src::InL,
            sb: Src::InR,
            a: None,
            b: None,
            loading: None,
            dlg: None,
            secs: 4.0,
            auto: true,
            delay: 0.0,
            gain_db: 0.0,
            invert: false,
            adj: Arc::new(Vec::new()),
            res: Arc::new(Vec::new()),
            b_len: 0,
            sr: 48000,
            stats: None,
            curves: None,
            spec_dirty: false,
            view_start: 0.0,
            view_span: 0.0,
            mode: 2,
            boost_db: 0.0,
            vol_db: -12.0,
            out_dev: String::new(),
            playing: false,
            player: None,
            msg: String::new(),
        }
    }

    fn capture(&mut self, cx: &Ctx) {
        let sr = cx.sh.sr.load(Relaxed).max(8000);
        let n = (self.secs * sr as f32) as usize;
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        cx.sh.ring.lock().copy_last(&mut l, &mut r);
        let (l, r) = (Arc::new(l), Arc::new(r));
        let name = |s: &Src| if *s == Src::InL { "input left" } else { "input right" };
        if self.sa != Src::File {
            self.a = Some(Sig { name: name(&self.sa).into(), sr, x: if self.sa == Src::InL { l.clone() } else { r.clone() } });
        }
        if self.sb != Src::File {
            self.b = Some(Sig { name: name(&self.sb).into(), sr, x: if self.sb == Src::InL { l } else { r } });
        }
        self.after_new_data();
    }

    fn after_new_data(&mut self) {
        self.msg.clear();
        if self.a.is_none() || self.b.is_none() {
            return;
        }
        if self.auto {
            self.align();
        } else {
            self.recompute();
        }
    }

    /// B on A's sample rate.
    fn b_at_rate(&self) -> Option<Arc<Vec<f32>>> {
        let (a, b) = (self.a.as_ref()?, self.b.as_ref()?);
        if a.sr == b.sr {
            return Some(b.x.clone());
        }
        let mut rs = Rs::new(b.sr, a.sr);
        rs.push(&b.x, &b.x);
        let n = rs.available();
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        let k = rs.pull(&mut l, &mut r);
        l.truncate(k);
        Some(Arc::new(l))
    }

    /// Find the delay (cross-correlation), the polarity and the level match.
    fn align(&mut self) {
        let (Some(a), Some(b)) = (self.a.clone(), self.b_at_rate()) else { return };
        let max_lag = (a.x.len().min(b.len()) / 2).min(a.sr as usize * 2);
        match xcorr_lag(&a.x, &b, max_lag) {
            Some((lag, c)) => {
                self.delay = lag;
                self.invert = c < 0.0;
                self.gain_db = 0.0;
                // level: least squares fit of the (delayed, inverted) B to A
                self.sr = a.sr;
                let moved = delayed(&b, lag, a.x.len());
                let (mut ab, mut bb) = (0f64, 0f64);
                let (lo, hi) = valid_range(a.x.len(), b.len(), lag);
                for n in lo..hi {
                    let s = if self.invert { -1.0 } else { 1.0 };
                    ab += a.x[n] as f64 * moved[n] as f64 * s;
                    bb += moved[n] as f64 * moved[n] as f64;
                }
                if bb > 1e-18 && ab > 0.0 {
                    self.gain_db = (20.0 * (ab / bb).log10() as f32).clamp(-60.0, 60.0);
                }
                self.msg = format!("aligned: B is {:.3} ms behind A", lag / a.sr as f64 * 1000.0);
            }
            None => self.msg = "the signals are too short or silent to line up".into(),
        }
        self.recompute();
    }

    /// Delay, gain and polarity are applied to B; the residual and the readouts follow.
    fn recompute(&mut self) {
        let (Some(a), Some(b)) = (self.a.clone(), self.b_at_rate()) else { return };
        self.sr = a.sr;
        let mut moved = delayed(&b, self.delay, a.x.len());
        let g = db_to_lin(self.gain_db) * if self.invert { -1.0 } else { 1.0 };
        moved.iter_mut().for_each(|v| *v *= g);
        let res: Vec<f32> = a.x.iter().zip(moved.iter()).map(|(x, y)| x - y).collect();
        let (lo, hi) = valid_range(a.x.len(), b.len(), self.delay);
        let hi = hi.max(lo);
        let rms = |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt();
        let (ra, rr) = (rms(&a.x[lo..hi]), rms(&res[lo..hi]));
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for n in lo..hi {
            let (x, y) = (a.x[n] as f64, moved[n] as f64);
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
        self.stats = (hi > lo).then(|| Stats {
            depth_db: 20.0 * (rr.max(1e-12) / ra.max(1e-12)).log10() as f32,
            res_peak_db: 20.0 * res[lo..hi].iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9).log10(),
            corr: if aa > 0.0 && bb > 0.0 { (ab / (aa * bb).sqrt()) as f32 } else { 0.0 },
            valid: hi - lo,
        });
        self.b_len = b.len();
        self.adj = Arc::new(moved);
        self.res = Arc::new(res);
        self.spec_dirty = true;
        if let Some(p) = &self.player {
            let mut s = p.sh.lock();
            s.sigs = [a.x.clone(), self.adj.clone(), self.res.clone()];
            s.sr = self.sr;
        }
    }

    fn build_curves(&mut self) {
        self.spec_dirty = false;
        let Some(a) = self.a.as_ref() else { return };
        let (lo, hi) = valid_range(a.x.len(), self.b_len, self.delay);
        if hi <= lo + 1024 {
            self.curves = None;
            return;
        }
        let mut n = 1024;
        while n * 2 <= SPEC_N.min(hi - lo) {
            n *= 2;
        }
        let sig = [&a.x[lo..hi], &self.adj[lo..hi], &self.res[lo..hi]];
        let spectra: Vec<Vec<f32>> = sig.iter().map(|s| avg_spectrum(s, n)).collect();
        let nyq = a.sr as f32 * 0.5;
        let df = a.sr as f32 / n as f32;
        let (f0, f1) = (20.0f32, 20000.0f32.min(nyq * 0.98));
        let f: Vec<f32> = (0..CURVE_N).map(|i| f0 * (f1 / f0).powf(i as f32 / (CURVE_N - 1) as f32)).collect();
        let at = |s: &[f32], fr: f32| -> f32 {
            let b = fr / df;
            let k = (b.floor() as usize).min(s.len() - 2);
            let t = (b - k as f32).clamp(0.0, 1.0);
            s[k] * (1.0 - t) + s[k + 1] * t
        };
        let curve = |s: &[f32]| -> Vec<f32> { f.iter().map(|&x| 10.0 * at(s, x).max(1e-18).log10()).collect() };
        self.curves = Some(Curves { a: curve(&spectra[0]), b: curve(&spectra[1]), r: curve(&spectra[2]), f });
    }

    fn open_file(&mut self, is_b: bool, path: PathBuf) {
        let slot: Loading = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        std::thread::spawn(move || {
            let name = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let res = std::panic::catch_unwind(|| super::decode::read_all(&path)).unwrap_or_else(|_| Err("the decoder failed on this file".into())).map(|(sr, ch)| {
                // channels are mixed to one
                let len = ch.iter().map(|c| c.len()).max().unwrap_or(0).min(sr as usize * 60);
                let mono: Vec<f32> = (0..len).map(|i| ch.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / ch.len().max(1) as f32).collect();
                (name, sr, mono)
            });
            *s2.lock() = Some(res);
        });
        self.loading = Some((is_b, slot));
    }

    fn ensure_player(&mut self, dev: &str) {
        if self.player.as_ref().is_some_and(|p| p.dev != dev) {
            self.player = None;
        }
        if self.player.is_none() {
            let a = self.a.as_ref().map(|a| a.x.clone()).unwrap_or_default();
            self.player = Some(Player::start(dev, PlayShared { sigs: [a, self.adj.clone(), self.res.clone()], mode: self.mode, boost: db_to_lin(self.boost_db), pos: 0, sr: self.sr }));
        }
    }

    fn source_row(&mut self, ui: &mut Ui, cx: &Ctx, is_b: bool) {
        let _ = cx;
        let (label, col) = if is_b { ("B", COL_B) } else { ("A", COL_A) };
        ui.label(RichText::new(label).strong().color(col));
        let cur = if is_b { self.sb.clone() } else { self.sa.clone() };
        let mut pick: Option<Src> = None;
        egui::ComboBox::from_id_salt(("nt_src", is_b)).width(92.0).selected_text(cur.label()).show_ui(ui, |ui| {
            for s in [Src::InL, Src::InR, Src::File] {
                if ui.selectable_label(cur == s, s.label()).clicked() {
                    pick = Some(s);
                }
            }
        });
        if let Some(s) = pick {
            if is_b {
                self.sb = s.clone();
            } else {
                self.sa = s.clone();
            }
            if s == Src::File {
                self.dlg = Some((is_b, dialogs::open("Open audio file", "Audio", decode::EXTENSIONS)));
            }
        }
        let sig = if is_b { &self.b } else { &self.a };
        if cur == Src::File {
            if ui.small_button("Open...").clicked() {
                self.dlg = Some((is_b, dialogs::open("Open audio file", "Audio", decode::EXTENSIONS)));
            }
        }
        if let Some(s) = sig {
            ui.label(RichText::new(format!("{}  {:.1} s, {} Hz", s.name, s.x.len() as f32 / s.sr as f32, s.sr)).small().weak());
        } else if cur != Src::File {
            ui.label(RichText::new("press Capture").small().weak());
        }
    }
}

/// Samples of A where the delayed B has real data (the edges are left out of the readouts).
fn valid_range(len_a: usize, len_b: usize, delay: f64) -> (usize, usize) {
    let k = SINC_K as isize + 2;
    let d = delay.floor() as isize;
    let lo = (d + k).max(0) as usize;
    let hi = ((len_b as isize + d - k).max(0) as usize).min(len_a);
    (lo.min(len_a), hi)
}

/// `out[n] = b(n - d)` for fractional `d` (windowed sinc), zero outside `b`.
fn delayed(b: &[f32], d: f64, len: usize) -> Vec<f32> {
    let di = d.floor() as isize;
    let fr = (d - di as f64) as f32;
    let mut out = vec![0f32; len];
    if fr < 1e-4 {
        for (n, o) in out.iter_mut().enumerate() {
            let i = n as isize - di;
            if i >= 0 && (i as usize) < b.len() {
                *o = b[i as usize];
            }
        }
        return out;
    }
    // value at m - fr, with m = n - di
    let taps: Vec<f32> = (1 - SINC_K..=SINC_K)
        .map(|k| {
            let x = k as f32 + fr;
            let s = if x.abs() < 1e-6 { 1.0 } else { (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x) };
            let w = 0.42 + 0.5 * (std::f32::consts::PI * x / SINC_K as f32).cos() + 0.08 * (2.0 * std::f32::consts::PI * x / SINC_K as f32).cos();
            s * w
        })
        .collect();
    for (n, o) in out.iter_mut().enumerate() {
        let m = n as isize - di;
        let first = m + 1 - SINC_K;
        if first + 2 * SINC_K <= 0 || first >= b.len() as isize {
            continue;
        }
        let mut acc = 0f32;
        for (j, t) in taps.iter().enumerate() {
            let i = first + j as isize;
            if i >= 0 && (i as usize) < b.len() {
                acc += b[i as usize] * t;
            }
        }
        *o = acc;
    }
    out
}

/// Lag (samples, fractional) at which `b` delayed by it best matches `a`, and the sign of the correlation.
fn xcorr_lag(a: &[f32], b: &[f32], max_lag: usize) -> Option<(f64, f32)> {
    if a.len() < 256 || b.len() < 256 {
        return None;
    }
    let n = (a.len() + b.len()).next_power_of_two();
    let mut pl = FftPlanner::<f32>::new();
    let (fwd, inv) = (pl.plan_fft_forward(n), pl.plan_fft_inverse(n));
    let prep = |x: &[f32]| -> Vec<Complex32> {
        let mut v: Vec<Complex32> = x.iter().map(|s| Complex32::new(*s, 0.0)).collect();
        v.resize(n, Complex32::new(0.0, 0.0));
        fwd.process(&mut v);
        v
    };
    let (fa, fb) = (prep(a), prep(b));
    let mut c: Vec<Complex32> = fa.iter().zip(fb.iter()).map(|(x, y)| x * y.conj()).collect();
    inv.process(&mut c);
    let at = |k: isize| c[k.rem_euclid(n as isize) as usize].re;
    let max_lag = max_lag.min(n / 2 - 2) as isize;
    let (mut best, mut bk) = (0f32, 0isize);
    for k in -max_lag..=max_lag {
        let v = at(k).abs();
        if v > best {
            best = v;
            bk = k;
        }
    }
    if best <= 0.0 {
        return None;
    }
    // parabola through the peak and its neighbours
    let (y0, y1, y2) = (at(bk - 1).abs(), at(bk).abs(), at(bk + 1).abs());
    let den = y0 - 2.0 * y1 + y2;
    let off = if den.abs() > 1e-12 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
    Some((bk as f64 + off as f64, at(bk)))
}

/// Mean power spectrum (Hann, 50 % overlap) of a signal, `n / 2 + 1` bins, scaled so that a full-scale sine is about 0 dB.
fn avg_spectrum(x: &[f32], n: usize) -> Vec<f32> {
    let mut pl = FftPlanner::<f32>::new();
    let fft = pl.plan_fft_forward(n);
    let win: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).collect();
    let wsum: f32 = win.iter().sum();
    let mut acc = vec![0f32; n / 2 + 1];
    let hop = n / 2;
    let mut count = 0;
    let mut start = 0;
    let stride = if x.len() > n * 200 { x.len() / 200 } else { hop };
    while start + n <= x.len() {
        let mut v: Vec<Complex32> = (0..n).map(|i| Complex32::new(x[start + i] * win[i], 0.0)).collect();
        fft.process(&mut v);
        for k in 0..acc.len() {
            acc[k] += v[k].norm_sqr();
        }
        count += 1;
        start += stride;
    }
    let scale = 4.0 / (wsum * wsum) / count.max(1) as f32;
    acc.iter_mut().for_each(|v| *v *= scale);
    acc
}

fn fmt_db(v: f32) -> String {
    if v <= -120.0 { "-inf dB".into() } else { format!("{v:.1} dB") }
}

impl Tool for NullTest {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        let done = self.loading.as_ref().and_then(|(is_b, slot)| slot.lock().take().map(|r| (*is_b, r)));
        {
            if let Some((is_b, res)) = done {
                self.loading = None;
                match res {
                    Ok((name, sr, x)) => {
                        let sig = Some(Sig { name, sr, x: Arc::new(x) });
                        if is_b {
                            self.b = sig;
                        } else {
                            self.a = sig;
                        }
                        self.after_new_data();
                    }
                    Err(e) => self.msg = e,
                }
            }
        }
        if let Some((is_b, d)) = &self.dlg {
            if let Some(res) = d.take() {
                let is_b = *is_b;
                self.dlg = None;
                if let Some(p) = res {
                    self.open_file(is_b, p);
                }
            }
        }
        let _ = cx;
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        // the live signals are what the analysers read: the side bar's channels, or the analyzer node of the routing graph
        ui.horizontal_wrapped(|ui| {
            self.source_row(ui, cx, false);
            ui.separator();
            self.source_row(ui, cx, true);
            ui.separator();
            let live = self.sa != Src::File || self.sb != Src::File;
            if ui.add_enabled(live, egui::Button::new(format!("Capture {:.0} s", self.secs))).on_hover_text(if cx.graph_on { "Take the last seconds of what the Analyzer node receives (left and right channel: use Split / Merge nodes to put the two signals there)" } else { "Take the last seconds of the left and right input channel chosen in the side bar" }).clicked() {
                self.capture(cx);
            }
            ui.add(egui::DragValue::new(&mut self.secs).range(0.5..=20.0).speed(0.1).suffix(" s")).on_hover_text("Length of a capture");
        });
        ui.horizontal_wrapped(|ui| {
            let ready = self.a.is_some() && self.b.is_some();
            let mut ch = false;
            let rate = self.sr.max(1) as f64;
            ui.label(RichText::new("Delay of B").small().weak());
            ch |= ui.add_enabled(ready, egui::DragValue::new(&mut self.delay).speed(0.05).range(-96000.0..=96000.0).max_decimals(2).suffix(" smp")).on_hover_text("Samples B is delayed by to line up with A (negative: moved earlier). Fractions are interpolated").changed();
            let mut ms = (self.delay / rate * 1000.0) as f32;
            if ui.add_enabled(ready, egui::DragValue::new(&mut ms).speed(0.01).max_decimals(3).suffix(" ms")).changed() {
                self.delay = ms as f64 * rate / 1000.0;
                ch = true;
            }
            ui.label(RichText::new("Level").small().weak());
            ch |= ui.add_enabled(ready, egui::DragValue::new(&mut self.gain_db).speed(0.02).range(-60.0..=60.0).max_decimals(2).suffix(" dB")).changed();
            ch |= ui.add_enabled(ready, egui::Checkbox::new(&mut self.invert, "Invert B")).on_hover_text("Turn B upside down before subtracting (when the signals have opposite polarity)").changed();
            if ui.add_enabled(ready, egui::Button::new("Align")).on_hover_text("Find the delay, polarity and level difference automatically").clicked() {
                self.align();
            }
            ui.checkbox(&mut self.auto, "auto").on_hover_text("Align automatically after every capture or file");
            if ch {
                self.recompute();
            }
        });
        // listening
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Listen").small().weak());
            let was = self.mode;
            for (i, (n, c)) in [("A", COL_A), ("B", COL_B), ("A - B", COL_R)].into_iter().enumerate() {
                if ui.selectable_label(self.mode == i, RichText::new(n).color(c)).on_hover_text(["Signal A", "Signal B after delay, level and polarity", "The difference: silence means the signals are identical"][i]).clicked() {
                    self.mode = i;
                }
            }
            if self.mode != was {
                if let Some(p) = &self.player {
                    p.sh.lock().mode = self.mode;
                }
            }
            let ready = self.a.is_some() && self.b.is_some();
            if ui.add_enabled(ready, egui::Button::new(if self.playing { "Stop" } else { "Play" })).clicked() {
                self.playing = !self.playing;
                if !self.playing {
                    self.player = None;
                }
            }
            ui.add(egui::DragValue::new(&mut self.vol_db).range(-60.0..=0.0).suffix(" dB").speed(0.2)).on_hover_text("Volume");
            if self.mode == 2 {
                if ui.add(egui::DragValue::new(&mut self.boost_db).range(0.0..=60.0).prefix("boost ").suffix(" dB").speed(0.2)).on_hover_text("Amplify the difference to hear small errors (limited at full scale)").changed() {
                    if let Some(p) = &self.player {
                        p.sh.lock().boost = db_to_lin(self.boost_db);
                    }
                }
            }
            // the output device: the side-bar style picker, or the first Output node of the routing graph
            let dev = if cx.graph_on {
                crate::graph::bus().first_out.lock().clone()
            } else {
                egui::ComboBox::from_id_salt("nt_out").width(120.0).truncate().selected_text(cx.out_devs.iter().find(|d| d.id == self.out_dev).map(|d| d.label.clone()).or_else(|| cx.out_devs.first().map(|d| d.label.clone())).unwrap_or_else(|| "default output".into())).show_ui(ui, |ui| {
                    for (i, d) in cx.out_devs.iter().enumerate() {
                        let id = if i == 0 { String::new() } else { d.id.clone() };
                        if ui.selectable_label(self.out_dev == id, &d.label).clicked() {
                            self.out_dev = id;
                        }
                    }
                });
                self.out_dev.clone()
            };
            if cx.graph_on {
                ui.label(RichText::new("plays on the first Output node").small().weak());
            }
            if self.playing && ready {
                self.ensure_player(&dev);
                if let Some(p) = &self.player {
                    p.gain.store(db_to_lin(self.vol_db).to_bits(), Relaxed);
                    let st = p.status.lock().clone();
                    if !st.is_empty() {
                        ui.label(RichText::new(st).small().weak());
                    }
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
        });
        if !self.msg.is_empty() {
            ui.label(RichText::new(&self.msg).small().color(Color32::from_rgb(255, 190, 90)));
        }
        let Some(a) = self.a.clone().filter(|_| self.b.is_some()) else {
            ui.add_space(8.0);
            ui.label(RichText::new("Choose two signals: input channels (Capture takes the last seconds) or files.\nThey are lined up and subtracted: the quieter the difference, the more alike the signals are.\nExamples: a device against its own recording, a plug-in on and off, two converters, a lossy file against its original.").weak());
            return;
        };
        if self.spec_dirty && !ui.input(|i| i.pointer.any_down()) {
            self.build_curves();
        }
        if let Some(s) = &self.stats {
            ui.horizontal(|ui| {
                let col = if s.depth_db < -60.0 { Color32::from_rgb(110, 220, 130) } else if s.depth_db < -30.0 { Color32::from_rgb(230, 200, 90) } else { Color32::from_rgb(255, 120, 110) };
                readout_sized(ui, "null depth (residual vs A)", &fmt_db(s.depth_db), col, 20.0);
                ui.add_space(10.0);
                readout_sized(ui, "residual peak", &fmt_db(s.res_peak_db), Color32::from_gray(210), 20.0);
                ui.add_space(10.0);
                readout_sized(ui, "correlation", &format!("{:.4}", s.corr), Color32::from_gray(210), 20.0);
                ui.add_space(10.0);
                readout_sized(ui, "compared", &format!("{:.2} s", s.valid as f32 / a.sr as f32), Color32::from_gray(210), 20.0);
            });
        }
        let total = a.x.len() as f32 / a.sr as f32;
        let span = if self.view_span <= 0.0 { total } else { self.view_span.min(total) };
        self.view_start = self.view_start.clamp(0.0, (total - span).max(0.0));
        let avail = ui.available_height().max(180.0);
        let (h_wave, h_res) = (avail * 0.34, avail * 0.2);
        let pos_s = self.player.as_ref().map(|p| p.sh.lock().pos as f32 / a.sr as f32);
        // waveforms: A and the adjusted B on top of each other, then the difference on the same scale
        for (k, h) in [h_wave, h_res].into_iter().enumerate() {
            let (resp, painter, rect) = canvas_h(ui, h);
            let pr = inset(rect);
            if pr.width() < 40.0 || pr.height() < 20.0 {
                continue;
            }
            let peak = a.x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-4);
            let scale = (peak * 1.1).min(1.5);
            let plot = Plot::new(pr, self.view_start, self.view_start + span, -scale, scale, false);
            plot.grid_y(&painter, &|v| format!("{v:.2}"));
            plot.grid_x(&painter, &|t| format!("{t:.3}s"));
            plot.frame(&painter);
            let layers: Vec<(&Arc<Vec<f32>>, Color32)> = if k == 0 { vec![(&a.x, COL_A), (&self.adj, COL_B)] } else { vec![(&self.res, COL_R)] };
            for (sig, col) in layers {
                draw_wave(&painter, &plot, sig, a.sr, col);
            }
            painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, if k == 0 { "A  B" } else { "A-B" }, FontId::proportional(9.0), crate::views::TXT);
            if let Some(t) = pos_s.filter(|t| *t >= self.view_start && *t <= self.view_start + span) {
                let x = plot.tx(t);
                painter.line_segment([Pos2::new(x, pr.top()), Pos2::new(x, pr.bottom())], Stroke::new(1.0, Color32::from_white_alpha(120)));
            }
            // wheel zooms around the pointer, dragging pans
            if resp.hovered() {
                let sc = ui.input(|i| i.smooth_scroll_delta.y);
                if sc != 0.0 {
                    if let Some(p) = resp.hover_pos() {
                        let t = plot.fx(p.x);
                        let ns = (span * (-sc * 0.003).exp()).clamp(0.002, total);
                        self.view_span = if ns >= total * 0.999 { 0.0 } else { ns };
                        let frac = (t - self.view_start) / span.max(1e-6);
                        self.view_start = (t - frac * ns).clamp(0.0, (total - ns).max(0.0));
                        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                    }
                }
                if let Some(p) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                    let t = plot.fx(p.x);
                    let i = ((t * a.sr as f32) as usize).min(a.x.len().saturating_sub(1));
                    let txt = if k == 0 { format!("{t:.4} s   A {:.4}   B {:.4}", a.x[i], self.adj.get(i).copied().unwrap_or(0.0)) } else { format!("{t:.4} s   A-B {:.5}", self.res.get(i).copied().unwrap_or(0.0)) };
                    plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
                }
            }
            if resp.dragged() {
                let dx = ui.input(|i| i.pointer.delta().x);
                self.view_start = (self.view_start - dx / pr.width().max(1.0) * span).clamp(0.0, (total - span).max(0.0));
            }
            if resp.double_clicked() {
                self.view_span = 0.0;
            }
        }
        // spectrum of A, B (adjusted) and the difference
        let h = ui.available_height().max(100.0);
        let (_, painter, rect) = canvas_h(ui, h);
        let pr = inset(rect);
        if let (Some(c), true) = (&self.curves, pr.width() > 40.0 && pr.height() > 20.0) {
            let plot = Plot::new(pr, c.f[0], *c.f.last().unwrap(), -140.0, 0.0, true);
            plot.grid_y(&painter, &|v| format!("{v:.0}"));
            plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
            plot.frame(&painter);
            for (v, col) in [(&c.a, COL_A), (&c.b, COL_B), (&c.r, COL_R)] {
                plot.line(&painter, c.f.iter().copied().zip(v.iter().copied()), Stroke::new(1.3, col));
            }
            painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
            painter.text(Pos2::new(pr.right() - 6.0, pr.bottom() - 6.0), Align2::RIGHT_BOTTOM, "where the difference is close to the signals, they do not cancel", FontId::proportional(9.5), Color32::from_white_alpha(90));
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("secs", self.secs);
        o.put("auto", self.auto);
        o.put("boost", self.boost_db);
        o.put("vol", self.vol_db);
        o.put("out", &self.out_dev);
        o.put("a_in", if self.sa == Src::InR { 1 } else { 0 });
        o.put("b_in", if self.sb == Src::InL { 0 } else { 1 });
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("secs", &mut self.secs);
        i.set("auto", &mut self.auto);
        i.set("boost", &mut self.boost_db);
        i.set("vol", &mut self.vol_db);
        if let Some(d) = i.str("out") {
            self.out_dev = d;
        }
        self.secs = if self.secs.is_finite() { self.secs.clamp(0.5, 20.0) } else { 4.0 };
        self.boost_db = if self.boost_db.is_finite() { self.boost_db.clamp(0.0, 60.0) } else { 0.0 };
        self.vol_db = if self.vol_db.is_finite() { self.vol_db.clamp(-60.0, 0.0) } else { -12.0 };
        self.sa = if i.get::<u8>("a_in") == Some(1) { Src::InR } else { Src::InL };
        self.sb = if i.get::<u8>("b_in") == Some(0) { Src::InL } else { Src::InR };
    }
}

/// Waveform in a plot: lines when zoomed in, a min / max envelope per pixel column otherwise.
fn draw_wave(p: &egui::Painter, plot: &Plot, x: &[f32], sr: u32, col: Color32) {
    let (t0, t1) = (plot.x0, plot.x1);
    let (i0, i1) = (((t0 * sr as f32) as usize).min(x.len()), ((t1 * sr as f32).ceil() as usize).min(x.len()));
    if i1 <= i0 + 1 {
        return;
    }
    let cols = plot.r.width().max(1.0) as usize;
    let per = (i1 - i0) as f32 / cols as f32;
    if per <= 2.0 {
        plot.line(p, (i0..i1).map(|i| (i as f32 / sr as f32, x[i])), Stroke::new(1.2, col));
    } else {
        let clip = p.with_clip_rect(plot.r);
        for c in 0..cols {
            let (a, b) = (i0 + (c as f32 * per) as usize, (i0 + ((c + 1) as f32 * per) as usize + 1).min(i1));
            if a >= b {
                continue;
            }
            let (mn, mx) = x[a..b].iter().fold((f32::MAX, f32::MIN), |m, v| (m.0.min(*v), m.1.max(*v)));
            let px = plot.r.left() + c as f32 + 0.5;
            clip.line_segment([Pos2::new(px, plot.ty(mx)), Pos2::new(px, plot.ty(mn).max(plot.ty(mx) + 1.0))], Stroke::new(1.0, col.gamma_multiply(0.85)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                s as f32 / u32::MAX as f32 - 0.5
            })
            .collect()
    }

    /// A band limited signal so that fractional delays are exact (noise through a low-pass).
    fn smooth(n: usize) -> Vec<f32> {
        // four one-pole low-passes in a row: nothing above a few percent of the sample rate is left
        let mut y = noise(n, 99);
        for _ in 0..4 {
            let mut s = 0f32;
            for v in y.iter_mut() {
                s += 0.08 * (*v - s);
                *v = s;
            }
        }
        y
    }

    #[test]
    fn the_lag_is_found_with_sign_and_fraction_and_the_null_is_deep() {
        let a = smooth(48000);
        // B is A delayed by 37.4 samples, scaled and inverted
        let b: Vec<f32> = delayed(&a, 37.4, a.len()).iter().map(|v| -0.5 * v).collect();
        let (lag, c) = xcorr_lag(&a, &b, 2000).unwrap();
        // xcorr_lag(x, y) answers: y delayed by lag matches x. Here A matches B advanced by 37.4 -> lag = -37.4, negative polarity
        assert!((lag + 37.4).abs() < 0.15, "{lag}");
        assert!(c < 0.0);
        let (lag2, c2) = xcorr_lag(&b, &a, 2000).unwrap();
        assert!((lag2 - 37.4).abs() < 0.15, "{lag2}");
        assert!(c2 < 0.0);
        // apply what was found: the residual is far below the signal
        let moved = delayed(&a, lag2 + 0.0, a.len());
        let (lo, hi) = valid_range(a.len(), a.len(), lag2);
        let g = -0.5f32;
        let (mut num, mut den) = (0f64, 0f64);
        for n in lo..hi {
            let r = b[n] - g * moved[n];
            num += (r as f64).powi(2);
            den += (b[n] as f64).powi(2);
        }
        let depth = 10.0 * (num / den).log10();
        assert!(depth < -50.0, "null depth {depth} dB");
    }

    #[test]
    fn identical_signals_cancel_completely_and_different_ones_do_not() {
        let a = smooth(20000);
        let same = delayed(&a, 0.0, a.len());
        assert_eq!(same, a);
        let (lo, hi) = valid_range(a.len(), a.len(), 0.0);
        assert!(lo > 0 && hi < a.len());
        let other = noise(20000, 5);
        let (l, c) = xcorr_lag(&a, &other, 1000).unwrap();
        assert!(l.abs() <= 1000.0 && c.is_finite());
    }

    #[test]
    fn short_or_empty_input_does_not_panic() {
        assert!(xcorr_lag(&[], &[], 10).is_none());
        assert!(xcorr_lag(&[0.0; 300], &[0.0; 300], 10).is_none());
        let d = delayed(&[1.0, 2.0], 5.5, 4);
        assert_eq!(d.len(), 4);
        let d = delayed(&[], -3.2, 4);
        assert!(d.iter().all(|v| *v == 0.0));
        let (lo, hi) = valid_range(10, 3, 100.0);
        assert!(lo <= 10 && hi <= 10);
        let s = avg_spectrum(&[0.0; 100], 1024);
        assert_eq!(s.len(), 513);
    }
}
