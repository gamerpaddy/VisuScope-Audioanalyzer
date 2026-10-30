//! Exponential-sweep measurement engine shared by the Response and Impulse windows.
//!
//! The sweep is rendered inside the generator's output callback (`next_sample`) while the input ring
//! records the result; deconvolution with the analytic inverse filter gives the impulse response, and
//! harmonic distortion lands *before* the main peak where the window cuts it away.

use super::*;
use parking_lot::Mutex;
use rustfft::{num_complex::Complex, FftPlanner};
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

type C64 = Complex<f64>;

// ------------------------------------------------------------------ playback side

pub struct PlayJob {
    f0: f64,
    f1: f64,
    secs: f64,
    amp: f32,
    pos: u64,
}

pub static PLAYER: Mutex<Option<PlayJob>> = Mutex::new(None);
pub static PLAY_ACTIVE: AtomicBool = AtomicBool::new(false);

const FADE_S: f64 = 0.01;

/// Sample of the sweep at time `t` (seconds), amplitude 1.
fn ess(t: f64, f0: f64, f1: f64, secs: f64) -> f64 {
    let l = (f1 / f0).ln();
    let ph = TAU * f0 * secs / l * ((t * l / secs).exp() - 1.0);
    let fade = if t < FADE_S {
        (std::f64::consts::FRAC_PI_2 * t / FADE_S).sin().powi(2)
    } else if t > secs - FADE_S {
        (std::f64::consts::FRAC_PI_2 * (secs - t).max(0.0) / FADE_S).sin().powi(2)
    } else {
        1.0
    };
    fade * ph.sin()
}

/// Called by the generator for every output frame. Returns the sweep sample (0 when idle).
pub fn next_sample(slot: &mut Option<PlayJob>, sr: f64) -> f32 {
    let Some(j) = slot.as_mut() else { return 0.0 };
    let t = j.pos as f64 / sr;
    if t >= j.secs {
        *slot = None;
        PLAY_ACTIVE.store(false, Relaxed);
        return 0.0;
    }
    j.pos += 1;
    (ess(t, j.f0, j.f1, j.secs) as f32) * j.amp
}

// ------------------------------------------------------------------ parameters / results

#[derive(Clone)]
pub struct SweepParams {
    pub f0: f32,
    pub f1: f32,
    pub secs: f32,
    pub amp_db: f32,
    pub tail: f32,
    pub averages: usize,
    pub ir_ms: f32,
    pub resp_ms: f32,
    /// R input carries the reference (loopback of the output); L is the device under test
    pub loopback: bool,
    pub dut_ch: usize,
}

impl Default for SweepParams {
    fn default() -> Self {
        Self { f0: 20.0, f1: 20000.0, secs: 3.0, amp_db: -12.0, tail: 1.5, averages: 1, ir_ms: 1500.0, resp_ms: 250.0, loopback: false, dut_ch: 0 }
    }
}

pub struct Transfer {
    pub df: f32,
    pub mag_db: Vec<f32>,
    /// unwrapped radians
    pub phase: Vec<f32>,
    pub coh: Option<Vec<f32>>,
}

pub struct SweepResult {
    pub sr: f32,
    pub ir: Vec<f32>,
    /// samples in the window before the IR peak
    pub pre: usize,
    pub h: Transfer,
    pub runs: usize,
    pub capture_peak_db: f32,
    pub params: SweepParams,
    /// bumped for every new result so views can invalidate caches
    pub id: u64,
}

pub struct RunOut {
    ir: Vec<f32>,
    pre: usize,
    y: Vec<C64>,
    x: Vec<C64>,
    peak_db: f32,
}

#[derive(Clone, PartialEq)]
pub enum State {
    Idle,
    Arming(Instant),
    Capturing { t0: u64, need: u64, since: Instant, run: usize },
    Computing { run: usize },
    Done,
    Failed(String),
}

struct Acc {
    ir: Vec<f32>,
    a: Vec<C64>,
    b: Vec<f64>,
    c: Vec<f64>,
    n: usize,
    pre: usize,
    peak_db: f32,
}

pub struct Sweep {
    pub p: SweepParams,
    pub state: State,
    pub result: Option<SweepResult>,
    slot: Arc<Mutex<Option<Result<RunOut, String>>>>,
    acc: Option<Acc>,
    next_id: u64,
}

impl Sweep {
    pub fn new() -> Self {
        Self { p: SweepParams::default(), state: State::Idle, result: None, slot: Arc::new(Mutex::new(None)), acc: None, next_id: 1 }
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Arming(_) | State::Capturing { .. } | State::Computing { .. })
    }

    pub fn start(&mut self) {
        if self.busy() {
            return;
        }
        self.acc = None;
        self.state = State::Arming(Instant::now());
    }

    pub fn cancel(&mut self) {
        *PLAYER.lock() = None;
        PLAY_ACTIVE.store(false, Relaxed);
        self.acc = None;
        self.state = State::Idle;
    }

    /// 0..1 progress of the running measurement.
    pub fn progress(&self) -> f32 {
        let runs = self.p.averages.max(1) as f32;
        match &self.state {
            State::Capturing { since, run, .. } => ((*run as f32 + (since.elapsed().as_secs_f32() / (self.p.secs + self.p.tail)).min(1.0)) / runs).min(1.0),
            State::Computing { run } => (*run as f32 + 1.0) / runs,
            State::Done => 1.0,
            _ => 0.0,
        }
    }

    fn post_job(&mut self, sh: &Shared, run: usize) {
        let sr = sh.sr.load(Relaxed) as f32;
        let secs = self.p.secs as f64;
        let need = ((self.p.secs + self.p.tail) * sr) as u64;
        if need as usize + 16384 > RING_CAP {
            self.state = State::Failed("capture too long for the sample buffer at this rate: reduce sweep time or tail".into());
            return;
        }
        let t0 = sh.ring.lock().total;
        *PLAYER.lock() = Some(PlayJob { f0: self.p.f0 as f64, f1: (self.p.f1.min(sr * 0.5 * 0.98)) as f64, secs, amp: 10f32.powf(self.p.amp_db / 20.0), pos: 0 });
        PLAY_ACTIVE.store(true, Relaxed);
        self.state = State::Capturing { t0, need, since: Instant::now(), run };
    }

    /// Advance the state machine; called every frame.
    pub fn tick(&mut self, sh: &Shared, gen_running: bool, req: &mut Requests) {
        let sr = sh.sr.load(Relaxed) as f32;
        match self.state.clone() {
            State::Arming(t) => {
                if sr <= 0.0 {
                    self.state = State::Failed("no input running".into());
                } else if !gen_running {
                    req.start_gen = true;
                    // wait for the output stream to come up
                    if t.elapsed() > Duration::from_secs(6) {
                        self.state = State::Failed("generator output did not start".into());
                    }
                } else if t.elapsed() > Duration::from_millis(900) {
                    self.post_job(sh, 0);
                }
            }
            State::Capturing { t0, need, since, run } => {
                let total = sh.ring.lock().total;
                if total < t0 {
                    self.cancel();
                    self.state = State::Failed("input restarted during the measurement".into());
                    return;
                }
                if total >= t0 + need {
                    let n = need as usize;
                    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
                    sh.ring.lock().copy_abs(t0 as usize, &mut l, &mut r);
                    let (p, slot) = (self.p.clone(), self.slot.clone());
                    self.state = State::Computing { run };
                    std::thread::Builder::new()
                        .name("sweep-fft".into())
                        .spawn(move || {
                            let out = process_run(&l, &r, sr, &p);
                            *slot.lock() = Some(out);
                        })
                        .ok();
                } else if since.elapsed() > Duration::from_secs_f32((self.p.secs + self.p.tail) * 3.0 + 5.0) {
                    self.cancel();
                    self.state = State::Failed("capture timed out (input stalled?)".into());
                }
            }
            State::Computing { run } => {
                let Some(res) = self.slot.try_lock().and_then(|mut g| g.take()) else { return };
                match res {
                    Err(e) => {
                        self.acc = None;
                        self.state = State::Failed(e);
                    }
                    Ok(out) => {
                        self.accumulate(out);
                        if run + 1 < self.p.averages.max(1) {
                            self.post_job(sh, run + 1);
                        } else {
                            self.finish(sr);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn accumulate(&mut self, o: RunOut) {
        match &mut self.acc {
            None => {
                let (b, c) = (o.x.iter().map(|v| v.norm_sqr()).collect(), o.y.iter().map(|v| v.norm_sqr()).collect());
                let a = o.y.iter().zip(&o.x).map(|(y, x)| y * x.conj()).collect();
                self.acc = Some(Acc { ir: o.ir, a, b, c, n: 1, pre: o.pre, peak_db: o.peak_db });
            }
            Some(acc) => {
                for (s, v) in acc.ir.iter_mut().zip(&o.ir) {
                    *s += v;
                }
                for i in 0..acc.a.len().min(o.y.len()) {
                    acc.a[i] += o.y[i] * o.x[i].conj();
                    acc.b[i] += o.x[i].norm_sqr();
                    acc.c[i] += o.y[i].norm_sqr();
                }
                acc.n += 1;
                acc.peak_db = acc.peak_db.max(o.peak_db);
            }
        }
    }

    fn finish(&mut self, sr: f32) {
        let Some(acc) = self.acc.take() else { return };
        let n = acc.n as f32;
        let m = (acc.a.len() - 1) * 2;
        let df = sr / m as f32;
        let mut mag = Vec::with_capacity(acc.a.len());
        let mut phase = Vec::with_capacity(acc.a.len());
        let mut coh = Vec::with_capacity(acc.a.len());
        let mut prev = 0f32;
        let mut off = 0f32;
        for i in 0..acc.a.len() {
            let h = acc.a[i] / acc.b[i].max(1e-30);
            mag.push(20.0 * (h.norm() as f32).max(1e-9).log10());
            let ph = h.arg() as f32;
            if i > 0 {
                let d = ph + off - prev;
                if d > std::f32::consts::PI {
                    off -= TAU as f32;
                } else if d < -std::f32::consts::PI {
                    off += TAU as f32;
                }
            }
            prev = ph + off;
            phase.push(prev);
            coh.push((acc.a[i].norm_sqr() / (acc.b[i] * acc.c[i]).max(1e-30)).min(1.0) as f32);
        }
        let ir: Vec<f32> = acc.ir.iter().map(|v| v / n).collect();
        let id = self.next_id;
        self.next_id += 1;
        self.result = Some(SweepResult {
            sr,
            ir,
            pre: acc.pre,
            h: Transfer { df, mag_db: mag, phase, coh: (acc.n >= 2).then_some(coh) },
            runs: acc.n,
            capture_peak_db: acc.peak_db,
            params: self.p.clone(),
            id,
        });
        self.state = State::Done;
    }
}

// ------------------------------------------------------------------ deconvolution

fn fft(planner: &mut FftPlanner<f64>, data: &[f32], n: usize) -> Vec<C64> {
    let mut b: Vec<C64> = data.iter().map(|&v| C64::new(v as f64, 0.0)).collect();
    b.resize(n, C64::new(0.0, 0.0));
    planner.plan_fft_forward(n).process(&mut b);
    b
}

fn ifft(planner: &mut FftPlanner<f64>, mut b: Vec<C64>) -> Vec<f64> {
    let n = b.len();
    planner.plan_fft_inverse(n).process(&mut b);
    b.iter().map(|v| v.re / n as f64).collect()
}

/// Run one captured sweep through deconvolution; `l`/`r` are the two input channels.
pub fn process_run(l: &[f32], r: &[f32], sr: f32, p: &SweepParams) -> Result<RunOut, String> {
    let srf = sr as f64;
    let secs = p.secs as f64;
    let f1 = (p.f1 as f64).min(srf * 0.5 * 0.98);
    let ns = (secs * srf).round() as usize;
    let ncap = l.len();
    if ncap < ns + 64 {
        return Err("capture shorter than the sweep".into());
    }
    let peak = l.iter().chain(r).fold(0f32, |m, v| m.max(v.abs()));
    let peak_db = 20.0 * peak.max(1e-9).log10();
    if peak_db < -90.0 {
        return Err("nothing was captured: route the generator output to the input (cable or [Loopback] device)".into());
    }
    let nfft = (ncap + ns).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let lr = (f1 / p.f0 as f64).ln();
    let x: Vec<f32> = (0..ns).map(|i| ess(i as f64 / srf, p.f0 as f64, f1, secs) as f32).collect();
    let inv: Vec<f32> = (0..ns).map(|i| x[ns - 1 - i] * (-(i as f64 / srf) * lr / secs).exp() as f32).collect();
    let spec_inv = fft(&mut planner, &inv, nfft);
    // Scale so that an ideal system reads 0 dB: the sweep times its inverse filter has a flat in-band
    // spectrum; its level (not the band-limited peak) is the reference.
    let spec_x = fft(&mut planner, &x, nfft);
    let bin = |f: f64| (f / srf * nfft as f64) as usize;
    let (k0, k1) = (bin(2.0 * p.f0 as f64), bin(f1 / 2.0).min(nfft / 2));
    let level = (k0..k1.max(k0 + 1)).map(|k| (spec_x[k] * spec_inv[k]).norm()).sum::<f64>() / (k1.max(k0 + 1) - k0) as f64;
    let norm = level.max(1e-30) * 10f64.powf(p.amp_db as f64 / 20.0);
    let mut deconv = |cap: &[f32]| -> Vec<f32> {
        let s = fft(&mut planner, cap, nfft);
        ifft(&mut planner, s.iter().zip(&spec_inv).map(|(a, b)| a * b).collect()).iter().map(|v| (v / norm) as f32).collect()
    };
    let (dut_in, ref_in) = if p.loopback { (l, Some(r)) } else if p.dut_ch == 0 { (l, None) } else { (r, None) };
    let h_dut = deconv(dut_in);
    let h_ref = ref_in.map(&mut deconv);
    // main peak: search where a causal response can be
    let lead = if let Some(h) = &h_ref { h } else { &h_dut };
    let (a, b) = (ns - 1, ncap.min(nfft) - 1);
    let (mut pk, mut best) = (a, 0f32);
    for i in a..=b {
        if lead[i].abs() > best {
            best = lead[i].abs();
            pk = i;
        }
    }
    // keep the band-limited pre-ringing (a few periods of the lowest frequency) but stay clear of the
    // second-harmonic response, which sits secs*ln(2)/ln(f1/f0) before the main peak
    let pre_s = (2.0 / p.f0 as f64).clamp(0.002, 0.5 * secs * std::f64::consts::LN_2 / lr);
    let pre = ((pre_s * srf) as usize).min(pk);
    let start = pk - pre;
    let ir_len = ((p.ir_ms as f64 * 1e-3 * srf) as usize).clamp(256, nfft - start);
    let cut = |h: &[f32], len: usize| -> Vec<f32> { (0..len).map(|k| h.get(start + k).copied().unwrap_or(0.0)).collect() };
    let ir = cut(&h_dut, ir_len);
    // frequency response window with a Hann tail
    let rl = (((p.resp_ms as f64 * 1e-3 * srf) as usize).clamp(256, ir_len)).max(256);
    let taper = |mut v: Vec<f32>| {
        let t0 = rl * 3 / 4;
        for (k, s) in v.iter_mut().enumerate().skip(t0) {
            let u = (k - t0) as f32 / (rl - t0).max(1) as f32;
            *s *= 0.5 * (1.0 + (std::f32::consts::PI * u).cos());
        }
        v
    };
    let m = (rl * 2).next_power_of_two().max(16384);
    let shift = |mut s: Vec<C64>| -> Vec<C64> {
        // move the peak to t = 0
        for (k, v) in s.iter_mut().enumerate() {
            *v *= C64::from_polar(1.0, TAU * k as f64 / m as f64 * pre as f64);
        }
        s.truncate(m / 2 + 1);
        s
    };
    let y = shift(fft(&mut planner, &taper(cut(&h_dut, rl)), m));
    let x = match &h_ref {
        Some(hr) => shift(fft(&mut planner, &taper(cut(hr, rl)), m)),
        None => vec![C64::new(1.0, 0.0); m / 2 + 1],
    };
    Ok(RunOut { ir, pre, y, x, peak_db })
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic device: 3rd-order nonlinearity, then gain, a 0.5 one-pole low-pass and a delay.
    fn device(play: &[f32], delay: usize, gain: f32, extra: usize) -> Vec<f32> {
        let mut out = vec![0f32; play.len() + delay + extra];
        let mut y = 0f32;
        for (i, &v) in play.iter().enumerate() {
            let u = v + 0.05 * v * v * v;
            y = 0.5 * u + 0.5 * y;
            out[i + delay] = gain * y;
        }
        for i in play.len()..out.len() - delay {
            y *= 0.5;
            out[i + delay] = gain * y;
        }
        out
    }

    #[test]
    fn sweep_recovers_response_and_rejects_harmonics() {
        let sr = 48000.0;
        let p = SweepParams { secs: 2.0, tail: 1.0, amp_db: -6.0, ..Default::default() };
        let amp = 10f32.powf(p.amp_db / 20.0);
        let n = (p.secs as f64 * sr as f64) as usize;
        let play: Vec<f32> = (0..n).map(|i| ess(i as f64 / sr as f64, 20.0, 20000.0, 2.0) as f32 * amp).collect();
        let cap = device(&play, 240, 0.8, (sr * 1.0) as usize);
        let silent = vec![0f32; cap.len()];
        let out = process_run(&cap, &silent, sr, &p).expect("process");
        // |H| = g * 0.5 / |1 - 0.5 e^-jw| for the one-pole low-pass
        let m = (out.y.len() - 1) * 2;
        let mut errs = Vec::new();
        for f in [60.0, 100.0, 300.0, 1000.0, 5000.0, 12000.0] {
            let w = TAU * f / sr as f64;
            let expect = 0.8 * 0.5 / ((1.0 - 0.5 * w.cos()).powi(2) + (0.5 * w.sin()).powi(2)).sqrt();
            let k = (f / sr as f64 * m as f64).round() as usize;
            errs.push((f, 20.0 * (out.y[k].norm() / expect).log10()));
        }
        // the cubic term adds ~0.08 dB of linear gain at this level
        assert!(errs.iter().all(|e| e.1.abs() < 0.35), "magnitude errors in dB: {errs:?}");
        // phase at 1 kHz: -atan(0.5 sin w / (1 - 0.5 cos w)), peak moved to t = 0
        let w = TAU * 1000.0 / sr as f64;
        let k = (1000.0 / sr as f64 * m as f64).round() as usize;
        let want = -(0.5 * w.sin()).atan2(1.0 - 0.5 * w.cos());
        let got = out.y[k].arg();
        assert!((got - want).abs() < 0.08, "phase {got} want {want}");
    }
}
