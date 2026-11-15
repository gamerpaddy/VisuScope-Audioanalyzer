//! Tests of `dsp.rs`.
#[test]
fn tremolo_and_wow_flutter_can_work_on_each_channel_on_its_own() {
    let fs = 48000.0;
    // tremolo: opposite phase makes the two gains swing against each other
    let mut t = TremUnit::default();
    t.set(&[5.0, 100.0, 0.0, 1.0], fs);
    let (mut l, mut r) = (vec![1.0f32; 48000], vec![1.0f32; 48000]);
    t.process(&mut l, &mut r);
    let worst = (0..48000).map(|i| (l[i] + r[i] - 1.0).abs()).fold(0f32, f32::max);
    assert!(worst < 0.01, "at 100 % depth and 180 degrees the gains add up to 1: {worst}");
    // linked: the same
    let mut t = TremUnit::default();
    t.set(&[5.0, 100.0, 0.0, 0.0], fs);
    let (mut l, mut r) = (vec![1.0f32; 48000], vec![1.0f32; 48000]);
    t.process(&mut l, &mut r);
    assert_eq!(l, r);
    // wow and flutter: linked channels move together, independent ones do not
    let sig: Vec<f32> = (0..96000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    for (split, same) in [(0.0f32, true), (1.0, false)] {
        let mut j = JitterUnit::new(fs);
        j.set(&[2000.0, 8.0, 1.0, 100.0, split]);
        let (mut l, mut r) = (sig.clone(), sig.clone());
        j.process(&mut l, &mut r);
        let diff = l.iter().zip(r.iter()).skip(20000).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert_eq!(diff < 1e-6, same, "split {split}: difference {diff}");
    }
}

use super::*;

fn sine(f: f32, fs: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / fs).sin()).collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn eq_boosts_its_band_and_leaves_the_rest() {
    let fs = 48000.0;
    let mut eq = EqUnit::default();
    let mut p = vec![6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0];
    p[0] = 0.0;
    p[2] = 12.0;
    eq.set(&p, fs);
    assert!((eq.response_db(fs, 1000.0) - 12.0).abs() < 0.2);
    assert!(eq.response_db(fs, 50.0).abs() < 0.5);
    let mut l = sine(1000.0, fs, 9600, 0.1);
    let mut r = l.clone();
    eq.process(&mut l, &mut r);
    let g = lin_to_db(rms(&l[4800..]) / rms(&sine(1000.0, fs, 4800, 0.1)));
    assert!((g - 12.0).abs() < 0.5, "{g}");
}

#[test]
fn eq_notch_depth_follows_the_gain_and_more_bands_work() {
    let fs = 48000.0;
    let mut eq = EqUnit::default();
    // one notch at 1 kHz, -30 dB deep, plus a ninth band: 9 bands in all
    let mut p: Vec<f32> = Vec::new();
    for b in 0..9 {
        p.extend(if b == 0 { [5.0, 1000.0, -30.0, 4.0] } else if b == 8 { [0.0, 4000.0, 6.0, 1.0] } else { [6.0, 1000.0, 0.0, 1.0] });
    }
    eq.set(&p, fs);
    assert_eq!(eq.bands.len(), 9);
    assert!((eq.response_db(fs, 1000.0) + 30.0).abs() < 1.0, "{}", eq.response_db(fs, 1000.0));
    assert!(eq.response_db(fs, 100.0).abs() < 0.5);
    assert!((eq.response_db(fs, 4000.0) - 6.0).abs() < 0.6);
    // a shallower notch really is shallower
    p[2] = -10.0;
    eq.set(&p, fs);
    assert!((eq.response_db(fs, 1000.0) + 10.0).abs() < 1.0);
}

/// Level (linear amplitude) of one frequency in a signal, by correlation over whole periods.
fn tone_level(x: &[f32], f: f32, fs: f32) -> f32 {
    let (mut re, mut im) = (0f64, 0f64);
    for (i, v) in x.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / fs as f64;
        re += *v as f64 * w.cos();
        im += *v as f64 * w.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
}

#[test]
fn harmonics_node_makes_a_series_of_even_or_odd_harmonics() {
    let fs = 48000.0;
    let run = |even: f32, odd: f32, rich: f32, amp: f32| {
        let mut u = HarmUnit::default();
        u.set(&[even, odd, rich, 0.0], fs);
        let mut l = sine(1000.0, fs, 48000, amp);
        let mut r = l.clone();
        u.process(&mut l, &mut r);
        l[12000..].to_vec()
    };
    let a = 0.8;
    // even: 2nd and 4th (and more), no odd ones
    let e = run(100.0, 0.0, 50.0, a);
    let (h2, h3, h4) = (tone_level(&e, 2000.0, fs), tone_level(&e, 3000.0, fs), tone_level(&e, 4000.0, fs));
    assert!(h2 > 0.2 * a, "2nd {h2}");
    assert!(h4 > 0.03 * a, "4th {h4}");
    assert!(h3 < 0.01 * a, "3rd {h3}");
    // odd: 3rd, 5th, 7th, no even ones
    let o = run(0.0, 100.0, 50.0, a);
    let (o2, o3, o5, o7) = (tone_level(&o, 2000.0, fs), tone_level(&o, 3000.0, fs), tone_level(&o, 5000.0, fs), tone_level(&o, 7000.0, fs));
    assert!(o3 > 0.2 * a, "3rd {o3}");
    assert!(o5 > 0.05 * a && o7 > 0.02 * a, "5th {o5} 7th {o7}");
    assert!(o2 < 0.01 * a, "2nd {o2}");
    // the fundamental stays what it was
    assert!((tone_level(&o, 1000.0, fs) - a).abs() < 0.03, "fundamental {}", tone_level(&o, 1000.0, fs));
    // richness reaches further up the series
    let poor = run(0.0, 100.0, 0.0, a);
    let rich = run(0.0, 100.0, 100.0, a);
    assert!(tone_level(&rich, 9000.0, fs) > tone_level(&poor, 9000.0, fs) * 2.0);
    // a quiet signal gets the same amount of harmonics relative to its level
    let loud = tone_level(&run(0.0, 100.0, 50.0, 0.8), 3000.0, fs) / 0.8;
    let quiet = tone_level(&run(0.0, 100.0, 50.0, 0.01), 3000.0, fs) / 0.01;
    assert!((loud / quiet - 1.0).abs() < 0.2, "loud {loud} quiet {quiet}");
    // nothing added: the input comes back
    let n = run(0.0, 0.0, 50.0, a);
    assert!((tone_level(&n, 1000.0, fs) - a).abs() < 0.01);
}
#[test]
fn distortions_stay_bounded_and_add_harmonics() {
    let fs = 48000.0;
    for shape in 0..4 {
        let mut u = DistUnit::default();
        u.set(&[24.0, shape as f32, 20000.0, 100.0, 0.0], fs);
        let mut l = sine(500.0, fs, 24000, 0.9);
        let mut r = l.clone();
        u.process(&mut l, &mut r);
        let peak = l.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 1.5 && peak > 0.2, "shape {shape} peak {peak}");
        let h3 = tone_level(&l[4000..], 1500.0, fs);
        let h2 = tone_level(&l[4000..], 1000.0, fs);
        if shape == 2 {
            assert!(h2 > 0.02, "asymmetric clipping makes even harmonics: {h2}");
        }
        if shape == 0 || shape == 1 {
            assert!(h3 > 0.02 && h2 < 0.01, "symmetric clipping makes odd harmonics: h2 {h2} h3 {h3}");
        }
    }
    let mut fz = FuzzUnit::default();
    fz.set(&[100.0, 0.0, 8000.0, -6.0], fs);
    let mut l = sine(200.0, fs, 24000, 0.5);
    let mut r = l.clone();
    fz.process(&mut l, &mut r);
    assert!(l.iter().all(|v| v.abs() <= 1.0) && l.iter().any(|v| v.abs() > 0.2));
}

#[test]
fn bitcrusher_jitter_and_tremolo() {
    let fs = 48000.0;
    let mut c = CrushUnit::default();
    c.set(&[3.0, 1.0, 100.0]);
    let mut l = sine(300.0, fs, 4800, 0.9);
    let mut r = l.clone();
    c.process(&mut l, &mut r);
    let mut levels: Vec<i32> = l.iter().map(|v| (v * 1000.0).round() as i32).collect();
    levels.sort();
    levels.dedup();
    assert!(levels.len() <= 8, "3 bits have at most 8 levels, got {}", levels.len());
    // held samples: with a hold of 8 the output changes at most every 8th frame
    let mut c2 = CrushUnit::default();
    c2.set(&[24.0, 8.0, 100.0]);
    let mut l = sine(300.0, fs, 4800, 0.9);
    let mut r = l.clone();
    c2.process(&mut l, &mut r);
    assert!(l.windows(2).filter(|w| w[0] != w[1]).count() <= 4800 / 8 + 1);
    // the deepest setting (50 ms) works and stays finite: a wow of a tape machine
    let mut deep = JitterUnit::new(fs);
    deep.set(&[50000.0, 0.7, 1.0, 100.0]);
    let mut l = sine(1000.0, fs, 48000, 0.5);
    let mut r = l.clone();
    for (a, b) in l.chunks_mut(480).zip(r.chunks_mut(480)) {
        deep.process(a, b);
    }
    assert!(l.iter().all(|v| v.is_finite() && v.abs() <= 0.51));
    // a 50 ms swing at 0.7 Hz bends the pitch by more than 20 %: the tone is smeared, but the sound is all there
    assert!(rms(&l[24000..]) > 0.25, "the sound survives a 50 ms wow: {}", rms(&l[24000..]));
    // jitter with no depth is a plain short delay; with depth it still carries the signal
    let mut j = JitterUnit::new(fs);
    j.set(&[0.0, 8.0, 0.0, 100.0]);
    let mut l = sine(1000.0, fs, 9600, 0.5);
    let mut r = l.clone();
    j.process(&mut l, &mut r);
    assert!((tone_level(&l[2000..], 1000.0, fs) - 0.5).abs() < 0.02);
    j.set(&[800.0, 20.0, 0.0, 100.0]);
    let mut l = sine(1000.0, fs, 9600, 0.5);
    let mut r = l.clone();
    j.process(&mut l, &mut r);
    let clean = tone_level(&l[2000..], 1000.0, fs);
    assert!(clean < 0.5 && clean > 0.1, "jitter smears the tone: {clean}");
    // tremolo at full depth goes to silence at the bottom of its swing
    let mut t = TremUnit::default();
    t.set(&[10.0, 100.0, 0.0], fs);
    let mut l = vec![1.0f32; 4800];
    let mut r = l.clone();
    t.process(&mut l, &mut r);
    let (lo, hi) = l.iter().fold((9f32, -9f32), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(lo < 0.05 && hi > 0.95, "{lo} {hi}");
}
#[test]
fn filters_cut_the_right_side() {
    let fs = 48000.0;
    for (low, f_test, expect_cut) in [(0.0, 100.0, true), (0.0, 4000.0, false), (1.0, 100.0, false), (1.0, 12000.0, true)] {
        let mut fu = FilterUnit::default();
        fu.set(&[low, 1000.0, 1.0], fs);
        let mut l = sine(f_test, fs, 19200, 0.5);
        let mut r = l.clone();
        fu.process(&mut l, &mut r);
        let g = lin_to_db(rms(&l[9600..]) / rms(&sine(f_test, fs, 9600, 0.5)));
        if expect_cut {
            assert!(g < -25.0, "low={low} f={f_test} gain {g}");
        } else {
            assert!(g > -1.0, "low={low} f={f_test} gain {g}");
        }
    }
}

#[test]
fn compressor_reduces_loud_signals_by_the_ratio() {
    let fs = 48000.0;
    let mut c = CompUnit::default();
    c.set(&[-20.0, 4.0, 1.0, 50.0, 0.0, 0.0], fs);
    // static curve: 8 dB over the threshold -> 6 dB less
    assert!((c.curve(-12.0) + 6.0).abs() < 1e-3);
    assert!(c.curve(-30.0).abs() < 1e-6);
    let mut l = sine(1000.0, fs, 48000, 0.5);
    let mut r = l.clone();
    c.process(&mut l, &mut r);
    let out_peak = l[24000..].iter().fold(0f32, |m, v| m.max(v.abs()));
    // 0.5 = -6 dB is 14 dB over: expect about -20 + 14/4 = -16.5 dB
    assert!((lin_to_db(out_peak) + 16.5).abs() < 1.0, "{}", lin_to_db(out_peak));
    assert!(c.gr_db < -5.0);
}

#[test]
fn limiter_never_exceeds_the_ceiling() {
    let fs = 48000.0;
    let mut u = LimiterUnit::default();
    u.set(&[-6.0, 50.0], fs);
    let mut l: Vec<f32> = sine(300.0, fs, 24000, 2.0);
    let mut r = l.clone();
    u.process(&mut l, &mut r);
    let ceil = db_to_lin(-6.0);
    assert!(l.iter().chain(r.iter()).all(|v| v.abs() <= ceil + 1e-6));
    assert!(u.gr_db < -3.0);
    // a quiet signal passes unchanged (after the latency)
    let mut q = sine(300.0, fs, 4800, 0.1);
    let mut qr = q.clone();
    let orig = q.clone();
    let mut u2 = LimiterUnit::default();
    u2.set(&[-6.0, 50.0], fs);
    u2.process(&mut q, &mut qr);
    let la = u2.latency();
    for i in 2000..4000 {
        assert!((q[i] - orig[i - la]).abs() < 1e-3);
    }
}

#[test]
fn gate_closes_on_silence_and_opens_on_signal() {
    let fs = 48000.0;
    let mut g = GateUnit::default();
    g.set(&[-40.0, 1.0, 50.0, -80.0], fs);
    let mut l = vec![0.001f32; 9600];
    let mut r = l.clone();
    g.process(&mut l, &mut r);
    assert!(!g.open && l[9599].abs() < 1e-4);
    let mut l = sine(500.0, fs, 9600, 0.3);
    let mut r = l.clone();
    g.process(&mut l, &mut r);
    assert!(g.open);
}

#[test]
fn delay_shifts_by_the_time() {
    let fs = 48000.0;
    let mut d = DelayUnit::new(1.0, fs);
    d.set(&[10.0, 0.0, 100.0], fs);
    let mut l = vec![0.0f32; 2000];
    l[0] = 1.0;
    let mut r = l.clone();
    d.process(&mut l, &mut r);
    assert_eq!(l.iter().position(|v| v.abs() > 0.5), Some(480));
}

#[test]
fn width_and_utility() {
    let (mut l, mut r) = (vec![1.0f32; 4], vec![-1.0f32; 4]);
    stereo_width(&mut l, &mut r, 0.0, 0.0);
    assert!(l[0].abs() < 1e-6 && r[0].abs() < 1e-6, "width 0 is mono");
    let (mut l, mut r) = (vec![0.5f32; 4], vec![0.25f32; 4]);
    utility(&mut l, &mut r, 4);
    assert_eq!((l[0], r[0]), (0.25, 0.5));
    utility(&mut l, &mut r, 1);
    assert_eq!(r[0], 0.25);
}

/// Level (dB) of a frequency in a signal (a single Fourier sum over the whole thing).
fn level_db(x: &[f32], f: f32, sr: f32) -> f32 {
    let (mut re, mut im) = (0f64, 0f64);
    for (i, v) in x.iter().enumerate() {
        let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    20.0 * (2.0 * (re * re + im * im).sqrt() / x.len() as f64).max(1e-12).log10() as f32
}

#[test]
fn upsampling_has_no_images_and_keeps_the_top_of_the_band() {
    // 48 kHz material played into a 192 kHz graph: a tone of 19 or 21 kHz stays at its level and has no copies above 24 kHz
    for f in [1000.0f32, 15000.0, 19000.0, 21000.0] {
        let x = sine(f, 48000.0, 48000, 0.5);
        let mut rs = Rs::new(48000, 192000);
        rs.push(&x, &x);
        let (mut l, mut r) = (vec![0f32; 190000], vec![0f32; 190000]);
        let n = rs.pull(&mut l, &mut r);
        assert!(n > 180000, "{n}");
        let y = &l[2000..n - 2000];
        let main = level_db(y, f, 192000.0);
        assert!((main - (-6.02)).abs() < 0.3, "{f} Hz comes out at {main} dB");
        // images at 48k - f, 48k + f, 96k - f ...
        for img in [48000.0 - f, 48000.0 + f, 96000.0 - f, 96000.0 + f] {
            let im = level_db(y, img, 192000.0);
            assert!(im < main - 85.0, "{f} Hz: image at {img} Hz is {im} dB (tone {main} dB)");
        }
    }
}

#[test]
fn downsampling_filters_what_the_new_rate_cannot_hold() {
    // 192 kHz into a 48 kHz graph: 30 kHz must not fold back to 18 kHz
    let x = sine(30000.0, 192000.0, 192000, 0.5);
    let mut rs = Rs::new(192000, 48000);
    rs.push(&x, &x);
    let (mut l, mut r) = (vec![0f32; 47000], vec![0f32; 47000]);
    let n = rs.pull(&mut l, &mut r);
    let y = &l[1000..n - 1000];
    let alias = level_db(y, 18000.0, 48000.0);
    assert!(alias < -85.0, "30 kHz folds back to 18 kHz at {alias} dB");
    // and a tone below Nyquist passes
    let x = sine(20000.0, 192000.0, 192000, 0.5);
    let mut rs = Rs::new(192000, 48000);
    rs.push(&x, &x);
    let n = rs.pull(&mut l, &mut r);
    assert!((level_db(&l[1000..n - 1000], 20000.0, 48000.0) + 6.02).abs() < 0.3);
}

#[test]
fn the_resampler_gives_the_same_samples_however_it_is_fed_and_pulled() {
    let x = sine(3000.0, 44100.0, 20000, 0.4);
    let mut a = Rs::new(44100, 96000);
    a.push(&x, &x);
    let (mut la, mut ra) = (vec![0f32; 50000], vec![0f32; 50000]);
    let na = a.pull(&mut la, &mut ra);
    let mut b = Rs::new(44100, 96000);
    let (mut lb, mut rb) = (Vec::new(), Vec::new());
    for chunk in x.chunks(333) {
        b.push(chunk, chunk);
        let mut tl = vec![0f32; 177];
        let mut tr = vec![0f32; 177];
        loop {
            let k = b.pull(&mut tl, &mut tr);
            lb.extend_from_slice(&tl[..k]);
            rb.extend_from_slice(&tr[..k]);
            if k < 177 {
                break;
            }
        }
    }
    let m = na.min(lb.len());
    assert!(m > 40000);
    let diff = (0..m).map(|i| (la[i] - lb[i]).abs()).fold(0f32, f32::max);
    assert!(diff < 1e-5, "{diff}");
}

#[test]
fn resampler_keeps_a_sine_and_the_length() {
    let (from, to) = (44100u32, 48000u32);
    let x = sine(1000.0, from as f32, 44100, 0.5);
    let mut rs = Rs::new(from, to);
    rs.push(&x, &x);
    let (mut l, mut r) = (vec![0f32; 47000], vec![0f32; 47000]);
    let n = rs.pull(&mut l, &mut r);
    assert!(n > 46900 && n <= 48000, "{n}");
    let want = sine(1000.0, to as f32, 48000, 0.5);
    let err = (1000..n - 10).map(|i| (l[i] - want[i]).abs()).fold(0f32, f32::max);
    assert!(err < 0.01, "max error {err}");
    // same rate: plain copy, nothing lost
    let mut same = Rs::new(48000, 48000);
    same.push(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
    let (mut a, mut b) = ([0f32; 5], [0f32; 5]);
    assert_eq!(same.pull(&mut a, &mut b), 3);
    assert_eq!(&a[..3], &[1.0, 2.0, 3.0]);
}

#[test]
fn a_delay_of_zero_is_no_delay() {
    // (it used to give the whole length of the line: half a second on every input node)
    let mut d = DelayUnit::new(0.5, 48000.0);
    d.set_time(0.0, 48000.0);
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    l[0] = 1.0;
    r[3] = -1.0;
    d.process(&mut l, &mut r);
    assert_eq!((l[0], r[3]), (1.0, -1.0));
    // and one sample of delay is one sample
    d.set_time(1000.0 / 48000.0 * 1.01, 48000.0);
    let (mut l, mut r) = (vec![0.0f32; 8], vec![0.0f32; 8]);
    l[2] = 1.0;
    d.process(&mut l, &mut r);
    assert_eq!((l[2], l[3]), (0.0, 1.0));
}

#[test]
fn filter_slopes_of_6_and_18_db() {
    let fs = 48000.0;
    let db = |slope: f32, low: f32, f: f32| {
        let mut u = FilterUnit::default();
        u.set(&[low, 1000.0, slope], fs);
        u.stages.iter().map(|b| b.response_db(fs, f)).sum::<f32>()
    };
    // every Butterworth is 3 dB down at its corner; an octave into the stop band it has lost about 6 dB per order
    for (slope, order) in [(3.0, 1.0), (0.0, 2.0), (4.0, 3.0), (1.0, 4.0)] {
        assert!((db(slope, 1.0, 1000.0) + 3.01).abs() < 0.1, "{slope}: {}", db(slope, 1.0, 1000.0));
        assert!((db(slope, 0.0, 1000.0) + 3.01).abs() < 0.1, "{slope}");
        assert!((db(slope, 0.0, 250.0) + 12.04 * order).abs() < 0.6, "{slope}: {}", db(slope, 0.0, 250.0));
    }
    // two 6 dB sections are a Linkwitz-Riley 12: 6 dB down at the corner
    assert!((2.0 * db(3.0, 1.0, 1000.0) + 6.02).abs() < 0.1);
}
