//! Equaliser design: fit parametric (peaking) filters to a measured response, and turn the result into
//! a linear-phase or minimum-phase FIR, Equalizer APO text and biquad coefficients.

use rustfft::{num_complex::Complex64, FftPlanner};
use std::f64::consts::{LN_2, PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filt {
    pub f: f32,
    pub gain: f32,
    pub q: f32,
}

/// RBJ peaking filter: [b0, b1, b2, a1, a2] with a0 = 1 (y = b0 x + b1 x1 + b2 x2 - a1 y1 - a2 y2).
pub fn coeffs(f: &Filt, sr: f64) -> [f64; 5] {
    let a = 10f64.powf(f.gain as f64 / 40.0);
    let w0 = TAU * f.f as f64 / sr;
    let alpha = w0.sin() / (2.0 * f.q as f64);
    let c = w0.cos();
    let a0 = 1.0 + alpha / a;
    [(1.0 + alpha * a) / a0, -2.0 * c / a0, (1.0 - alpha * a) / a0, -2.0 * c / a0, (1.0 - alpha / a) / a0]
}

/// Response of one biquad at the point `z1 = e^(-jw)` of the unit circle (`z2` its square): the sine and cosine of the frequency
/// are worked out once for all the filters.
fn biquad_gain(c: &[f64; 5], z1: Complex64, z2: Complex64) -> Complex64 {
    (Complex64::new(c[0], 0.0) + z1 * c[1] + z2 * c[2]) / (Complex64::new(1.0, 0.0) + z1 * c[3] + z2 * c[4])
}

/// Combined response of filters whose coefficients are known, at the angular frequency `w`, in dB.
fn response_cs(cs: &[[f64; 5]], w: f64) -> f64 {
    let z1 = Complex64::from_polar(1.0, -w);
    let z2 = z1 * z1;
    cs.iter().map(|c| 20.0 * biquad_gain(c, z1, z2).norm().max(1e-12).log10()).sum()
}

/// Combined response of the filters at `f` in dB.
pub fn response_db(filts: &[Filt], sr: f64, f: f64) -> f64 {
    let cs: Vec<[f64; 5]> = filts.iter().map(|x| coeffs(x, sr)).collect();
    response_cs(&cs, TAU * f / sr)
}

/// Greedy fit: repeatedly put a peaking filter on the largest remaining error of `desired` (dB, the
/// correction wanted at each frequency of `f`), sized to the width of that error.
pub fn fit(f: &[f32], desired: &[f32], n: usize, max_boost: f32, max_cut: f32, qmax: f32, band: (f32, f32), sr: f32) -> Vec<Filt> {
    let inb = |fr: f32| fr >= band.0 && fr <= band.1;
    let mut res: Vec<f32> = f.iter().zip(desired).map(|(&fr, &d)| if inb(fr) && d.is_finite() { d.clamp(-max_cut, max_boost) } else { 0.0 }).collect();
    let mut out: Vec<Filt> = Vec::new();
    for _ in 0..n {
        let (k, v) = res.iter().enumerate().fold((0, 0f32), |m, (i, &v)| if v.abs() > m.1.abs() { (i, v) } else { m });
        if v.abs() < 0.4 {
            break;
        }
        let half = v.abs() * 0.5;
        let s = v.signum();
        let (mut lo, mut hi) = (k, k);
        while lo > 0 && res[lo - 1] * s > half {
            lo -= 1;
        }
        while hi + 1 < res.len() && res[hi + 1] * s > half {
            hi += 1;
        }
        // half-gain bandwidth in octaves (at least one grid step either side)
        let bw = ((f[(hi + 1).min(f.len() - 1)] / f[lo.saturating_sub(1)]).log2() as f64).max(0.12);
        let w0 = TAU * f[k] as f64 / sr as f64;
        let q = (1.0 / (2.0 * (LN_2 / 2.0 * bw * w0 / w0.sin()).sinh())).clamp(0.5, qmax as f64) as f32;
        let filt = Filt { f: f[k], gain: v.clamp(-max_cut, max_boost), q };
        // (the coefficients of the new filter are the same for every point: once, not once per point)
        let cs = [coeffs(&filt, sr as f64)];
        for (i, r) in res.iter_mut().enumerate() {
            if inb(f[i]) {
                *r -= response_cs(&cs, TAU * f[i] as f64 / sr as f64) as f32;
            }
        }
        out.push(filt);
    }
    out.sort_by(|a, b| a.f.total_cmp(&b.f));
    out
}

/// Largest boost of the combined filters over 20 Hz - 20 kHz, dB (>= 0): the preamp needed to avoid clipping.
pub fn peak_boost(filts: &[Filt], sr: f64) -> f32 {
    let cs: Vec<[f64; 5]> = filts.iter().map(|x| coeffs(x, sr)).collect();
    (0..600).map(|i| response_cs(&cs, TAU * (20.0 * 1000f64.powf(i as f64 / 599.0)) / sr)).fold(0.0f64, f64::max) as f32
}

fn desired_mag(filts: &[Filt], sr: f64, nfft: usize) -> Vec<f64> {
    let cs: Vec<[f64; 5]> = filts.iter().map(|x| coeffs(x, sr)).collect();
    (0..=nfft / 2)
        .map(|k| {
            let f = (k as f64 * sr / nfft as f64).max(1.0);
            10f64.powf(response_cs(&cs, TAU * f / sr) / 20.0)
        })
        .collect()
}

fn ifft_real(spec: &[Complex64], nfft: usize) -> Vec<f64> {
    let mut b: Vec<Complex64> = vec![Complex64::new(0.0, 0.0); nfft];
    for k in 0..=nfft / 2 {
        b[k] = spec[k];
        if k > 0 && k < nfft / 2 {
            b[nfft - k] = spec[k].conj();
        }
    }
    FftPlanner::<f64>::new().plan_fft_inverse(nfft).process(&mut b);
    b.iter().map(|v| v.re / nfft as f64).collect()
}

/// Linear-phase FIR (delay = taps / 2 samples) with the magnitude response of the filters.
pub fn fir_linear(filts: &[Filt], sr: f64, taps: usize) -> Vec<f32> {
    let nfft = (taps * 4).next_power_of_two();
    let spec: Vec<Complex64> = desired_mag(filts, sr, nfft).into_iter().map(|m| Complex64::new(m, 0.0)).collect();
    let h = ifft_real(&spec, nfft);
    (0..taps)
        .map(|i| {
            let w = 0.5 - 0.5 * (TAU * i as f64 / (taps - 1) as f64).cos();
            (h[(i + nfft - taps / 2) % nfft] * w) as f32
        })
        .collect()
}

/// Minimum-phase FIR via the real cepstrum: same magnitude, least delay.
pub fn fir_minphase(filts: &[Filt], sr: f64, taps: usize) -> Vec<f32> {
    let m = (taps * 8).next_power_of_two();
    let logmag: Vec<Complex64> = desired_mag(filts, sr, m).into_iter().map(|v| Complex64::new(v.max(1e-9).ln(), 0.0)).collect();
    let cep = ifft_real(&logmag, m);
    let mut folded = vec![Complex64::new(0.0, 0.0); m];
    folded[0] = Complex64::new(cep[0], 0.0);
    for n in 1..m / 2 {
        folded[n] = Complex64::new(2.0 * cep[n], 0.0);
    }
    folded[m / 2] = Complex64::new(cep[m / 2], 0.0);
    FftPlanner::<f64>::new().plan_fft_forward(m).process(&mut folded);
    // the exponential of the folded cepstrum's spectrum is the minimum-phase spectrum on the full circle
    let mut full: Vec<Complex64> = folded.iter().map(|c| c.exp()).collect();
    FftPlanner::<f64>::new().plan_fft_inverse(m).process(&mut full);
    (0..taps)
        .map(|i| {
            // half-Hann fade over the last quarter
            let t0 = taps * 3 / 4;
            let w = if i > t0 { 0.5 + 0.5 * (PI * (i - t0) as f64 / (taps - t0) as f64).cos() } else { 1.0 };
            (full[i].re / m as f64 * w) as f32
        })
        .collect()
}

/// Equalizer APO configuration (`Preamp` plus one `PK` line per filter).
pub fn apo_text(filts: &[Filt], sr: f64) -> String {
    let mut s = format!("Preamp: {:.1} dB\n", -peak_boost(filts, sr));
    for (i, f) in filts.iter().enumerate() {
        s += &format!("Filter {}: ON PK Fc {:.1} Hz Gain {:.1} dB Q {:.2}\n", i + 1, f.f, f.gain, f.q);
    }
    s
}

pub fn biquad_csv(filts: &[Filt], sr: f64) -> String {
    let mut s = format!("# peaking biquads at {sr} Hz: y = b0 x + b1 x1 + b2 x2 - a1 y1 - a2 y2\nfc_hz,gain_db,q,b0,b1,b2,a1,a2\n");
    for f in filts {
        let c = coeffs(f, sr);
        s += &format!("{:.2},{:.2},{:.3},{:.10},{:.10},{:.10},{:.10},{:.10}\n", f.f, f.gain, f.q, c[0], c[1], c[2], c[3], c[4]);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the code as it was: the coefficients of every filter and the sine and cosine of the frequency were worked out again at every point

    fn old_biquad_gain(c: &[f64; 5], w: f64) -> Complex64 {
        let z1 = Complex64::from_polar(1.0, -w);
        let z2 = z1 * z1;
        (Complex64::new(c[0], 0.0) + z1 * c[1] + z2 * c[2]) / (Complex64::new(1.0, 0.0) + z1 * c[3] + z2 * c[4])
    }

    fn old_response_db(filts: &[Filt], sr: f64, f: f64) -> f64 {
        let w = TAU * f / sr;
        filts.iter().map(|x| 20.0 * old_biquad_gain(&coeffs(x, sr), w).norm().max(1e-12).log10()).sum()
    }

    fn old_peak_boost(filts: &[Filt], sr: f64) -> f32 {
        (0..600).map(|i| old_response_db(filts, sr, 20.0 * 1000f64.powf(i as f64 / 599.0))).fold(0.0f64, f64::max) as f32
    }

    fn old_desired_mag(filts: &[Filt], sr: f64, nfft: usize) -> Vec<f64> {
        (0..=nfft / 2)
            .map(|k| {
                let f = (k as f64 * sr / nfft as f64).max(1.0);
                10f64.powf(old_response_db(filts, sr, f) / 20.0)
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn old_fit(f: &[f32], desired: &[f32], n: usize, max_boost: f32, max_cut: f32, qmax: f32, band: (f32, f32), sr: f32) -> Vec<Filt> {
        let inb = |fr: f32| fr >= band.0 && fr <= band.1;
        let mut res: Vec<f32> = f.iter().zip(desired).map(|(&fr, &d)| if inb(fr) && d.is_finite() { d.clamp(-max_cut, max_boost) } else { 0.0 }).collect();
        let mut out: Vec<Filt> = Vec::new();
        for _ in 0..n {
            let (k, v) = res.iter().enumerate().fold((0, 0f32), |m, (i, &v)| if v.abs() > m.1.abs() { (i, v) } else { m });
            if v.abs() < 0.4 {
                break;
            }
            let half = v.abs() * 0.5;
            let s = v.signum();
            let (mut lo, mut hi) = (k, k);
            while lo > 0 && res[lo - 1] * s > half {
                lo -= 1;
            }
            while hi + 1 < res.len() && res[hi + 1] * s > half {
                hi += 1;
            }
            let bw = ((f[(hi + 1).min(f.len() - 1)] / f[lo.saturating_sub(1)]).log2() as f64).max(0.12);
            let w0 = TAU * f[k] as f64 / sr as f64;
            let q = (1.0 / (2.0 * (LN_2 / 2.0 * bw * w0 / w0.sin()).sinh())).clamp(0.5, qmax as f64) as f32;
            let filt = Filt { f: f[k], gain: v.clamp(-max_cut, max_boost), q };
            for (i, r) in res.iter_mut().enumerate() {
                if inb(f[i]) {
                    *r -= old_response_db(&[filt], sr as f64, f[i] as f64) as f32;
                }
            }
            out.push(filt);
        }
        out.sort_by(|a, b| a.f.total_cmp(&b.f));
        out
    }

    fn some_filters(n: usize) -> Vec<Filt> {
        (0..n).map(|i| Filt { f: 40.0 * 1.6f32.powi(i as i32), gain: if i % 2 == 0 { 5.0 - i as f32 * 0.7 } else { -4.0 + i as f32 * 0.5 }, q: 0.7 + 0.45 * i as f32 }).collect()
    }

    /// The faster evaluation gives bit for bit what the old one gave: the response, the preamp, the target of the FIRs, and the fit.
    #[test]
    fn hoisted_coefficients_change_nothing() {
        for n in [1usize, 3, 10] {
            let filts = some_filters(n);
            for sr in [44100.0f64, 48000.0, 96000.0] {
                for f in [20.0, 63.5, 1000.0, 8000.0, 19999.0] {
                    assert_eq!(response_db(&filts, sr, f), old_response_db(&filts, sr, f), "{n} filters, {sr} Hz, at {f}");
                }
                assert_eq!(peak_boost(&filts, sr), old_peak_boost(&filts, sr));
                assert!(desired_mag(&filts, sr, 4096) == old_desired_mag(&filts, sr, 4096));
            }
        }
        assert_eq!(response_db(&[], 48000.0, 1000.0), old_response_db(&[], 48000.0, 1000.0));
        // the fit, on a measurement made of a few humps and a ripple
        let f = grid();
        let desired: Vec<f32> = f.iter().enumerate().map(|(i, &x)| -(6.0 * (-((x / 90.0).ln()).powi(2) * 8.0).exp() + 3.0 * (-((x / 700.0).ln()).powi(2) * 3.0).exp() + 1.5 * ((i as f32) * 0.7).sin())).collect();
        for n in [2usize, 6, 12] {
            assert_eq!(fit(&f, &desired, n, 6.0, 12.0, 10.0, (30.0, 8000.0), 48000.0), old_fit(&f, &desired, n, 6.0, 12.0, 10.0, (30.0, 8000.0), 48000.0));
        }
        let filts = some_filters(5);
        assert!(fir_linear(&filts, 48000.0, 1024) == {
            // the FIR through the old target
            let nfft = (1024usize * 4).next_power_of_two();
            let spec: Vec<Complex64> = old_desired_mag(&filts, 48000.0, nfft).into_iter().map(|m| Complex64::new(m, 0.0)).collect();
            let h = ifft_real(&spec, nfft);
            (0..1024usize).map(|i| (h[(i + nfft - 512) % nfft] * (0.5 - 0.5 * (TAU * i as f64 / 1023.0).cos())) as f32).collect::<Vec<f32>>()
        });
    }

    /// Cost of the preamp estimate (the Equaliser window works it out in every frame) and of the FIR design, old against new
    /// (cargo test --locked eq_cost -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn eq_cost() {
        let filts = some_filters(12);
        let t = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(old_peak_boost(&filts, 48000.0));
        }
        let old = t.elapsed() / 200;
        let t = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(peak_boost(&filts, 48000.0));
        }
        println!("peak_boost, 12 filters: old {old:?}, new {:?}", t.elapsed() / 200);
        let t = std::time::Instant::now();
        std::hint::black_box(old_desired_mag(&filts, 48000.0, 65536));
        let old = t.elapsed();
        let t = std::time::Instant::now();
        std::hint::black_box(desired_mag(&filts, 48000.0, 65536));
        println!("target of an 8192 tap FIR, 12 filters: old {old:?}, new {:?}", t.elapsed());
    }

    fn grid() -> Vec<f32> {
        (0..400).map(|i| 20.0 * 1000f32.powf(i as f32 / 399.0)).collect()
    }

    /// Two room-mode like peaks (+7 dB at 80 Hz, +4 dB at 250 Hz): the fit must flatten them.
    #[test]
    fn fit_flattens_peaks() {
        let f = grid();
        let peaks = [Filt { f: 80.0, gain: 7.0, q: 4.0 }, Filt { f: 250.0, gain: 4.0, q: 2.0 }];
        let measured: Vec<f32> = f.iter().map(|&x| response_db(&peaks, 48000.0, x as f64) as f32).collect();
        let desired: Vec<f32> = measured.iter().map(|m| -m).collect();
        let eq = fit(&f, &desired, 6, 6.0, 12.0, 10.0, (30.0, 500.0), 48000.0);
        assert!(!eq.is_empty() && eq.len() <= 6);
        let rms = |v: Vec<f32>| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let band: Vec<usize> = (0..f.len()).filter(|&i| f[i] >= 30.0 && f[i] <= 500.0).collect();
        let before = rms(band.iter().map(|&i| measured[i]).collect());
        let after = rms(band.iter().map(|&i| measured[i] + response_db(&eq, 48000.0, f[i] as f64) as f32).collect());
        assert!(after < before * 0.25 && after < 0.8, "rms before {before} after {after}");
        assert!(eq.iter().all(|x| x.gain < 1.0), "no real boosts needed: {eq:?}");
    }

    fn fir_mag_db(h: &[f32], sr: f64, f: f64) -> f64 {
        let w = TAU * f / sr;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in h.iter().enumerate() {
            re += v as f64 * (w * i as f64).cos();
            im -= v as f64 * (w * i as f64).sin();
        }
        20.0 * (re * re + im * im).sqrt().log10()
    }

    #[test]
    fn firs_match_the_filters() {
        let sr = 48000.0;
        let eq = [Filt { f: 100.0, gain: -6.0, q: 1.5 }, Filt { f: 1000.0, gain: 4.0, q: 1.5 }, Filt { f: 6000.0, gain: -3.0, q: 2.0 }];
        for (name, h) in [("linear", fir_linear(&eq, sr, 8192)), ("minphase", fir_minphase(&eq, sr, 8192))] {
            for f in [60.0, 100.0, 200.0, 1000.0, 3000.0, 6000.0, 12000.0] {
                let want = response_db(&eq, sr, f);
                let got = fir_mag_db(&h, sr, f);
                assert!((got - want).abs() < 0.4, "{name} at {f} Hz: {got} vs {want}");
            }
        }
        // the minimum-phase filter puts its energy at the start, the linear-phase one in the middle
        let (lin, min) = (fir_linear(&eq, sr, 2048), fir_minphase(&eq, sr, 2048));
        let peak = |h: &[f32]| h.iter().enumerate().fold((0, 0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m }).0;
        assert!(peak(&min) < 50 && (peak(&lin) as i32 - 1024).abs() < 50, "peaks {} {}", peak(&min), peak(&lin));
    }

    #[test]
    fn text_exports() {
        let eq = [Filt { f: 100.0, gain: -6.0, q: 3.0 }, Filt { f: 1000.0, gain: 4.0, q: 1.5 }];
        let t = apo_text(&eq, 48000.0);
        assert!(t.starts_with("Preamp: -") && t.contains("Filter 2: ON PK Fc 1000.0 Hz Gain 4.0 dB Q 1.50"));
        assert_eq!(biquad_csv(&eq, 48000.0).lines().count(), 4);
    }
}
