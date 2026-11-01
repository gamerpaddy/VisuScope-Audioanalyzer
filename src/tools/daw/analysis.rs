//! Offline analysis of a rendered range: level statistics, loudness, averaged spectrum and a spectrogram.

use crate::tools::loudness::Engine;
use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Clone, Default)]
pub struct Stats {
    pub secs: f32,
    pub peak_db: [f32; 2],
    pub rms_db: [f32; 2],
    pub dc: [f32; 2],
    pub true_peak_db: f32,
    pub lufs: Option<f32>,
    pub lra: Option<f32>,
    pub clipped: usize,
    pub silence_pct: f32,
    /// L/R correlation (-1..1), 1 for mono material
    pub corr: f32,
    pub crest_db: f32,
}

#[derive(Clone, Default)]
pub struct AnalysisOut {
    pub stats: Stats,
    pub sr: u32,
    /// log spaced frequencies and the averaged level (dBFS) of each channel
    pub f: Vec<f32>,
    pub db: [Vec<f32>; 2],
    /// spectrogram, columns x rows (row 0 = lowest frequency), intensity 0..255
    pub spg: Vec<u8>,
    pub spg_w: usize,
    pub spg_h: usize,
    pub truncated: bool,
}

pub const SPG_DB_RANGE: f32 = 100.0;
pub const F_LO: f32 = 20.0;

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

pub fn stats(l: &[f32], r: &[f32], sr: u32) -> Stats {
    let n = l.len().min(r.len());
    let mut s = Stats { secs: n as f32 / sr as f32, true_peak_db: -120.0, ..Default::default() };
    if n == 0 {
        s.peak_db = [-120.0; 2];
        s.rms_db = [-120.0; 2];
        return s;
    }
    let (mut pk, mut sq, mut sum) = ([0f32; 2], [0f64; 2], [0f64; 2]);
    let mut cross = 0f64;
    for i in 0..n {
        for (c, v) in [l[i], r[i]].into_iter().enumerate() {
            pk[c] = pk[c].max(v.abs());
            sq[c] += (v as f64) * (v as f64);
            sum[c] += v as f64;
            if v.abs() >= 0.9999 {
                s.clipped += 1;
            }
        }
        cross += l[i] as f64 * r[i] as f64;
    }
    for c in 0..2 {
        s.peak_db[c] = db(pk[c]);
        s.rms_db[c] = db((sq[c] / n as f64).sqrt() as f32);
        s.dc[c] = (sum[c] / n as f64) as f32;
    }
    let den = (sq[0] * sq[1]).sqrt();
    s.corr = if den > 1e-12 { (cross / den) as f32 } else { 1.0 };
    s.crest_db = s.peak_db[0].max(s.peak_db[1]) - 10.0 * (((sq[0] + sq[1]) / (2.0 * n as f64)).max(1e-18)).log10() as f32 * 1.0;
    // loudness engine (K-weighted, gated)
    if n as f32 / sr as f32 >= 0.4 {
        let mut e = Engine::new(sr);
        for (a, b) in l[..n].chunks(4800).zip(r[..n].chunks(4800)) {
            e.process(a, b);
        }
        s.lufs = e.integrated();
        s.lra = e.lra();
        s.true_peak_db = e.tp_db[0].max(e.tp_db[1]);
    } else {
        s.true_peak_db = s.peak_db[0].max(s.peak_db[1]);
    }
    // share of 50 ms windows quieter than -60 dBFS (dead air)
    let w = (sr as usize / 20).max(1);
    let (mut quiet, mut total) = (0usize, 0usize);
    for (a, b) in l[..n].chunks(w).zip(r[..n].chunks(w)) {
        let e = a.iter().zip(b).map(|(x, y)| x * x + y * y).sum::<f32>() / (2 * a.len()) as f32;
        total += 1;
        if e < 1e-6 {
            quiet += 1;
        }
    }
    s.silence_pct = 100.0 * quiet as f32 / total.max(1) as f32;
    s
}

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).collect()
}

/// Averaged spectrum (power average over Hann windowed frames, 50 % overlap) of each channel as dBFS per
/// bin; a full-scale sine reads 0 dB.
pub fn avg_spectrum(x: [&[f32]; 2], n: usize) -> [Vec<f32>; 2] {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let win = hann(n);
    let norm = n as f32 * 0.25;
    let mut out = [vec![0f32; n / 2 + 1], vec![0f32; n / 2 + 1]];
    for c in 0..2 {
        let d = x[c];
        let mut acc = vec![0f64; n / 2 + 1];
        let mut frames = 0usize;
        let mut buf = vec![Complex::new(0f32, 0f32); n];
        let hop = n / 2;
        let mut start = 0;
        loop {
            if start + n > d.len() && frames > 0 {
                break;
            }
            for i in 0..n {
                let v = d.get(start + i).copied().unwrap_or(0.0);
                buf[i] = Complex::new(v * win[i], 0.0);
            }
            fft.process(&mut buf);
            for k in 0..=n / 2 {
                acc[k] += (buf[k].norm_sqr()) as f64;
            }
            frames += 1;
            start += hop;
            if start >= d.len() {
                break;
            }
        }
        for k in 0..=n / 2 {
            let p = (acc[k] / frames.max(1) as f64).sqrt() as f32 / norm;
            out[c][k] = db(p);
        }
    }
    out
}

/// Resample a linear-frequency spectrum onto `count` log spaced points (maximum where a point covers
/// several bins, interpolation where it lies between two).
pub fn log_points(spec: &[f32], sr: u32, f0: f32, f1: f32, count: usize) -> (Vec<f32>, Vec<f32>) {
    let n = (spec.len() - 1) * 2;
    let bin = sr as f32 / n as f32;
    let ratio = (f1 / f0).powf(1.0 / count as f32);
    let (mut f, mut v) = (Vec::new(), Vec::new());
    for i in 0..count {
        let (a, b) = (f0 * ratio.powi(i as i32), f0 * ratio.powi(i as i32 + 1));
        let (ka, kb) = (a / bin, b / bin);
        if ka >= (spec.len() - 1) as f32 {
            break;
        }
        let val = if kb - ka < 1.0 {
            let mid = 0.5 * (ka + kb);
            let k = mid.floor() as usize;
            let t = mid - k as f32;
            spec[k] * (1.0 - t) + spec[(k + 1).min(spec.len() - 1)] * t
        } else {
            // the strongest bin: tones keep their level, like the other spectrum displays
            let (lo, hi) = (ka.ceil() as usize, (kb.floor() as usize).min(spec.len() - 1));
            spec[lo..=hi.max(lo)].iter().cloned().fold(f32::MIN, f32::max)
        };
        f.push((a * b).sqrt());
        v.push(val);
    }
    (f, v)
}

/// Spectrogram of the mono mix: `w` columns, `h` log-frequency rows (lowest first), 0..255 over `SPG_DB_RANGE`.
pub fn spectrogram(l: &[f32], r: &[f32], sr: u32, w: usize, h: usize) -> Vec<u8> {
    let n = 2048usize;
    let mut out = vec![0u8; w * h];
    if l.len() < 64 {
        return out;
    }
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let win = hann(n);
    let norm = n as f32 * 0.25;
    let f1 = (sr as f32 * 0.5).min(20000.0);
    let bin = sr as f32 / n as f32;
    let ratio = (f1 / F_LO).powf(1.0 / h as f32);
    let mut buf = vec![Complex::new(0f32, 0f32); n];
    let total = l.len().min(r.len());
    for x in 0..w {
        let centre = ((x as f64 + 0.5) / w as f64 * total as f64) as isize;
        let s0 = centre - (n / 2) as isize;
        for i in 0..n {
            let j = s0 + i as isize;
            let v = if j >= 0 && (j as usize) < total { 0.5 * (l[j as usize] + r[j as usize]) } else { 0.0 };
            buf[i] = Complex::new(v * win[i], 0.0);
        }
        fft.process(&mut buf);
        for y in 0..h {
            let (a, b) = (F_LO * ratio.powi(y as i32), F_LO * ratio.powi(y as i32 + 1));
            let (ka, kb) = ((a / bin).floor() as usize, ((b / bin).ceil() as usize).max((a / bin).floor() as usize + 1));
            let mut m = 0f32;
            for k in ka.min(n / 2)..kb.min(n / 2 + 1) {
                m = m.max(buf[k].norm_sqr());
            }
            let d = db(m.sqrt() / norm);
            out[x * h + y] = (((d + SPG_DB_RANGE) / SPG_DB_RANGE).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    out
}

/// Everything at once for a stereo rendering at `sr`.
pub fn analyze(l: Vec<f32>, r: Vec<f32>, sr: u32, truncated: bool) -> AnalysisOut {
    let st = stats(&l, &r, sr);
    let n = if l.len() >= 1 << 15 { 1 << 14 } else if l.len() >= 1 << 12 { 1 << 12 } else { 1 << 10 };
    let spec = avg_spectrum([&l, &r], n);
    let f1 = (sr as f32 * 0.5).min(20000.0);
    let (f, a) = log_points(&spec[0], sr, F_LO, f1, 480);
    let (_, b) = log_points(&spec[1], sr, F_LO, f1, 480);
    let (spg_w, spg_h) = (400, 140);
    let spg = spectrogram(&l, &r, sr, spg_w, spg_h);
    AnalysisOut { stats: st, sr, f, db: [a, b], spg, spg_w, spg_h, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, amp: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / sr).sin()).collect()
    }

    #[test]
    fn sine_reads_its_level() {
        let sr = 48000;
        let x = sine(1000.0, 0.5, 96000, sr as f32);
        let a = analyze(x.clone(), x, sr, false);
        // -6 dBFS peak, -9 dBFS rms
        assert!((a.stats.peak_db[0] + 6.02).abs() < 0.1, "{}", a.stats.peak_db[0]);
        assert!((a.stats.rms_db[0] + 9.03).abs() < 0.1);
        assert!((a.stats.corr - 1.0).abs() < 1e-3);
        let (k, _) = a.f.iter().enumerate().min_by(|a, b| (a.1 - 1000.0).abs().total_cmp(&(b.1 - 1000.0).abs())).unwrap();
        let peak = a.db[0][k.saturating_sub(2)..k + 3].iter().cloned().fold(f32::MIN, f32::max);
        assert!((peak + 6.02).abs() < 1.5, "spectrum peak {peak}");
        assert!(a.stats.lufs.is_some());
        assert_eq!(a.spg.len(), a.spg_w * a.spg_h);
        assert!(a.spg.iter().any(|&v| v > 200));
    }

    #[test]
    fn silence_is_detected() {
        let z = vec![0f32; 48000];
        let s = stats(&z, &z, 48000);
        assert!(s.silence_pct > 99.0);
        assert!(s.peak_db[0] <= -100.0);
    }
}
