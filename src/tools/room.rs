//! Room-acoustics helpers for the Impulse window: octave-band filtering, per-band decay times, STI
//! from the impulse response, cumulative spectral decay (waterfall) and low-frequency room modes.

use super::impulse::{analyse, Metrics};
use rustfft::{num_complex::Complex32, FftPlanner};
use std::f64::consts::{PI, TAU};

pub const BANDS: [f32; 7] = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
/// STI modulation frequencies (Hz)
pub const MOD_F: [f64; 14] = [0.63, 0.8, 1.0, 1.25, 1.6, 2.0, 2.5, 3.15, 4.0, 5.0, 6.3, 8.0, 10.0, 12.5];
const ALPHA: [f32; 7] = [0.085, 0.127, 0.230, 0.233, 0.309, 0.224, 0.173];
const BETA: [f32; 6] = [0.085, 0.078, 0.065, 0.011, 0.047, 0.095];

// ------------------------------------------------------------------ octave band filter

#[derive(Clone, Copy)]
struct Bq {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Bq {
    fn new(b: [f64; 3], a0: f64, a1: f64, a2: f64) -> Self {
        Self { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [a1 / a0, a2 / a0], z: [0.0; 2] }
    }

    /// Second-order low / high pass (RBJ cookbook).
    fn second(f: f64, sr: f64, q: f64, high: bool) -> Self {
        let w = TAU * f / sr;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        let b = if high { [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0] } else { [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0] };
        Self::new(b, 1.0 + al, -2.0 * c, 1.0 - al)
    }

    /// First-order low / high pass (bilinear).
    fn first(f: f64, sr: f64, high: bool) -> Self {
        let k = (PI * f / sr).tan();
        let b = if high { [1.0, -1.0, 0.0] } else { [k, k, 0.0] };
        Self::new(b, 1.0 + k, k - 1.0, 0.0)
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Causal 6th-order Butterworth octave band around `fc`; `None` if the band does not fit below Nyquist.
pub fn octave_band(ir: &[f32], sr: f32, fc: f32) -> Option<Vec<f32>> {
    let (lo, hi) = (fc as f64 / 2f64.sqrt(), fc as f64 * 2f64.sqrt());
    if hi >= sr as f64 * 0.45 {
        return None;
    }
    let s = sr as f64;
    let mut secs = [Bq::first(lo, s, true), Bq::second(lo, s, 1.0, true), Bq::first(hi, s, false), Bq::second(hi, s, 1.0, false)];
    Some(
        ir.iter()
            .map(|&x| {
                let mut y = x as f64;
                for q in secs.iter_mut() {
                    y = q.run(y);
                }
                y as f32
            })
            .collect(),
    )
}

// ------------------------------------------------------------------ decay per band

#[derive(Clone, Default)]
pub struct BandRes {
    pub fc: f32,
    pub m: Option<Metrics>,
}

pub fn band_decays(ir: &[f32], sr: f32) -> Vec<BandRes> {
    BANDS.iter().map(|&fc| BandRes { fc, m: octave_band(ir, sr, fc).and_then(|b| analyse(&b, sr)).map(|r| r.3) }).collect()
}

// ------------------------------------------------------------------ STI

/// Modulation transfer at `f` Hz of the squared impulse response `e`.
pub fn mtf(e: &[f64], sr: f64, f: f64) -> f64 {
    let w = -TAU * f / sr;
    let (sn, cs) = w.sin_cos();
    let (mut zr, mut zi) = (1.0f64, 0.0f64);
    let (mut re, mut im, mut tot) = (0.0, 0.0, 0.0);
    for &v in e {
        re += v * zr;
        im += v * zi;
        tot += v;
        let t = zr * cs - zi * sn;
        zi = zr * sn + zi * cs;
        zr = t;
    }
    ((re * re + im * im).sqrt() / tot.max(1e-30)).clamp(0.0, 1.0)
}

#[derive(Clone)]
pub struct Sti {
    pub mti: [f32; 7],
    pub sti: f32,
    pub alcons: f32,
}

fn ti_of(m: f64) -> f64 {
    let m = m.clamp(1e-6, 1.0 - 1e-6);
    ((10.0 * (m / (1.0 - m)).log10()).clamp(-15.0, 15.0) + 15.0) / 30.0
}

/// Combine band MTIs into the STI (male weighting, IEC 60268-16, no masking) and %ALcons.
pub fn sti_from_mti(mti: [f32; 7]) -> Sti {
    let mut s: f32 = (0..7).map(|k| ALPHA[k] * mti[k]).sum();
    for k in 0..6 {
        s -= BETA[k] * (mti[k] * mti[k + 1]).max(0.0).sqrt();
    }
    let sti = s.clamp(0.0, 1.0);
    Sti { mti, sti, alcons: (170.5405 * (-5.419 * sti as f64).exp()).clamp(0.0, 100.0) as f32 }
}

/// Indirect STI from the impulse response: MTF per octave band at the 14 modulation frequencies.
/// `pk` is the sample of the direct sound, `cut` the usable length after it.
pub fn sti(ir: &[f32], sr: f32, pk: usize, cut: usize) -> Option<Sti> {
    let mut mti = [0f32; 7];
    for (k, &fc) in BANDS.iter().enumerate() {
        let b = octave_band(ir, sr, fc)?;
        let end = (pk + cut).min(b.len());
        if end <= pk + 64 {
            return None;
        }
        let e: Vec<f64> = b[pk..end].iter().map(|v| (*v as f64).powi(2)).collect();
        mti[k] = (MOD_F.iter().map(|&f| ti_of(mtf(&e, sr as f64, f))).sum::<f64>() / MOD_F.len() as f64) as f32;
    }
    Some(sti_from_mti(mti))
}

pub fn sti_rating(s: f32) -> &'static str {
    match s {
        x if x < 0.30 => "bad",
        x if x < 0.45 => "poor",
        x if x < 0.60 => "fair",
        x if x < 0.75 => "good",
        _ => "excellent",
    }
}

// ------------------------------------------------------------------ cumulative spectral decay

pub struct Csd {
    /// dB relative to the strongest value, `rows` x `cols`, row 0 = start of the decay
    pub db: Vec<f32>,
    pub rows: usize,
    pub cols: usize,
    pub f0: f32,
    pub f1: f32,
    pub span_ms: f32,
}

pub const CSD_ROWS: usize = 64;
pub const CSD_COLS: usize = 240;

pub fn csd(ir: &[f32], sr: f32, pk: usize, cut: usize) -> Option<Csd> {
    let end = (pk + cut.min((0.8 * sr) as usize)).min(ir.len());
    if end < pk + 1024 {
        return None;
    }
    let span = (((end - pk) as f32) * 0.7).min(0.4 * sr).max(0.05 * sr);
    let nfft = (end - pk).next_power_of_two().clamp(8192, 1 << 17);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(nfft);
    let mut buf = vec![Complex32::new(0.0, 0.0); nfft];
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let (f0, f1) = (20.0f32, 20000.0f32.min(sr * 0.47));
    let ratio = (f1 / f0).powf(1.0 / (CSD_COLS - 1) as f32);
    let hz_bin = sr / nfft as f32;
    let fade = ((0.0005 * sr) as usize).max(4);
    let mut db = vec![-200.0f32; CSD_ROWS * CSD_COLS];
    for r in 0..CSD_ROWS {
        let start = pk + (r as f32 * span / CSD_ROWS as f32) as usize;
        if end <= start + 256 {
            continue;
        }
        let len = end - start;
        buf.fill(Complex32::new(0.0, 0.0));
        let tail = (len / 4).max(1);
        for i in 0..len {
            let mut w = 1.0f32;
            if i < fade {
                w *= 0.5 - 0.5 * (PI as f32 * i as f32 / fade as f32).cos();
            }
            if i + tail > len {
                w *= 0.5 + 0.5 * (PI as f32 * (i + tail - len) as f32 / tail as f32).cos();
            }
            buf[i] = Complex32::new(ir[start + i] * w, 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for c in 0..CSD_COLS {
            let fc = f0 * ratio.powi(c as i32);
            let (a, b) = (fc / ratio.sqrt() / hz_bin, fc * ratio.sqrt() / hz_bin);
            let (ia, ib) = ((a.floor() as usize).max(1), (b.ceil() as usize).max(a.floor() as usize + 1).min(nfft / 2));
            let p: f32 = (ia..=ib).map(|k| buf[k].norm_sqr()).sum::<f32>() / (ib - ia + 1) as f32;
            db[r * CSD_COLS + c] = 10.0 * p.max(1e-30).log10();
        }
    }
    let mx = db.iter().cloned().fold(f32::MIN, f32::max);
    for v in db.iter_mut() {
        *v -= mx;
    }
    Some(Csd { db, rows: CSD_ROWS, cols: CSD_COLS, f0, f1, span_ms: span / sr * 1000.0 })
}

// ------------------------------------------------------------------ room modes

#[derive(Clone)]
pub struct Mode {
    pub f: f32,
    pub db: f32,
    pub t60: f32,
}

pub struct Modes {
    pub f: Vec<f32>,
    pub db: Vec<f32>,
    pub peaks: Vec<Mode>,
}

pub const MODE_FMAX: f32 = 300.0;

pub fn modes(ir: &[f32], sr: f32, pk: usize) -> Option<Modes> {
    let start = pk.saturating_sub((0.002 * sr) as usize);
    let end = (start + (1.2 * sr) as usize).min(ir.len());
    if end < start + 4096 {
        return None;
    }
    let len = end - start;
    let nfft = (len * 4).next_power_of_two().min(1 << 19);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(nfft);
    let mut buf = vec![Complex32::new(0.0, 0.0); nfft];
    let tail = len / 4;
    for i in 0..len {
        let w = if i + tail > len { 0.5 + 0.5 * (PI as f32 * (i + tail - len) as f32 / tail as f32).cos() } else { 1.0 };
        buf[i] = Complex32::new(ir[start + i] * w, 0.0);
    }
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    fft.process_with_scratch(&mut buf, &mut scratch);
    let hz = sr / nfft as f32;
    let (k0, k1) = ((15.0 / hz) as usize, ((MODE_FMAX * 1.2 / hz) as usize).min(nfft / 2 - 1));
    let f: Vec<f32> = (k0..=k1).map(|k| k as f32 * hz).collect();
    let mut db: Vec<f32> = (k0..=k1).map(|k| 20.0 * buf[k].norm().max(1e-20).log10()).collect();
    let mx = db.iter().cloned().fold(f32::MIN, f32::max);
    for v in db.iter_mut() {
        *v -= mx;
    }
    // peaks: local maximum within +-4 % of the frequency, standing out of the local mean by 3 dB
    let mut peaks: Vec<Mode> = Vec::new();
    for i in 1..f.len() - 1 {
        if f[i] < 20.0 || f[i] > MODE_FMAX {
            continue;
        }
        let w = ((f[i] * 0.04 / hz) as usize).max(2);
        let (a, b) = (i.saturating_sub(w), (i + w).min(f.len() - 1));
        if db[a..=b].iter().any(|&v| v > db[i]) {
            continue;
        }
        let w2 = ((f[i] * 0.2 / hz) as usize).max(4);
        let (a2, b2) = (i.saturating_sub(w2), (i + w2).min(f.len() - 1));
        let mean = 10.0 * (db[a2..=b2].iter().map(|v| 10f32.powf(v / 10.0)).sum::<f32>() / (b2 - a2 + 1) as f32).log10();
        if db[i] - mean < 3.0 {
            continue;
        }
        let (mut lo, mut hi) = (i, i);
        while lo > 0 && db[lo] > db[i] - 3.0 {
            lo -= 1;
        }
        while hi + 1 < f.len() && db[hi] > db[i] - 3.0 {
            hi += 1;
        }
        let bw = (f[hi] - f[lo]).max(hz);
        peaks.push(Mode { f: f[i], db: db[i], t60: 2.2 / bw });
    }
    peaks.sort_by(|a, b| b.db.total_cmp(&a.db));
    peaks.truncate(8);
    peaks.sort_by(|a, b| a.f.total_cmp(&b.f));
    Some(Modes { f, db, peaks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise_decay(sr: f32, t60: f32, secs: f32) -> Vec<f32> {
        let mut seed = 7u64;
        (0..(secs * sr) as usize)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let r = ((seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                r * 10f32.powf(-3.0 * (i as f32 / sr) / t60)
            })
            .collect()
    }

    fn gain_db(f: f32, sr: f32, fc: f32) -> f32 {
        let x: Vec<f32> = (0..(sr as usize)).map(|i| (TAU as f32 * f * i as f32 / sr).sin()).collect();
        let y = octave_band(&x, sr, fc).unwrap();
        let rms = |v: &[f32]| (v.iter().map(|a| a * a).sum::<f32>() / v.len() as f32).sqrt();
        20.0 * (rms(&y[y.len() / 2..]) / rms(&x[x.len() / 2..])).log10()
    }

    #[test]
    fn octave_band_shape() {
        let sr = 48000.0;
        assert!(gain_db(1000.0, sr, 1000.0).abs() < 1.2); // HP and LP skirts overlap: about -1 dB at the centre
        assert!((gain_db(707.0, sr, 1000.0) + 3.0).abs() < 0.8);
        assert!(gain_db(4000.0, sr, 1000.0) < -20.0);
        assert!(gain_db(250.0, sr, 1000.0) < -20.0);
    }

    #[test]
    fn mtf_matches_schroeder_formula() {
        let sr = 16000.0;
        let t60 = 1.0f64;
        let e: Vec<f64> = (0..(3.0 * sr) as usize).map(|i| 10f64.powf(-6.0 * (i as f64 / sr as f64) / t60)).collect();
        for &f in &[0.63, 2.0, 8.0] {
            let want = 1.0 / (1.0 + (TAU * f * t60 / 13.8155).powi(2)).sqrt();
            assert!((mtf(&e, sr as f64, f) - want).abs() < 0.01, "F {f}");
        }
    }

    #[test]
    fn sti_of_decay_matches_theory() {
        let (sr, t60) = (48000.0, 0.6);
        let ir = noise_decay(sr, t60, 2.0);
        let got = sti(&ir, sr, 0, (1.5 * sr) as usize).unwrap();
        let ti = |t: f64| MOD_F.iter().map(|&f| ti_of(1.0 / (1.0 + (TAU * f * t / 13.8155).powi(2)).sqrt())).sum::<f64>() / 14.0;
        let want = sti_from_mti([ti(t60 as f64) as f32; 7]);
        assert!((got.sti - want.sti).abs() < 0.05, "sti {} want {}", got.sti, want.sti);
        assert!(got.sti > 0.3 && got.sti < 0.8);
    }

    #[test]
    fn band_decay_and_csd_and_modes() {
        let sr = 48000.0;
        let ir = noise_decay(sr, 0.8, 2.0);
        let b = band_decays(&ir, sr);
        let t = b[3].m.as_ref().and_then(|m| m.t30).expect("T30 at 1 kHz");
        assert!((t - 0.8).abs() < 0.15, "T30 {t}");
        let c = csd(&ir, sr, 0, (1.5 * sr) as usize).unwrap();
        assert!(c.db.iter().all(|v| v.is_finite()));
        let mean = |r: usize| c.db[r * c.cols..(r + 1) * c.cols].iter().sum::<f32>() / c.cols as f32;
        assert!(mean(0) > mean(c.rows - 4) + 5.0);
        // 80 Hz resonance with T60 0.5 s
        let ring: Vec<f32> = (0..(2.0 * sr) as usize).map(|i| (TAU as f32 * 80.0 * i as f32 / sr).sin() * 10f32.powf(-3.0 * (i as f32 / sr) / 0.5)).collect();
        let m = modes(&ring, sr, 0).unwrap();
        let p = m.peaks.iter().max_by(|a, b| a.db.total_cmp(&b.db)).unwrap();
        assert!((p.f - 80.0).abs() < 1.0, "f {}", p.f);
        assert!(p.t60 > 0.3 && p.t60 < 0.8, "t60 {}", p.t60);
    }
}
