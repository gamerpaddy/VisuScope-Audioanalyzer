//! DSP thread: windowed FFT of both channels (one complex FFT), averaging/hold traces,
//! cross-spectrum correlation, THD+N / ENOB measurements and spectrogram column generation.

use crate::shared::*;
use rustfft::{
    num_complex::{Complex32, Complex64},
    Fft, FftPlanner,
};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Window {
    Rect,
    Hann,
    Hamming,
    Blackman,
    BlackmanHarris,
    BlackmanHarris7,
    Nuttall,
    FlatTop,
    Kaiser,
    Gauss,
    Bartlett,
}

/// Half width (bins) of the main lobe of the measurement window (7-term Blackman-Harris).
const MEAS_HW: usize = 8;

impl Window {
    pub const ALL: [Window; 11] = [
        Window::Rect,
        Window::Hann,
        Window::Hamming,
        Window::Blackman,
        Window::BlackmanHarris,
        Window::BlackmanHarris7,
        Window::Nuttall,
        Window::FlatTop,
        Window::Kaiser,
        Window::Gauss,
        Window::Bartlett,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Window::Rect => "Rectangular",
            Window::Hann => "Hann",
            Window::Hamming => "Hamming",
            Window::Blackman => "Blackman",
            Window::BlackmanHarris => "Blackman-Harris 4",
            Window::BlackmanHarris7 => "Blackman-Harris 7 (-180 dB)",
            Window::Nuttall => "Nuttall 4",
            Window::FlatTop => "Flat-top (amplitude)",
            Window::Kaiser => "Kaiser (beta 12)",
            Window::Gauss => "Gaussian (sigma 0.35)",
            Window::Bartlett => "Bartlett (triangle)",
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..80 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

pub fn make_window(w: Window, n: usize) -> Vec<f32> {
    window_f64(w, n).into_iter().map(|v| v as f32).collect()
}

pub fn window_f64(w: Window, n: usize) -> Vec<f64> {
    let nf = n as f64;
    let tau = std::f64::consts::TAU;
    (0..n)
        .map(|i| {
            let x = tau * i as f64 / nf;
            let v = match w {
                Window::Rect => 1.0,
                Window::Hann => 0.5 - 0.5 * x.cos(),
                Window::Hamming => 0.54 - 0.46 * x.cos(),
                Window::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
                Window::BlackmanHarris => 0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos(),
                Window::BlackmanHarris7 => {
                    // Albrecht 7-term, sidelobes below -180 dB
                    0.271_051_400_693_42 - 0.433_297_939_234_48 * x.cos() + 0.218_122_999_543_11 * (2.0 * x).cos()
                        - 0.065_925_446_388_03 * (3.0 * x).cos()
                        + 0.010_811_742_098_37 * (4.0 * x).cos()
                        - 0.000_776_584_825_22 * (5.0 * x).cos()
                        + 0.000_013_887_217_35 * (6.0 * x).cos()
                }
                Window::Nuttall => 0.355768 - 0.487396 * x.cos() + 0.144232 * (2.0 * x).cos() - 0.012604 * (3.0 * x).cos(),
                Window::FlatTop => {
                    0.21557895 - 0.41663158 * x.cos() + 0.277263158 * (2.0 * x).cos() - 0.083578947 * (3.0 * x).cos()
                        + 0.006947368 * (4.0 * x).cos()
                }
                Window::Kaiser => {
                    let beta = 12.0;
                    let t = 2.0 * i as f64 / nf - 1.0;
                    bessel_i0(beta * (1.0 - t * t).max(0.0).sqrt()) / bessel_i0(beta)
                }
                Window::Gauss => {
                    let t = (i as f64 - nf / 2.0) / (0.35 * nf / 2.0);
                    (-0.5 * t * t).exp()
                }
                Window::Bartlett => 1.0 - (2.0 * i as f64 / nf - 1.0).abs(),
            };
            v
        })
        .collect()
}

/// Main-lobe lookup for one window, used to recover a tone's true peak amplitude from the bins around
/// it. Entry `i` describes a tone `d = -0.5 + 0.01 i` bins away from the peak bin:
/// `[rho, gain_db]` with `rho = (m[+1] - m[-1]) / m[0]` (linear magnitudes) and `gain_db` the amount to
/// add to the peak bin's level to undo the scalloping loss at that offset.
pub fn peak_table(win: Window) -> Vec<[f32; 2]> {
    let n0 = 4096;
    let w = window_f64(win, n0);
    let sw: f64 = w.iter().sum();
    let wn = |x: f64| -> f64 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &wi) in w.iter().enumerate() {
            let ph = std::f64::consts::TAU * x * i as f64 / n0 as f64;
            re += wi * ph.cos();
            im -= wi * ph.sin();
        }
        (re * re + im * im).sqrt() / sw
    };
    (0..=100)
        .map(|i| {
            let d = -0.5 + i as f64 * 0.01;
            let w0 = wn(d).max(1e-12);
            let rho = (wn(1.0 - d) - wn(1.0 + d)) / w0;
            [rho as f32, (-20.0 * w0.log10()) as f32]
        })
        .collect()
}

/// Level (dB) of the true peak of an isolated tone whose strongest bin is `kc`, corrected for
/// scalloping loss with a table from [`peak_table`]. `None` if `kc` is not a local maximum.
pub fn corrected_peak_db(data: &[f32], kc: usize, tab: &[[f32; 2]]) -> Option<f32> {
    if tab.len() < 2 || kc == 0 || kc + 1 >= data.len() {
        return None;
    }
    let (a, m, c) = (data[kc - 1], data[kc], data[kc + 1]);
    if m < a || m < c {
        return None;
    }
    let lin = |d: f32| 10f32.powf(d / 20.0);
    let rho = (lin(c) - lin(a)) / lin(m).max(1e-20);
    let last = tab.len() - 1;
    if rho <= tab[0][0] {
        return Some(m + tab[0][1]);
    }
    if rho >= tab[last][0] {
        return Some(m + tab[last][1]);
    }
    let j = tab.partition_point(|e| e[0] < rho).clamp(1, last);
    let (p, q) = (tab[j - 1], tab[j]);
    let t = (rho - p[0]) / (q[0] - p[0]).max(1e-9);
    Some(m + p[1] + t * (q[1] - p[1]))
}
/// Fast log2 approximation (abs error < 1e-4).
#[inline(always)]
fn fast_log2(x: f32) -> f32 {
    let bits = x.to_bits();
    let e = ((bits >> 23) & 255) as f32 - 127.0;
    let m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    let p = -1.741_793_9 + (2.821_202_6 + (-1.469_956_8 + (0.447_179_55 - 0.056_570_851 * m) * m) * m) * m;
    e + p
}
const DB_PER_LOG2: f32 = 3.010_299_9;

struct State {
    n: usize,
    sr: u32,
    win: Window,
    log: bool,
    fft: Option<Arc<dyn Fft<f32>>>,
    scratch: Vec<Complex32>,
    buf: Vec<Complex32>,
    w: Vec<f32>,
    sw: f64,
    sw2: f64,
    l: Vec<f32>,
    r: Vec<f32>,
    plin: [Vec<f32>; 2],
    avgp: [Vec<f32>; 2],
    avg_cnt: u32,
    live: [Vec<f32>; 2],
    avg: [Vec<f32>; 2],
    mx: [Vec<f32>; 2],
    mn: [Vec<f32>; 2],
    /// worst-case scalloping loss of the display window (tone half a bin off-centre), dB
    scallop_db: f32,
    peak_tab: Vec<[f32; 2]>,
    rowlo: Vec<f32>,
    rowhi: Vec<f32>,
    metrics: [Metrics; 2],
    epoch: u32,
    meng: Option<MeasEngine>,
    meas_t: Instant,
}

impl State {
    fn new() -> Self {
        Self {
            n: 0,
            sr: 0,
            win: Window::Hann,
            log: true,
            fft: None,
            scratch: vec![],
            buf: vec![],
            w: vec![],
            sw: 1.0,
            sw2: 1.0,
            l: vec![],
            r: vec![],
            plin: Default::default(),
            avgp: Default::default(),
            avg_cnt: 0,
            live: Default::default(),
            avg: Default::default(),
            mx: Default::default(),
            mn: Default::default(),
            scallop_db: 0.0,
            peak_tab: Vec::new(),
            rowlo: vec![],
            rowhi: vec![],
            metrics: Default::default(),
            epoch: 0,
            meng: None,
            meas_t: Instant::now(),
        }
    }

    fn reset_traces(&mut self) {
        let h = self.n / 2 + 1;
        for c in 0..2 {
            self.avgp[c] = vec![0.0; h];
            self.avg[c] = vec![-200.0; h];
            self.mx[c] = vec![-200.0; h];
            self.mn[c] = vec![200.0; h];
        }
        self.avg_cnt = 0;
    }

    fn rebuild(&mut self, planner: &mut FftPlanner<f32>, p: &Params, sr: u32) {
        let n = p.fft_size;
        let size_changed = n != self.n;
        if self.sr != sr || self.log != p.log_freq {
            self.epoch += 1;
        }
        self.n = n;
        self.sr = sr;
        self.win = p.window;
        self.log = p.log_freq;
        let fft = planner.plan_fft_forward(n);
        self.scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        self.fft = Some(fft);
        self.buf = vec![Complex32::new(0.0, 0.0); n];
        self.w = make_window(p.window, n);
        self.sw = self.w.iter().map(|&v| v as f64).sum();
        self.sw2 = self.w.iter().map(|&v| (v as f64) * (v as f64)).sum();
        self.l = vec![0.0; n];
        self.r = vec![0.0; n];
        let h = n / 2 + 1;
        for c in 0..2 {
            self.plin[c] = vec![0.0; h];
            self.live[c] = vec![-200.0; h];
        }
        // scalloping loss: response of the window half a bin away from a bin centre
        {
            let wf = window_f64(p.window, n);
            let w0: f64 = wf.iter().sum();
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, w) in wf.iter().enumerate() {
                let ph = std::f64::consts::PI * i as f64 / n as f64;
                re += w * ph.cos();
                im -= w * ph.sin();
            }
            self.scallop_db = (20.0 * (w0 / (re * re + im * im).sqrt().max(1e-30)).log10()) as f32;
            self.peak_tab = peak_table(p.window);
        }
        self.reset_traces();
        let _ = size_changed;

        // Spectrogram row -> bin mapping.
        let bin_hz = sr as f32 / n as f32;
        let srf = sr as f32;
        let edge = |i: usize| -> f32 {
            let u = i as f32 / ROWS as f32;
            u_to_freq(u, p.log_freq, srf) / bin_hz
        };
        self.rowlo = (0..ROWS).map(edge).collect();
        self.rowhi = (1..=ROWS).map(edge).collect();
    }
}

pub fn spawn(sh: Arc<Shared>) {
    std::thread::Builder::new()
        .name("dsp".into())
        .spawn(move || run(sh))
        .expect("spawn dsp thread");
}

fn wait(sh: &Shared, ms: u64) {
    let mut g = sh.cv_lock.lock();
    sh.cv.wait_for(&mut g, Duration::from_millis(ms));
}

fn run(sh: Arc<Shared>) {
    let mut planner = FftPlanner::<f32>::new();
    let mut st = State::new();
    let mut last_total = 0u64;
    let mut last_col = 0u64;
    let mut last_reset = 0u32;
    let mut fps_t = Instant::now();
    let mut fps_frames = 0u32;
    let mut fps = 0.0f32;
    let mut ms_smooth = 0.0f32;

    loop {
        if sh.quit.load(Relaxed) {
            return;
        }
        sh.dsp_beat.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
        let p = sh.params.lock().clone();
        let sr = sh.sr.load(Relaxed);
        if sr == 0 || p.paused {
            wait(&sh, 20);
            continue;
        }
        let total = sh.ring.lock().total;
        if total < last_total {
            last_total = total;
            last_col = total;
        }
        let hop = ((sr as f32 / p.update_hz.max(1.0)) as u64).max(1);
        if total < last_total + hop {
            wait(&sh, 3);
            continue;
        }
        let t0 = Instant::now();
        last_total = total;

        if st.n != p.fft_size || st.win != p.window || st.sr != sr || st.log != p.log_freq {
            st.rebuild(&mut planner, &p, sr);
            sh.columns.lock().clear();
        }
        if last_reset != p.reset_gen {
            last_reset = p.reset_gen;
            st.reset_traces();
        }

        {
            let ring = sh.ring.lock();
            ring.copy_last(&mut st.l, &mut st.r);
        }
        let n = st.n;
        let half = n / 2;
        let srf = sr as f32;

        // Time-domain levels over the most recent chunk.
        let tn = n.min(8192);
        let mut rms_db = [0.0f32; 2];
        let mut peak_db = [0.0f32; 2];
        let mut dc = [0.0f32; 2];
        for c in 0..2 {
            let s = if c == 0 { &st.l[n - tn..] } else { &st.r[n - tn..] };
            let (mut sum, mut sq, mut pk) = (0.0f64, 0.0f64, 0.0f32);
            for &v in s {
                sum += v as f64;
                sq += (v as f64) * (v as f64);
                pk = pk.max(v.abs());
            }
            rms_db[c] = 10.0 * ((sq / tn as f64) as f32 + 1e-20).log10();
            peak_db[c] = 20.0 * (pk + 1e-10).log10();
            dc[c] = (sum / tn as f64) as f32;
        }

        for i in 0..n {
            let w = st.w[i];
            st.buf[i] = Complex32::new(st.l[i] * w, st.r[i] * w);
        }
        let fft = st.fft.clone().unwrap();
        fft.process_with_scratch(&mut st.buf, &mut st.scratch);

        let c = ((2.0 / st.sw) * (2.0 / st.sw)) as f32;
        let alpha = (1.0 / p.avg_count.max(1.0)).max(1.0 / (st.avg_cnt as f32 + 1.0));
        st.avg_cnt = st.avg_cnt.saturating_add(1);
        let mask = n - 1;
        for k in 0..=half {
            let zk = st.buf[k];
            let zn = st.buf[(n - k) & mask].conj();
            let xl = (zk + zn) * 0.5;
            let d = zk - zn;
            let xr = Complex32::new(d.im, -d.re) * 0.5;
            let pl = xl.norm_sqr();
            let pr = xr.norm_sqr();
            st.plin[0][k] = pl;
            st.plin[1][k] = pr;
            let dl = DB_PER_LOG2 * fast_log2(pl * c + 1e-30);
            let dr = DB_PER_LOG2 * fast_log2(pr * c + 1e-30);
            st.live[0][k] = dl;
            st.live[1][k] = dr;
            st.avgp[0][k] += alpha * (pl - st.avgp[0][k]);
            st.avgp[1][k] += alpha * (pr - st.avgp[1][k]);
            st.avg[0][k] = DB_PER_LOG2 * fast_log2(st.avgp[0][k] * c + 1e-30);
            st.avg[1][k] = DB_PER_LOG2 * fast_log2(st.avgp[1][k] * c + 1e-30);
            if dl > st.mx[0][k] {
                st.mx[0][k] = dl;
            }
            if dr > st.mx[1][k] {
                st.mx[1][k] = dr;
            }
            if dl < st.mn[0][k] {
                st.mn[0][k] = dl;
            }
            if dr < st.mn[1][k] {
                st.mn[1][k] = dr;
            }
        }

        // Measurements
        let manual = if p.manual_fund { Some(p.fund_hz) } else { None };
        let hi = p.bw_hi.min(srf * 0.5 * 0.999);
        // Measurements use their own FFT (f64, 7-term Blackman-Harris) so they do not depend on
        // the display window and can resolve distortion/noise far below the display's dynamic range.
        let mn = p.fft_size.clamp(16384, 131072);
        if st.meng.as_ref().map(|e| e.n) != Some(mn) {
            st.meng = Some(MeasEngine::new(mn));
            st.metrics = Default::default();
            st.meas_t = Instant::now() - Duration::from_secs(1);
        }
        if st.meas_t.elapsed() >= Duration::from_millis(120) {
            st.meas_t = Instant::now();
            let mut eng = st.meng.take().unwrap();
            {
                let ring = sh.ring.lock();
                ring.copy_last(&mut eng.l, &mut eng.r);
            }
            let res = eng.run(srf, p.bw_lo, hi, p.harmonics.clamp(2, MAXH), manual);
            st.meng = Some(eng);
            for ch in 0..2 {
                st.metrics[ch] = smooth_metrics(st.metrics[ch], res[ch]);
            }
        }

        // Spectrogram column
        let col_interval = (srf / p.cols_per_sec.clamp(1.0, p.update_hz.max(1.0))) as u64;
        if total >= last_col + col_interval.max(1) {
            last_col = total;
            let mut col = vec![0u8; ROWS];
            for r in 0..ROWS {
                let (b0, b1) = (st.rowlo[r], st.rowhi[r]);
                let get = |s: &State, k: usize| -> f32 {
                    let k = k.min(half);
                    match p.spectro_src {
                        0 => s.live[0][k],
                        1 => s.live[1][k],
                        _ => s.live[0][k].max(s.live[1][k]),
                    }
                };
                let v = if b1 - b0 < 1.0 {
                    let mid = 0.5 * (b0 + b1);
                    let k0 = mid.floor().max(0.0) as usize;
                    let fr = mid - mid.floor();
                    get(&st, k0) * (1.0 - fr) + get(&st, k0 + 1) * fr
                } else {
                    let a = b0.floor().max(0.0) as usize;
                    let b = (b1.ceil() as usize).min(half);
                    let mut m = -300.0f32;
                    for k in a..=b {
                        m = m.max(get(&st, k));
                    }
                    m
                };
                col[r] = (((v + HIST_DB_RANGE) / HIST_DB_RANGE) * 255.0).clamp(0.0, 255.0) as u8;
            }
            let mut q = sh.columns.lock();
            q.push((st.epoch, col));
            if q.len() > 1024 {
                let drop_n = q.len() - 1024;
                q.drain(0..drop_n);
            }
        }

        // On a slow machine a huge FFT can cost more than the hop budget: never run flat out.
        // Back off in proportion to the frame cost so the UI and audio threads keep their share.
        let spent = t0.elapsed();
        if spent > Duration::from_millis(3) {
            std::thread::sleep(spent / 2);
        }

        // Publish
        fps_frames += 1;
        if fps_t.elapsed().as_secs_f32() >= 0.5 {
            fps = fps_frames as f32 / fps_t.elapsed().as_secs_f32();
            fps_frames = 0;
            fps_t = Instant::now();
        }
        ms_smooth += 0.1 * (t0.elapsed().as_secs_f32() * 1000.0 - ms_smooth);
        {
            let mut a = sh.analysis.lock();
            a.seq += 1;
            a.epoch = st.epoch;
            a.sr = srf;
            a.n = n;
            for ch in 0..2 {
                copy_into(&mut a.live[ch], &st.live[ch]);
                copy_into(&mut a.avg[ch], &st.avg[ch]);
                copy_into(&mut a.max[ch], &st.mx[ch]);
                copy_into(&mut a.min[ch], &st.mn[ch]);
            }
            if let Some(e) = st.meng.as_ref() {
                copy_into(&mut a.corr, &e.corr);
            }
            a.scallop_db = st.scallop_db;
            a.peak_tab.clone_from(&st.peak_tab);
            a.metrics = st.metrics;
            a.meas_n = st.meng.as_ref().map(|e| e.n).unwrap_or(0);
            a.rms_db = rms_db;
            a.peak_db = peak_db;
            a.dc = dc;
            a.enbw_bins = (n as f64 * st.sw2 / (st.sw * st.sw)) as f32;
            a.coherent_gain = (st.sw / n as f64) as f32;
            a.dsp_ms = ms_smooth;
            a.dsp_fps = fps;
        }
    }
}

fn copy_into(dst: &mut Vec<f32>, src: &[f32]) {
    dst.resize(src.len(), 0.0);
    dst.copy_from_slice(src);
}

fn smooth_metrics(prev: Metrics, new: Metrics) -> Metrics {
    if !new.valid {
        return new;
    }
    if !prev.valid || (prev.fund_hz - new.fund_hz).abs() > 0.03 * new.fund_hz.max(1.0) {
        return new;
    }
    let a = 0.3;
    let s = |p: f32, n: f32| if n.is_finite() && p.is_finite() { p + a * (n - p) } else { n };
    let mut m = new;
    m.fund_hz = s(prev.fund_hz, new.fund_hz);
    m.fund_dbfs = s(prev.fund_dbfs, new.fund_dbfs);
    m.thd_db = s(prev.thd_db, new.thd_db);
    m.thdn_db = s(prev.thdn_db, new.thdn_db);
    m.snr_db = s(prev.snr_db, new.snr_db);
    m.sinad_db = s(prev.sinad_db, new.sinad_db);
    m.sfdr_db = s(prev.sfdr_db, new.sfdr_db);
    m.noise_floor_dbfs = s(prev.noise_floor_dbfs, new.noise_floor_dbfs);
    m.enob = (m.sinad_db - 1.76) / 6.02;
    m.enob_fs = (m.sinad_db - 1.76 - m.fund_dbfs) / 6.02;
    for i in 0..MAXH {
        m.harm_db[i] = s(prev.harm_db[i], new.harm_db[i]);
    }
    m
}

#[inline]
fn db10(x: f64) -> f32 {
    (10.0 * (x + 1e-30).log10()) as f32
}

/// THD, THD+N, SNR, SINAD, SFDR and ENOB from one power spectrum frame.
#[allow(clippy::too_many_arguments)]
fn measure(
    plin: &[f64],
    n: usize,
    sr: f32,
    sw: f64,
    sw2: f64,
    lo: f32,
    hi: f32,
    nh: usize,
    hw: usize,
    manual: Option<f32>,
) -> Metrics {
    let mut m = Metrics::default();
    m.harm_db = [-200.0; MAXH];
    let half = n / 2;
    let bin_hz = sr / n as f32;
    // keep the DC skirt of the window out of the measurement band
    let k_lo = ((lo / bin_hz).ceil() as usize).max(hw + 2);
    let k_hi = ((hi / bin_hz).floor() as usize).min(half - 1);
    if k_hi < k_lo + 4 * hw + 4 {
        return m;
    }
    let scale = 2.0 / (n as f64 * sw2);
    let amp_c = (2.0 / sw) * (2.0 / sw);

    let argmax = |a: usize, b: usize| -> usize {
        let mut kb = a;
        let mut vb = -1.0f64;
        for k in a..=b {
            if plin[k] > vb {
                vb = plin[k];
                kb = k;
            }
        }
        kb
    };

    let mut total = 0.0f64;
    for k in k_lo..=k_hi {
        total += plin[k] as f64;
    }
    let mut kp = argmax(k_lo, k_hi);
    if let Some(f) = manual {
        let kc = ((f / bin_hz).round() as usize).clamp(k_lo, k_hi);
        kp = argmax(kc.saturating_sub(3).max(k_lo), (kc + 3).min(k_hi));
    }
    let range = |kc: usize| (kc.saturating_sub(hw).max(k_lo), (kc + hw).min(k_hi));
    let (fa, fb) = range(kp);
    let (mut pf, mut ksum) = (0.0f64, 0.0f64);
    for k in fa..=fb {
        let v = plin[k] as f64;
        pf += v;
        ksum += v * k as f64;
    }
    if pf <= 0.0 {
        return m;
    }
    let f0 = (ksum / pf) * bin_hz as f64;
    let mut used: Vec<(usize, usize)> = vec![(fa, fb)];
    let mut used_sum = pf;
    let mut ph_tot = 0.0f64;
    let mut hp = [0.0f64; MAXH];
    for h in 2..=nh {
        let mut f = (f0 * h as f64) % sr as f64;
        if f > sr as f64 * 0.5 {
            f = sr as f64 - f;
        }
        m.harm_hz[h - 1] = f as f32;
        let kc = (f / bin_hz as f64).round() as usize;
        if kc < k_lo || kc > k_hi {
            continue;
        }
        let kc = argmax(kc.saturating_sub(2).max(k_lo), (kc + 2).min(k_hi));
        let (ra, rb) = range(kc);
        if used.iter().any(|&(ua, ub)| ra <= ub && rb >= ua) {
            continue;
        }
        used.push((ra, rb));
        let s: f64 = plin[ra..=rb].iter().map(|&v| v as f64).sum();
        used_sum += s;
        ph_tot += s;
        hp[h - 1] = s;
    }
    m.harm_hz[0] = f0 as f32;
    let nbins = k_hi - k_lo + 1;
    let used_bins: usize = used.iter().map(|&(a, b)| b - a + 1).sum();
    let noise_sum = (total - used_sum).max(0.0);
    let noise_bins = nbins.saturating_sub(used_bins).max(1);

    let pf_s = pf * scale;
    let ph_s = ph_tot * scale;
    let pn_s = noise_sum * scale;
    let tot_minus_f = ph_s + pn_s;

    m.valid = 10.0 * (2.0 * pf_s + 1e-30).log10() > -110.0;
    m.fund_hz = f0 as f32;
    m.fund_dbfs = db10(2.0 * pf_s);
    m.thd_db = db10(ph_s / pf_s);
    m.thdn_db = db10(tot_minus_f / pf_s);
    m.snr_db = db10(pf_s / pn_s.max(1e-30));
    m.sinad_db = db10(pf_s / tot_minus_f.max(1e-30));
    m.enob = (m.sinad_db - 1.76) / 6.02;
    m.enob_fs = (m.sinad_db - 1.76 - m.fund_dbfs) / 6.02;
    for h in 2..=nh {
        if hp[h - 1] > 0.0 {
            m.harm_db[h - 1] = db10(hp[h - 1] / pf);
        }
    }
    // SFDR: strongest bin outside the fundamental lobe.
    let mut spur = 0.0f64;
    for k in k_lo..=k_hi {
        if k < fa || k > fb {
            spur = spur.max(plin[k]);
        }
    }
    m.sfdr_db = db10(plin[kp] as f64 / (spur as f64 + 1e-30));
    m.noise_floor_dbfs = db10(noise_sum / noise_bins as f64 * amp_c);
    m
}

/// Dedicated high-dynamic-range measurement path: f64 FFT with a 7-term Blackman-Harris window.
struct MeasEngine {
    n: usize,
    fft: Arc<dyn Fft<f64>>,
    scratch: Vec<Complex64>,
    buf: Vec<Complex64>,
    w: Vec<f64>,
    sw: f64,
    sw2: f64,
    l: Vec<f32>,
    r: Vec<f32>,
    plin: [Vec<f64>; 2],
    /// smoothed cross spectrum / powers for the L-R correlation per bin
    cre: Vec<f64>,
    pls: Vec<f64>,
    prs: Vec<f64>,
    corr: Vec<f32>,
}

impl MeasEngine {
    fn new(n: usize) -> Self {
        let fft = FftPlanner::<f64>::new().plan_fft_forward(n);
        let w = window_f64(Window::BlackmanHarris7, n);
        let sw = w.iter().sum();
        let sw2 = w.iter().map(|v| v * v).sum();
        Self {
            n,
            scratch: vec![Complex64::new(0.0, 0.0); fft.get_inplace_scratch_len()],
            fft,
            buf: vec![Complex64::new(0.0, 0.0); n],
            w,
            sw,
            sw2,
            l: vec![0.0; n],
            r: vec![0.0; n],
            plin: [vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]],
            cre: vec![0.0; n / 2 + 1],
            pls: vec![0.0; n / 2 + 1],
            prs: vec![0.0; n / 2 + 1],
            corr: vec![0.0; n / 2 + 1],
        }
    }

    /// Analyse the samples currently in `l` / `r`.
    fn run(&mut self, sr: f32, lo: f32, hi: f32, nh: usize, manual: Option<f32>) -> [Metrics; 2] {
        let n = self.n;
        // remove DC so its window skirt cannot leak into the band
        let ml = self.l.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let mr = self.r.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        for i in 0..n {
            let w = self.w[i];
            self.buf[i] = Complex64::new((self.l[i] as f64 - ml) * w, (self.r[i] as f64 - mr) * w);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        let mask = n - 1;
        for k in 0..=n / 2 {
            let zk = self.buf[k];
            let zn = self.buf[(n - k) & mask].conj();
            let xl = (zk + zn) * 0.5;
            let d = zk - zn;
            let xr = Complex64::new(d.im, -d.re) * 0.5;
            let (pl, pr) = (xl.norm_sqr(), xr.norm_sqr());
            self.plin[0][k] = pl;
            self.plin[1][k] = pr;
            // L/R correlation per bin: Re<L R*> / sqrt(<|L|^2><|R|^2>), smoothed over ~0.4 s
            const A: f64 = 0.26;
            self.cre[k] += A * (xl.re * xr.re + xl.im * xr.im - self.cre[k]);
            self.pls[k] += A * (pl - self.pls[k]);
            self.prs[k] += A * (pr - self.prs[k]);
            self.corr[k] = (self.cre[k] / (self.pls[k] * self.prs[k] + 1e-300).sqrt()) as f32;
        }
        let a = measure(&self.plin[0], n, sr, self.sw, self.sw2, lo, hi, nh, MEAS_HW, manual);
        let b = measure(&self.plin[1], n, sr, self.sw, self.sw2, lo, hi, nh, MEAS_HW, manual);
        [a, b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauss(seed: &mut u64) -> f64 {
        let mut s = 0.0;
        for _ in 0..12 {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            s += (*seed >> 11) as f64 / (1u64 << 53) as f64;
        }
        s - 6.0
    }

    /// Tone at `f0` (not bin-centred) with a 2nd/3rd harmonic and white noise, all given in dBc.
    fn synth(f0: f64, h2_dbc: f64, h3_dbc: f64, noise_dbc: f64, n: usize, sr: f32) -> (MeasEngine, f64) {
        let a1 = 0.5f64;
        let a2 = a1 * 10f64.powf(h2_dbc / 20.0);
        let a3 = a1 * 10f64.powf(h3_dbc / 20.0);
        // noise power spread over 0..sr/2; in-band share computed by the caller
        let sigma = (a1 * a1 / 2.0 * 10f64.powf(noise_dbc / 10.0)).sqrt();
        let mut seed = 7u64;
        let mut eng = MeasEngine::new(n);
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let tau = std::f64::consts::TAU;
            let x = a1 * (tau * f0 * t).sin() + a2 * (tau * 2.0 * f0 * t + 0.3).sin() + a3 * (tau * 3.0 * f0 * t + 1.1).sin() + sigma * gauss(&mut seed);
            eng.l[i] = x as f32;
            eng.r[i] = x as f32;
        }
        (eng, sigma)
    }

    #[test]
    fn window7_sidelobes() {
        // far sidelobes of the measurement window must sit below -150 dB
        let n = 8192;
        let w = window_f64(Window::BlackmanHarris7, n);
        let mut buf: Vec<Complex64> = (0..n).map(|i| Complex64::new(w[i] * (std::f64::consts::TAU * 1000.37 * i as f64 / n as f64).sin(), 0.0)).collect();
        FftPlanner::<f64>::new().plan_fft_forward(n).process(&mut buf);
        let peak = buf.iter().take(n / 2).map(|c| c.norm()).fold(0.0, f64::max);
        let far = (1020..n / 2).map(|k| buf[k].norm()).fold(0.0, f64::max);
        let db = 20.0 * (far / peak).log10();
        eprintln!("BH7 far sidelobe {db:.1} dB");
        assert!(db < -150.0, "{db}");
    }

    #[test]
    fn thdn_independent_of_leakage_and_deep() {
        let sr = 48000.0f32;
        // non-coherent tone, distortion and noise far below -100 dBc
        for &(f0, h2, h3, nz) in &[(1234.5678f64, -125.0, -130.0, -128.0), (997.0, -110.0, -120.0, -118.0), (50.3, -105.0, -115.0, -125.0)] {
            let n = 32768;
            let (mut eng, sigma) = synth(f0, h2, h3, nz, n, sr);
            let m = eng.run(sr, 20.0, 20000.0, 9, None)[0];
            let a1 = 0.5f64;
            let p_f = a1 * a1 / 2.0;
            let p_h = (a1 * 10f64.powf(h2 / 20.0)).powi(2) / 2.0 + (a1 * 10f64.powf(h3 / 20.0)).powi(2) / 2.0;
            let p_n = sigma * sigma * (20000.0 - 20.0) / (sr as f64 / 2.0);
            let expect = 10.0 * ((p_h + p_n) / p_f).log10();
            eprintln!("f0 {f0}: measured thdn {:.2} dB thd {:.2} dB, expected thdn {expect:.2} dB", m.thdn_db, m.thd_db);
            assert!(m.valid);
            assert!((m.fund_hz as f64 - f0).abs() < 0.5, "f0 {}", m.fund_hz);
            // f32 sample quantisation (about -150 dBc) and noise estimation scatter allow ~1.5 dB
            assert!((m.thdn_db as f64 - expect).abs() < 1.5, "thdn {} vs {expect}", m.thdn_db);
            assert!(m.thdn_db < -100.0);
        }
    }

    #[test]
    fn thdn_enob_moderate() {
        let sr = 48000.0f32;
        let (mut eng, sigma) = synth(2929.6875, -60.0, -200.0, -70.0, 16384, sr);
        let m = eng.run(sr, 20.0, 20000.0, 9, None)[0];
        let a1 = 0.5f64;
        let p_h = (a1 * 10f64.powf(-60.0 / 20.0)).powi(2) / 2.0;
        let p_n = sigma * sigma * (20000.0 - 20.0) / (sr as f64 / 2.0);
        let thdn = 10.0 * ((p_h + p_n) / (a1 * a1 / 2.0)).log10();
        eprintln!("{m:?} expected {thdn:.2}");
        assert!((m.thd_db as f64 + 60.0).abs() < 0.3);
        assert!((m.thdn_db as f64 - thdn).abs() < 0.5);
        assert!((m.enob as f64 - (-thdn - 1.76) / 6.02).abs() < 0.1);
        assert!((m.fund_dbfs as f64 - 20.0 * a1.log10()).abs() < 0.1);
    }
    /// A tone half a bin off-centre reads low by the window's scalloping loss; parabolic interpolation
    /// of the three bins around the peak recovers (nearly) all of it.
    #[test]
    fn scalloping_and_parabolic_peak() {
        let n = 8192;
        for (win, expect_loss) in [(Window::Hann, 1.42), (Window::BlackmanHarris, 0.83), (Window::FlatTop, 0.0)] {
            let w = window_f64(win, n);
            let sw: f64 = w.iter().sum();
            let mut buf: Vec<Complex64> = (0..n).map(|i| Complex64::new(w[i] * (std::f64::consts::TAU * 1000.5 * i as f64 / n as f64).sin(), 0.0)).collect();
            FftPlanner::<f64>::new().plan_fft_forward(n).process(&mut buf);
            let db: Vec<f64> = (0..n / 2).map(|k| 20.0 * (2.0 * buf[k].norm() / sw).log10()).collect();
            let kp = (1..n / 2).max_by(|&a, &b| db[a].partial_cmp(&db[b]).unwrap()).unwrap();
            let raw = db[kp];
            let dbf: Vec<f32> = db.iter().map(|v| *v as f32).collect();
            let peak = corrected_peak_db(&dbf, kp, &peak_table(win)).unwrap() as f64;
            eprintln!("{win:?}: bin peak {raw:.3} dB, parabolic {peak:.3} dB (expected loss ~{expect_loss})");
            assert!((raw + expect_loss).abs() < 0.1, "{win:?} raw {raw}");
            assert!(peak.abs() < 0.05, "{win:?} corrected {peak}");
        }
    }

    /// The per-bin L/R correlation comes from the measurement engine and must not depend on the display window.
    #[test]
    fn stereo_correlation_per_bin() {
        let sr = 48000.0f32;
        for (inv, expect) in [(false, 1.0f32), (true, -1.0f32)] {
            let n = 16384;
            let mut eng = MeasEngine::new(n);
            for i in 0..n {
                let x = 0.4 * (std::f64::consts::TAU * 1234.5 * i as f64 / sr as f64).sin() as f32;
                eng.l[i] = x;
                eng.r[i] = if inv { -x } else { x };
            }
            for _ in 0..30 {
                eng.run(sr, 20.0, 20000.0, 9, None);
            }
            let k = (1234.5 / (sr / n as f32)) as usize;
            let c = eng.corr[k];
            eprintln!("inv={inv}: corr at tone {c:.3}");
            assert!((c - expect).abs() < 0.02, "corr {c}");
        }
    }
}
