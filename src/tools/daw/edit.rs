//! Offline audio edits of the timeline: spectral regions (attenuate or remove what is drawn on the spectrogram), click and hum
//! removal, time-stretch and pitch-shift (a phase vocoder with identity phase locking). Plain functions on sample buffers.

use rustfft::{num_complex::Complex32, FftPlanner};
use std::f32::consts::PI;

pub const N: usize = 2048;
const HOP: usize = N / 4;

fn hann() -> Vec<f32> {
    (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / N as f32).cos()).collect()
}

fn sqrt_hann() -> Vec<f32> {
    hann().into_iter().map(|v| v.sqrt()).collect()
}

// ---------------------------------------------------------------------- spectral regions

/// A rectangle on the spectrogram: frames `t0..t1` of the clip, frequencies `f0..f1`, level change in dB (very low = removed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub t0: usize,
    pub t1: usize,
    pub f0: f32,
    pub f1: f32,
    pub gain_db: f32,
}

/// Weight (0..1) of a region at frame time `tc` and bin `k`: the edges are soft (a few frames, a few bins), so nothing clicks.
fn weight(r: &Region, tc: f32, k: usize, bin_hz: f32) -> f32 {
    let ft = 3.0 * HOP as f32;
    let wt = ((tc - r.t0 as f32) / ft + 0.5).clamp(0.0, 1.0).min(((r.t1 as f32 - tc) / ft + 0.5).clamp(0.0, 1.0));
    let f = k as f32 * bin_hz;
    let fw = 3.0 * bin_hz;
    let wf = ((f - r.f0) / fw + 0.5).clamp(0.0, 1.0).min(((r.f1 - f) / fw + 0.5).clamp(0.0, 1.0));
    wt * wf
}

/// Apply the regions to one channel. Only the frames that a region touches are processed; the rest of the signal is untouched.
pub fn apply_regions(x: &[f32], sr: u32, regs: &[Region]) -> Vec<f32> {
    let mut out = x.to_vec();
    if regs.is_empty() || x.is_empty() {
        return out;
    }
    let w = sqrt_hann();
    let mut pl = FftPlanner::<f32>::new();
    let (fwd, inv) = (pl.plan_fft_forward(N), pl.plan_fft_inverse(N));
    let bin_hz = sr as f32 / N as f32;
    let mut buf = vec![Complex32::new(0.0, 0.0); N];
    let mut diff = vec![Complex32::new(0.0, 0.0); N];
    // frames start one window before the signal and go on to one after it (the padding is zeros)
    let frames = (x.len() + N) / HOP + 1;
    for k in 0..frames {
        let pos = k as isize * HOP as isize - N as isize;
        let tc = pos as f32 + N as f32 * 0.5;
        if !regs.iter().any(|r| tc >= r.t0 as f32 - 3.0 * HOP as f32 && tc <= r.t1 as f32 + 3.0 * HOP as f32) {
            continue;
        }
        for i in 0..N {
            let j = pos + i as isize;
            let v = if j >= 0 && (j as usize) < x.len() { x[j as usize] } else { 0.0 };
            buf[i] = Complex32::new(v * w[i], 0.0);
        }
        fwd.process(&mut buf);
        let mut any = false;
        for kk in 0..=N / 2 {
            // product of what the regions do to this bin
            let mut g = 1.0f32;
            for r in regs {
                let wt = weight(r, tc, kk, bin_hz);
                if wt > 0.0 {
                    let rg = 10f32.powf(r.gain_db / 20.0);
                    g *= 1.0 - wt * (1.0 - rg);
                }
            }
            let d = buf[kk] * (g - 1.0);
            any |= g != 1.0;
            diff[kk] = d;
            if kk > 0 && kk < N / 2 {
                diff[N - kk] = d.conj();
            }
        }
        if !any {
            continue;
        }
        inv.process(&mut diff);
        for i in 0..N {
            let j = pos + i as isize;
            if j >= 0 && (j as usize) < out.len() {
                // (sqrt-Hann on both sides, 75 % overlap: the windows add up to 2)
                out[j as usize] += diff[i].re / N as f32 * w[i] * 0.5;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------- clicks and hum

/// Find clicks (short bursts far above the signal's own roughness) and replace them by a smooth bridge. `sens` is the threshold in
/// multiples of the typical second difference (lower finds more); returns the repaired signal and how many clicks it found.
pub fn declick(x: &[f32], sens: f32) -> (Vec<f32>, usize) {
    let n = x.len();
    let mut out = x.to_vec();
    if n < 16 {
        return (out, 0);
    }
    let d2: Vec<f32> = (0..n).map(|i| if i >= 2 { (x[i] - 2.0 * x[i - 1] + x[i - 2]).abs() } else { 0.0 }).collect();
    // the typical size: the median of a thinned copy
    let mut sample: Vec<f32> = d2.iter().step_by((n / 50_000).max(1)).copied().collect();
    sample.sort_by(|a, b| a.total_cmp(b));
    let med = sample[sample.len() / 2];
    let thr = (sens.max(2.0) * 1.4826 * med).max(2e-3);
    let mut clicks = 0;
    let mut i = 2;
    while i < n {
        if d2[i] > thr {
            // the run of flagged samples around it
            let start = i.saturating_sub(3).max(1);
            let mut end = i;
            let mut j = i;
            while j < n && j <= end + 6 {
                if d2[j] > thr {
                    end = j;
                }
                j += 1;
            }
            let end = (end + 1).min(n - 2);
            if end - start <= 100 && start >= 2 && end + 2 < n {
                // cubic Hermite bridge from the sample before to the sample after, with the slopes of the neighbours
                let (a, b) = (start - 1, end + 1);
                let (ya, yb) = (x[a], x[b]);
                let (ma, mb) = ((x[a] - x[a - 1]) * (b - a) as f32, (x[b + 1] - x[b]) * (b - a) as f32);
                for k in start..=end {
                    let t = (k - a) as f32 / (b - a) as f32;
                    let (t2, t3) = (t * t, t * t * t);
                    out[k] = (2.0 * t3 - 3.0 * t2 + 1.0) * ya + (t3 - 2.0 * t2 + t) * ma + (-2.0 * t3 + 3.0 * t2) * yb + (t3 - t2) * mb;
                }
                clicks += 1;
            }
            i = end + 4;
        } else {
            i += 1;
        }
    }
    (out, clicks)
}

/// Remove mains hum: narrow notches at `f0` and its harmonics (as many as fit below 0.45 of the sample rate, at most `harmonics`).
pub fn dehum(x: &[f32], sr: u32, f0: f32, harmonics: usize, q: f32) -> Vec<f32> {
    let mut y = x.to_vec();
    let mut k = 1;
    while k <= harmonics {
        let f = f0 * k as f32;
        if f > 0.45 * sr as f32 {
            break;
        }
        // RBJ notch; run forwards and backwards: no phase change
        let w0 = 2.0 * PI * f / sr as f32;
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2, a1, a2) = (1.0 / a0, -2.0 * w0.cos() / a0, 1.0 / a0, -2.0 * w0.cos() / a0, (1.0 - alpha) / a0);
        for _ in 0..2 {
            let (mut x1, mut x2, mut y1, mut y2) = (0f64, 0f64, 0f64, 0f64);
            for v in y.iter_mut() {
                let xi = *v as f64;
                let yi = b0 as f64 * xi + b1 as f64 * x1 + b2 as f64 * x2 - a1 as f64 * y1 - a2 as f64 * y2;
                (x2, x1, y2, y1) = (x1, xi, y1, yi);
                *v = yi as f32;
            }
            y.reverse();
        }
        k += 1;
    }
    y
}

// ---------------------------------------------------------------------- time-stretch, pitch-shift

fn princarg(x: f32) -> f32 {
    x - 2.0 * PI * (x / (2.0 * PI)).round()
}

/// Change the duration by `ratio` (2.0 = twice as long) and keep the pitch: a phase vocoder; the phases around each spectral peak
/// move together (identity phase locking), which keeps tones clean. Transients are softened somewhat, as in every vocoder.
pub fn stretch(x: &[f32], ratio: f64) -> Vec<f32> {
    let ratio = ratio.clamp(0.1, 10.0);
    if x.len() < N || (ratio - 1.0).abs() < 1e-4 {
        return x.to_vec();
    }
    let hs = HOP as f64;
    let ha = hs / ratio;
    let out_len = (x.len() as f64 * ratio).round() as usize;
    let w = hann();
    let mut pl = FftPlanner::<f32>::new();
    let (fwd, inv) = (pl.plan_fft_forward(N), pl.plan_fft_inverse(N));
    let bins = N / 2 + 1;
    let omega: Vec<f32> = (0..bins).map(|k| 2.0 * PI * k as f32 / N as f32).collect();
    let mut prev_ph = vec![0f32; bins];
    let mut syn_ph = vec![0f32; bins];
    let mut mag = vec![0f32; bins];
    let mut ph = vec![0f32; bins];
    let mut new_ph = vec![0f32; bins];
    let mut buf = vec![Complex32::new(0.0, 0.0); N];
    let mut acc = vec![0f32; out_len + 2 * N];
    let n_frames = ((x.len() as f64 / ha).ceil() as usize) + 2;
    let mut peaks: Vec<usize> = Vec::new();
    for k in 0..n_frames {
        let pa = (k as f64 * ha).round() as isize - (N / 2) as isize;
        for i in 0..N {
            let j = pa + i as isize;
            let v = if j >= 0 && (j as usize) < x.len() { x[j as usize] } else { 0.0 };
            buf[i] = Complex32::new(v * w[i], 0.0);
        }
        fwd.process(&mut buf);
        for b in 0..bins {
            mag[b] = buf[b].norm();
            ph[b] = buf[b].arg();
        }
        // spectral peaks (local maxima over four neighbours) and the regions they own
        peaks.clear();
        for b in 2..bins - 2 {
            if mag[b] > mag[b - 1] && mag[b] > mag[b - 2] && mag[b] >= mag[b + 1] && mag[b] > mag[b + 2] && mag[b] > 1e-6 {
                peaks.push(b);
            }
        }
        if k == 0 {
            syn_ph.copy_from_slice(&ph);
        } else {
            // the phase of each peak moves with its true frequency
            for &p in &peaks {
                let dev = princarg(ph[p] - prev_ph[p] - omega[p] * ha as f32);
                let inst = omega[p] + dev / ha as f32;
                new_ph[p] = syn_ph[p] + inst * hs as f32;
            }
            // the other bins keep their phase relation to the peak they belong to
            let mut pi = 0usize;
            for b in 0..bins {
                if peaks.is_empty() {
                    let dev = princarg(ph[b] - prev_ph[b] - omega[b] * ha as f32);
                    new_ph[b] = syn_ph[b] + (omega[b] + dev / ha as f32) * hs as f32;
                    continue;
                }
                while pi + 1 < peaks.len() && (b as isize - peaks[pi + 1] as isize).abs() < (b as isize - peaks[pi] as isize).abs() {
                    pi += 1;
                }
                let p = peaks[pi];
                if b != p {
                    new_ph[b] = new_ph[p] + (ph[b] - ph[p]);
                }
            }
            syn_ph.copy_from_slice(&new_ph);
        }
        prev_ph.copy_from_slice(&ph);
        for b in 0..bins {
            let c = Complex32::from_polar(mag[b], syn_ph[b]);
            buf[b] = c;
            if b > 0 && b < N / 2 {
                buf[N - b] = c.conj();
            }
        }
        buf[0].im = 0.0;
        buf[N / 2].im = 0.0;
        inv.process(&mut buf);
        let po = (k as f64 * hs).round() as usize;
        for i in 0..N {
            if po + i < acc.len() {
                acc[po + i] += buf[i].re / N as f32 * w[i];
            }
        }
    }
    // (Hann analysis and synthesis at 75 % overlap add up to 1.5); the first half window is the padding
    let off = N / 2;
    acc[off..off + out_len].iter().map(|v| v / 1.5).collect()
}

/// Shift the pitch by `semitones` and keep the duration: stretch, then play back at the matching speed.
pub fn pitch_shift(x: &[f32], semitones: f32) -> Vec<f32> {
    if semitones.abs() < 1e-3 || x.len() < N {
        return x.to_vec();
    }
    let p = 2f64.powf(semitones as f64 / 12.0);
    let s = stretch(x, p);
    // read the stretched signal p times as fast: pitch up by p, back to the original length
    resample_ratio(&s, p, x.len())
}

/// Read `x` at `speed` times the pace (windowed sinc), giving `n` samples.
pub fn resample_ratio(x: &[f32], speed: f64, n: usize) -> Vec<f32> {
    const TAPS: isize = 16;
    let cutoff = (1.0 / speed).min(1.0);
    (0..n)
        .map(|i| {
            let t = i as f64 * speed;
            let c = t.floor() as isize;
            let mut acc = 0f64;
            let mut norm = 0f64;
            for k in (c - TAPS + 1)..=(c + TAPS) {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let d = t - k as f64;
                let a = d * cutoff;
                let sinc = if a.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * a).sin() / (std::f64::consts::PI * a) };
                let win = 0.5 + 0.5 * (std::f64::consts::PI * d / TAPS as f64).cos();
                let wgt = sinc * win * cutoff;
                acc += x[k as usize] as f64 * wgt;
                norm += wgt;
            }
            if norm.abs() > 1e-9 { (acc / norm.max(0.5)) as f32 } else { 0.0 }
        })
        .collect()
}

// ---------------------------------------------------------------------- spectrogram for the editor

/// A spectrogram picture for the editor: `w` columns over the whole signal, `h` rows from `f_lo` to the Nyquist frequency on a
/// log axis (row 0 = highest), brightness 0..255 over `range_db`.
pub fn spectrogram(x: &[f32], sr: u32, w: usize, h: usize, f_lo: f32, range_db: f32) -> Vec<u8> {
    let mut img = vec![0u8; w * h];
    if x.is_empty() || w == 0 || h == 0 {
        return img;
    }
    let win = hann();
    let fft = FftPlanner::<f32>::new().plan_fft_forward(N);
    let mut buf = vec![Complex32::new(0.0, 0.0); N];
    let nyq = sr as f32 * 0.5;
    // row -> bin range
    let row_bin: Vec<(usize, usize)> = (0..h)
        .map(|r| {
            let t0 = 1.0 - (r + 1) as f32 / h as f32;
            let t1 = 1.0 - r as f32 / h as f32;
            let f = |t: f32| f_lo * (nyq / f_lo).powf(t);
            let (b0, b1) = ((f(t0) / nyq * (N / 2) as f32) as usize, (f(t1) / nyq * (N / 2) as f32).ceil() as usize);
            (b0.min(N / 2), b1.clamp(b0.min(N / 2) + 1, N / 2 + 1))
        })
        .collect();
    let mut col = vec![0f32; N / 2 + 1];
    for c in 0..w {
        let centre = ((c as f64 + 0.5) / w as f64 * x.len() as f64) as isize;
        for i in 0..N {
            let j = centre - (N / 2) as isize + i as isize;
            let v = if j >= 0 && (j as usize) < x.len() { x[j as usize] } else { 0.0 };
            buf[i] = Complex32::new(v * win[i], 0.0);
        }
        fft.process(&mut buf);
        for b in 0..=N / 2 {
            col[b] = buf[b].norm() / (N as f32 * 0.25);
        }
        for (r, &(b0, b1)) in row_bin.iter().enumerate() {
            let m = col[b0..b1.min(col.len())].iter().copied().fold(0f32, f32::max);
            let db = 20.0 * m.max(1e-9).log10();
            img[r * w + c] = ((db + range_db) / range_db * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, n: usize, sr: u32, a: f32) -> Vec<f32> {
        (0..n).map(|i| a * (2.0 * PI * f * i as f32 / sr as f32).sin()).collect()
    }

    /// Level (amplitude) of a frequency in a stretch of signal (a single Fourier sum).
    fn level(x: &[f32], f: f32, sr: u32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
            re += *v as f64 * a.cos();
            im += *v as f64 * a.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    #[test]
    fn a_region_removes_what_is_drawn_and_nothing_else() {
        let sr = 48000;
        let n = sr as usize * 2;
        let x: Vec<f32> = tone(1000.0, n, sr, 0.4).iter().zip(tone(5000.0, n, sr, 0.4)).map(|(a, b)| a + b).collect();
        // 4 to 6 kHz, in the second half only
        let r = Region { t0: n / 2, t1: n, f0: 4000.0, f1: 6000.0, gain_db: -80.0 };
        let y = apply_regions(&x, sr, &[r]);
        let (a, b) = (&y[..n / 4], &y[3 * n / 4..n - 4096]);
        assert!(level(a, 5000.0, sr) > 0.39, "the first half is untouched");
        assert!(level(b, 5000.0, sr) < 0.4 * 0.01, "5 kHz is gone in the second half: {}", level(b, 5000.0, sr));
        assert!((level(b, 1000.0, sr) - 0.4).abs() < 0.01, "1 kHz stays: {}", level(b, 1000.0, sr));
        // a gentle change
        let g = apply_regions(&x, sr, &[Region { t0: 0, t1: n, f0: 4000.0, f1: 6000.0, gain_db: -6.0 }]);
        let lv = level(&g[8192..n - 8192], 5000.0, sr);
        assert!((lv - 0.2).abs() < 0.02, "-6 dB: {lv}");
        // no region, no change
        assert_eq!(apply_regions(&x, sr, &[]), x);
    }

    #[test]
    fn clicks_are_bridged_and_the_music_is_not() {
        let sr = 48000;
        let clean = tone(440.0, 48000, sr, 0.3);
        let mut x = clean.clone();
        for &i in &[5000usize, 12000, 30000] {
            x[i] += 0.8;
            x[i + 1] -= 0.5;
        }
        let (y, n) = declick(&x, 12.0);
        assert!(n >= 3, "found {n}");
        let err = y.iter().zip(clean.iter()).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 0.02, "worst error after the repair {err}");
        // a clean tone is left alone
        let (z, n) = declick(&clean, 12.0);
        assert_eq!(n, 0);
        assert_eq!(z, clean);
    }

    #[test]
    fn hum_goes_and_the_tone_stays() {
        let sr = 48000;
        let n = sr as usize * 2;
        let mut x = tone(1000.0, n, sr, 0.3);
        for (k, a) in [(1, 0.2f32), (2, 0.1), (3, 0.1)] {
            for (i, v) in tone(50.0 * k as f32, n, sr, a).iter().enumerate() {
                x[i] += v;
            }
        }
        let y = dehum(&x, sr, 50.0, 6, 30.0);
        let seg = |s: &[f32], f| level(&s[sr as usize / 2..sr as usize * 3 / 2], f, sr);
        assert!(seg(&y, 50.0) < seg(&x, 50.0) * 0.03, "{} -> {}", seg(&x, 50.0), seg(&y, 50.0));
        assert!(seg(&y, 150.0) < seg(&x, 150.0) * 0.03);
        assert!((seg(&y, 1000.0) - 0.3).abs() < 0.01);
    }

    #[test]
    fn stretching_changes_the_length_and_keeps_the_pitch() {
        let sr = 48000;
        let x = tone(440.0, sr as usize * 2, sr, 0.5);
        for ratio in [0.5f64, 0.8, 1.5, 2.0] {
            let y = stretch(&x, ratio);
            assert!((y.len() as f64 - x.len() as f64 * ratio).abs() < 2.0, "length {}", y.len());
            let mid = &y[y.len() / 4..y.len() * 3 / 4];
            let l440 = level(mid, 440.0, sr);
            assert!(l440 > 0.4 && l440 < 0.6, "ratio {ratio}: level at 440 Hz {l440}");
            // not at some other frequency
            assert!(level(mid, 440.0 * ratio as f32, sr) < 0.1, "ratio {ratio}: the pitch moved");
        }
    }

    #[test]
    fn pitch_shift_moves_the_pitch_and_keeps_the_length() {
        let sr = 48000;
        let x = tone(440.0, sr as usize * 2, sr, 0.5);
        for (st, f) in [(12.0f32, 880.0f32), (-12.0, 220.0), (7.0, 659.26)] {
            let y = pitch_shift(&x, st);
            assert_eq!(y.len(), x.len());
            let mid = &y[y.len() / 4..y.len() * 3 / 4];
            let l = level(mid, f, sr);
            assert!(l > 0.4 && l < 0.6, "{st} semitones: level at {f} Hz is {l}");
            assert!(level(mid, 440.0, sr) < 0.1, "{st} semitones: still at 440 Hz");
        }
    }

    #[test]
    fn the_spectrogram_shows_a_tone_in_its_row() {
        let sr = 48000;
        let x = tone(2000.0, sr as usize, sr, 0.5);
        let (w, h) = (100, 120);
        let img = spectrogram(&x, sr, w, h, 20.0, 100.0);
        // the row of 2 kHz on a log axis from 20 Hz to 24 kHz
        let row = ((1.0 - (2000.0f32 / 20.0).ln() / (24000.0f32 / 20.0).ln()) * h as f32) as usize;
        let at = |r: usize| img[r * w + w / 2];
        assert!(at(row) > 200 && at(row) > at(row + 15) + 80 && at(row) > at(row.saturating_sub(15)) + 80, "{} {} {}", at(row), at(row + 15), at(row - 15));
    }
}
