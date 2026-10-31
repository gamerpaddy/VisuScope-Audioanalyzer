//! Live dual-channel transfer function (H1 estimator) on the DSP thread.
//!
//! One channel carries the reference (what goes into the system), the other what comes out. The
//! averaged cross-spectrum gives magnitude, phase and coherence while any programme material plays.
//! The delay between the channels is found by GCC-PHAT and removed before the FFT, so latency does not
//! smear the estimate.

use crate::dsp::{make_window, Window};
use crate::shared::*;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct TfEngine {
    n: usize,
    sr: u32,
    win: Window,
    fft: Option<Arc<dyn Fft<f32>>>,
    ifft: Option<Arc<dyn Fft<f32>>>,
    scratch: Vec<Complex32>,
    buf: Vec<Complex32>,
    cc: Vec<Complex32>,
    w: Vec<f32>,
    l: Vec<f32>,
    r: Vec<f32>,
    gxx: Vec<f32>,
    gyy: Vec<f32>,
    gxy: Vec<Complex32>,
    cnt: u32,
    last_total: u64,
    reset: u32,
    /// delay of the measured channel behind the reference, samples (fractional)
    delay: f32,
    est_t: Instant,
    est_every: Duration,
}

impl TfEngine {
    pub fn new() -> Self {
        Self {
            n: 0,
            sr: 0,
            win: Window::Hann,
            fft: None,
            ifft: None,
            scratch: Vec::new(),
            buf: Vec::new(),
            cc: Vec::new(),
            w: Vec::new(),
            l: Vec::new(),
            r: Vec::new(),
            gxx: Vec::new(),
            gyy: Vec::new(),
            gxy: Vec::new(),
            cnt: 0,
            last_total: 0,
            reset: 0,
            delay: 0.0,
            est_t: Instant::now(),
            est_every: Duration::from_millis(400),
        }
    }

    fn rebuild(&mut self, planner: &mut FftPlanner<f32>, n: usize, win: Window, sr: u32) {
        let (f, i) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
        self.scratch = vec![Complex32::new(0.0, 0.0); f.get_inplace_scratch_len().max(i.get_inplace_scratch_len())];
        self.fft = Some(f);
        self.ifft = Some(i);
        self.n = n;
        self.sr = sr;
        self.win = win;
        self.buf = vec![Complex32::new(0.0, 0.0); n];
        self.cc = vec![Complex32::new(0.0, 0.0); n];
        self.w = make_window(win, n);
        self.l = vec![0.0; n];
        self.r = vec![0.0; n];
        self.clear_avg();
    }

    fn clear_avg(&mut self) {
        let h = self.n / 2 + 1;
        self.gxx = vec![0.0; h];
        self.gyy = vec![0.0; h];
        self.gxy = vec![Complex32::new(0.0, 0.0); h];
        self.cnt = 0;
    }

    /// Called once per DSP loop; does work only when enough new samples have arrived (75 % overlap).
    pub fn step(&mut self, sh: &Shared, p: &Params, sr: u32, planner: &mut FftPlanner<f32>, total: u64) {
        let tp = sh.tf_params.lock().clone();
        if !tp.on {
            return;
        }
        let n = p.fft_size;
        if self.n != n || self.win != p.window || self.sr != sr {
            self.rebuild(planner, n, p.window, sr);
        }
        if self.reset != tp.reset {
            self.reset = tp.reset;
            self.clear_avg();
            if tp.auto_delay {
                self.delay = 0.0;
            }
        }
        if total < self.last_total {
            self.last_total = total;
            self.clear_avg();
        }
        if total < self.last_total + (n / 4) as u64 || (total as usize) < n {
            return;
        }
        self.last_total = total;

        if !tp.auto_delay {
            let d = tp.delay_ms * 1e-3 * sr as f32;
            if (d - self.delay).abs() > 1e-3 {
                self.delay = d;
                self.clear_avg();
            }
        }
        // measured channel lags the reference by `d`: look further back in the reference
        let d = self.delay.round() as isize;
        let (shift_ref, shift_meas) = if d >= 0 { (d as usize, 0) } else { (0, (-d) as usize) };
        let (sl, sr_) = if tp.ref_r { (shift_meas, shift_ref) } else { (shift_ref, shift_meas) };
        {
            let ring = sh.ring.lock();
            ring.copy_last_shifted(&mut self.l, &mut self.r, sl, sr_);
        }
        for i in 0..n {
            let w = self.w[i];
            self.buf[i] = Complex32::new(self.l[i] * w, self.r[i] * w);
        }
        let fft = self.fft.clone().unwrap();
        fft.process_with_scratch(&mut self.buf, &mut self.scratch);

        let half = n / 2;
        let mask = n - 1;
        let alpha = (1.0 / p.avg_count.max(1.0)).max(1.0 / (self.cnt as f32 + 1.0));
        self.cnt = self.cnt.saturating_add(1);
        for k in 0..=half {
            let zk = self.buf[k];
            let zn = self.buf[(n - k) & mask].conj();
            let xl = (zk + zn) * 0.5;
            let dd = zk - zn;
            let xr = Complex32::new(dd.im, -dd.re) * 0.5;
            let (x, y) = if tp.ref_r { (xr, xl) } else { (xl, xr) };
            self.gxx[k] += alpha * (x.norm_sqr() - self.gxx[k]);
            self.gyy[k] += alpha * (y.norm_sqr() - self.gyy[k]);
            let g = self.gxy[k];
            self.gxy[k] = g + (y * x.conj() - g) * alpha;
        }

        if tp.auto_delay && self.cnt >= 3 && self.est_t.elapsed() >= self.est_every {
            self.est_t = Instant::now();
            if let Some(r) = self.residual_delay(sr as f32) {
                if r.abs() >= 1.5 {
                    self.delay = (self.delay + r).clamp(-(n as f32) * 0.45, n as f32 * 0.45);
                    self.clear_avg();
                    return;
                }
            }
        }

        // publish
        let frac = self.delay - self.delay.round();
        let mut out = sh.tf_out.lock();
        out.seq += 1;
        out.sr = sr as f32;
        out.n = n;
        out.delay_ms = self.delay / sr as f32 * 1000.0;
        out.frames = self.cnt;
        out.mag.resize(half + 1, 0.0);
        out.phase.resize(half + 1, 0.0);
        out.coh.resize(half + 1, 0.0);
        for k in 0..=half {
            let (gxx, gyy, gxy) = (self.gxx[k], self.gyy[k], self.gxy[k]);
            if gxx < 1e-20 {
                out.mag[k] = f32::NAN;
                out.phase[k] = f32::NAN;
                out.coh[k] = 0.0;
                continue;
            }
            out.mag[k] = 20.0 * (gxy.norm() / gxx).max(1e-12).log10();
            let th = std::f32::consts::TAU * k as f32 * frac / n as f32;
            let comp = Complex32::new(th.cos(), th.sin());
            out.phase[k] = (gxy * comp).arg().to_degrees();
            out.coh[k] = (gxy.norm_sqr() / (gxx * gyy + 1e-30)).clamp(0.0, 1.0);
        }
    }

    /// GCC-PHAT of the current cross-spectrum: remaining delay (samples) between the aligned windows.
    fn residual_delay(&mut self, sr: f32) -> Option<f32> {
        let n = self.n;
        let half = n / 2;
        let (k0, k1) = (((40.0 * n as f32 / sr) as usize).max(1), ((0.45 * sr * n as f32 / sr) as usize).min(half));
        self.cc.fill(Complex32::new(0.0, 0.0));
        let mut any = false;
        for k in k0..k1 {
            let g = self.gxy[k];
            let m = g.norm();
            if m < 1e-20 || self.gxx[k] < 1e-20 || self.gyy[k] < 1e-20 {
                continue;
            }
            let coh = (m * m / (self.gxx[k] * self.gyy[k] + 1e-30)).clamp(0.0, 1.0);
            let v = g * (coh / m);
            self.cc[k] = v;
            self.cc[n - k] = v.conj();
            any = true;
        }
        if !any {
            return None;
        }
        let ifft = self.ifft.clone().unwrap();
        ifft.process_with_scratch(&mut self.cc, &mut self.scratch);
        let at = |i: isize| self.cc[i.rem_euclid(n as isize) as usize].re.abs();
        let lim = (n / 2) as isize;
        let (mut best, mut bv) = (0isize, 0f32);
        for m in -lim..lim {
            let v = at(m);
            if v > bv {
                bv = v;
                best = m;
            }
        }
        // the peak must stand out, otherwise there is nothing to lock onto
        let mean = (0..n).map(|i| self.cc[i].re.abs()).sum::<f32>() / n as f32;
        if bv < 8.0 * mean {
            return None;
        }
        let (a, b, c) = (at(best - 1), at(best), at(best + 1));
        let den = a - 2.0 * b + c;
        let off = if den.abs() > 1e-12 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
        Some(best as f32 + off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    /// y = 0.5 * x delayed by 100.4 samples: gain, delay and coherence must come out.
    #[test]
    fn finds_delay_and_gain() {
        let sh = Shared::new();
        sh.sr.store(48000, std::sync::atomic::Ordering::Relaxed);
        sh.tf_params.lock().on = true;
        let p = Params::default();
        let mut planner = FftPlanner::<f32>::new();
        let mut eng = TfEngine::new();
        eng.est_every = Duration::ZERO;
        let mut seed = 1u32;
        let x: Vec<f32> = (0..400_000).map(|_| noise(&mut seed) * 0.3).collect();
        // fractional delay by linear interpolation is good enough at low frequencies
        let d = 100.4f32;
        let y = |t: usize| -> f32 {
            let s = t as f32 - d;
            let i = s.floor() as isize;
            let f = s - i as f32;
            if i < 0 { 0.0 } else { 0.5 * (x[i as usize] * (1.0 - f) + x[i as usize + 1] * f) }
        };
        for t in 0..x.len() {
            {
                let mut r = sh.ring.lock();
                r.push(y(t), x[t]);
            }
            if t % 2048 == 0 && t > 20000 {
                let total = sh.ring.lock().total;
                eng.step(&sh, &p, 48000, &mut planner, total);
            }
        }
        let o = sh.tf_out.lock();
        assert!(o.n > 0);
        let bin = |f: f32| (f / (48000.0 / o.n as f32)) as usize;
        let k = bin(1000.0);
        eprintln!("delay {} ms mag {} phase {} coh {}", o.delay_ms, o.mag[k], o.phase[k], o.coh[k]);
        assert!((o.delay_ms - 100.4 / 48.0).abs() < 0.05, "delay {}", o.delay_ms);
        assert!((o.mag[k] + 6.02).abs() < 0.5, "mag {}", o.mag[k]);
        assert!(o.phase[k].abs() < 10.0, "phase {}", o.phase[k]);
        assert!(o.coh[k] > 0.9, "coh {}", o.coh[k]);
    }
}
