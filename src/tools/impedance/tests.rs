//! Tests of `impedance.rs`.
use super::*;

#[test]
fn the_voltage_divider_gives_the_impedance() {
    // a pure resistor of 8 ohm behind 100 ohm: H = 8 / 108
    let h = (8.0 / 108.0, 0.0);
    let z = z_from_h(h, 100.0);
    assert!((z.0 - 8.0).abs() < 1e-3 && z.1.abs() < 1e-4, "{z:?}");
    // a coil: Z = 8 + j 6 ohm behind 100: H = Z / (100 + Z)
    let (zr, zx) = (8.0f32, 6.0f32);
    let (a, b) = (100.0 + zr, zx);
    let d = a * a + b * b;
    let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
    let z = z_from_h(h, 100.0);
    assert!((z.0 - 8.0).abs() < 1e-2 && (z.1 - 6.0).abs() < 1e-2, "{z:?}");
    // a capacitor (negative reactance) is negative
    let (zr, zx) = (1.0f32, -20.0f32);
    let (a, b) = (100.0 + zr, zx);
    let d = a * a + b * b;
    let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
    let z = z_from_h(h, 100.0);
    assert!(z.1 < -19.0, "{z:?}");
}

#[test]
fn the_two_point_calibration_finds_the_gain_and_the_series_resistance() {
    // a box whose input reads 0.8 of the true ratio, with 47 ohm in series: open gives 0.8, 1 kΩ gives 0.8 * 1000 / 1047
    let (gain_true, rs_true) = (0.8f32, 47.0f32);
    let open = gain_true * 1.0;
    let with_r = gain_true * 1000.0 / (rs_true + 1000.0);
    let (g, rs) = two_point_calibration(open, with_r, 1000.0).unwrap();
    assert!((g - 1.0 / gain_true).abs() < 1e-4, "{g}");
    assert!((rs - rs_true).abs() < 0.05, "{rs}");
    // nonsense (the resistor reads higher than open) is refused
    assert!(two_point_calibration(0.5, 0.7, 1000.0).is_none());
    // the impedance of a speaker with the found values
    let h = (6.0 / (rs + 6.0), 0.0);
    let z = z_from_h(h, rs);
    assert!((z.0 - 6.0).abs() < 0.05, "{z:?}");
}

/// A sweep result whose transfer function is the one of the impedance `z(f)` behind the DATS series resistor.
fn fake_result(z: impl Fn(f32) -> (f32, f32)) -> SweepResult {
    let df = 5.0f32;
    let n = 4800;
    let (mut mag, mut phase) = (Vec::new(), Vec::new());
    for k in 0..n {
        let f = (k as f32 * df).max(1.0);
        let (zr, zx) = z(f);
        // H = Z / (Rs + Z), divided by the gain of the box
        let (a, b) = (DATS_RS + zr, zx);
        let d = a * a + b * b;
        let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
        let h = (h.0 / DATS_GAIN, h.1 / DATS_GAIN);
        mag.push(20.0 * (h.0 * h.0 + h.1 * h.1).sqrt().max(1e-9).log10());
        phase.push(h.1.atan2(h.0));
    }
    let t = super::super::sweep::Transfer { df, mag_db: mag, phase, coh: None };
    SweepResult { harm: None, sr: 48000.0, ir: Vec::new(), pre: 0, h: t, runs: 1, capture_peak_db: -20.0, params: super::super::sweep::SweepParams { f0: 20.0, f1: 20000.0, ..Default::default() }, id: 1 }
}

#[test]
fn the_short_of_the_leads_is_taken_off_the_measurements() {
    // leads: 0.48 ohm and 5 microhenry; a coil of 8 ohm and 0.5 mH behind them
    let leads = |f: f32| (0.48f32, 2.0 * std::f32::consts::PI * f * 5e-6);
    let dut = |f: f32| (8.0f32, 2.0 * std::f32::consts::PI * f * 0.5e-3);
    let total = |f: f32| {
        let (a, b) = (leads(f), dut(f));
        (a.0 + b.0, a.1 + b.1)
    };
    let setup = Setup { r_ref: DATS_RS, r_out: 0.0, gain: DATS_GAIN };
    let sh = fake_result(leads);
    let (f, z) = z_points(&sh, setup);
    let short = ZShort { f, re: z.iter().map(|v| v.0).collect(), im: z.iter().map(|v| v.1).collect() };
    assert!((short.at(1000.0).0 - 0.48).abs() < 0.01, "{:?}", short.at(1000.0));
    let m = fake_result(total);
    let plain = curve_from_sweep(&m, setup, None);
    let fixed = curve_from_sweep(&m, setup, Some(&short));
    let k = fixed.f.partition_point(|&x| x < 1000.0);
    // without the compensation the leads are in the number; with it the coil of 8 ohm and 0.5 mH is left
    let want = ((8.0f32).powi(2) + (2.0 * std::f32::consts::PI * fixed.f[k] * 0.5e-3).powi(2)).sqrt();
    assert!((fixed.ohm(k) - want).abs() < 0.03, "{} vs {want}", fixed.ohm(k));
    assert!((plain.ohm(k) - want).abs() > 0.2, "the leads are visible without it");
    // the settings file keeps it
    let back = ZShort::decode(&short.encode()).unwrap();
    assert!((back.at(1000.0).0 - short.at(1000.0).0).abs() < 0.01 && (back.at(10000.0).1 - short.at(10000.0).1).abs() < 0.01);
}

#[test]
fn the_dats_values_read_its_built_in_resistor_as_1k() {
    // what the box showed with its 1 kΩ resistor on the terminals: L / R = 0.9129
    let h = (0.9129 * DATS_GAIN, 0.0);
    let z = z_from_h(h, DATS_RS);
    assert!((z.0 - 1000.0).abs() < 3.0 && z.1.abs() < 1e-3, "{z:?}");
}

#[test]
fn calibration_with_a_known_resistor_corrects_the_level() {
    // a measurement that reads 3 dB too high: 100 ohm behind 100 ohm should give 0.5, it reads 0.7071
    let h = super::super::sweep::Transfer { df: 5.0, mag_db: vec![-3.0103; 2000], phase: vec![0.0; 2000], coh: None };
    let res = SweepResult { harm: None, sr: 48000.0, ir: Vec::new(), pre: 0, h, runs: 1, capture_peak_db: -20.0, params: Default::default(), id: 1 };
    let g = calibration_gain(&res, 100.0, 100.0).unwrap();
    assert!((g - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01, "{g}");
}

/// With a DATS V3 connected (leads on the built-in 1 kΩ resistor): plays tones through it and prints what comes back on each input
/// channel (cargo test --release probe_the_dats -- --ignored --nocapture).
#[test]
#[ignore]
fn probe_the_dats_hardware() {
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;
    let ins = crate::audio::list_devices();
    let outs = crate::tonegen::list_output_devices();
    for d in ins.iter().chain(outs.iter()) {
        println!("device {:>3} | {}", d.id.chars().take(8).collect::<String>(), d.label);
    }
    let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 (USB AUDIO  CODEC) is connected");
    println!("found: {d:?}");
    let sh = Arc::new(crate::shared::Shared::new());
    let _a = crate::audio::start(sh.clone(), d.in_id.clone().expect("an input"));
    let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
    let status = Arc::new(Mutex::new(String::new()));
    let _g = crate::tonegen::start(params.clone(), d.out_id.clone().expect("an output"), status.clone());
    std::thread::sleep(Duration::from_millis(1500));
    println!("capture: {}   generator: {}", sh.status.lock(), status.lock());
    let sr = sh.sr.load(std::sync::atomic::Ordering::Relaxed) as f32;
    let dft = |x: &[f32], f: f32| -> (f32, f32) {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
            re += *v as f64 * a.cos();
            im -= *v as f64 * a.sin();
        }
        ((2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32, im.atan2(re) as f32)
    };
    for f in [0.0f32, 100.0, 300.0, 1000.0, 3000.0, 10000.0] {
        {
            let mut g = params.lock();
            g.on = f > 0.0;
            g.wave = crate::tonegen::Wave::Sine;
            g.freq = f.max(1.0);
            g.level_db = [-20.0, -20.0];
        }
        std::thread::sleep(Duration::from_millis(1200));
        let n = (sr * 0.5) as usize;
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        sh.ring.lock().copy_last(&mut l, &mut r);
        if f == 0.0 {
            let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
            let db = |v: f32| 20.0 * v.max(1e-9).log10();
            println!("silence: L {:.1} dBFS rms, R {:.1} dBFS rms (peak L {:.4}, R {:.4})", db(rms(&l)), db(rms(&r)), l.iter().fold(0f32, |m, v| m.max(v.abs())), r.iter().fold(0f32, |m, v| m.max(v.abs())));
            continue;
        }
        let ((al, pl), (ar, pr)) = (dft(&l, f), dft(&r, f));
        let wrap = |x: f32| { let t = std::f32::consts::TAU; (x + std::f32::consts::PI).rem_euclid(t) - std::f32::consts::PI };
        // the impedance this reads with the values of the DATS: H = L / R
        let (mag, ang) = (al / ar.max(1e-9) * DATS_GAIN, wrap(pl - pr));
        let z = z_from_h((mag * ang.cos(), mag * ang.sin()), DATS_RS);
        println!("{f:>7.0} Hz: L {:.2} dBFS ({:.4})  R {:.2} dBFS ({:.4})   R-L phase {:.2} deg   L-R ratio {:.5}   Z = {:.4} + j {:.4} ohm", 20.0 * al.max(1e-9).log10(), al, 20.0 * ar.max(1e-9).log10(), ar, wrap(pr - pl).to_degrees(), al / ar.max(1e-9), z.0, z.1);
    }
    params.lock().on = false;
    std::thread::sleep(Duration::from_millis(300));
}

/// With a DATS V3 and a speaker on its terminals: a stepped sine over 20 Hz to 20 kHz, the impedance with the DATS values and the short of
/// the leads taken off (cargo test --release measure_a_speaker -- --ignored --nocapture).
#[test]
#[ignore]
fn measure_a_speaker_with_the_dats() {
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;
    let ins = crate::audio::list_devices();
    let outs = crate::tonegen::list_output_devices();
    let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 is connected");
    let sh = Arc::new(crate::shared::Shared::new());
    let _a = crate::audio::start(sh.clone(), d.in_id.clone().unwrap());
    let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
    let _g = crate::tonegen::start(params.clone(), d.out_id.clone().unwrap(), Arc::new(Mutex::new(String::new())));
    std::thread::sleep(Duration::from_millis(1500));
    let sr = sh.sr.load(std::sync::atomic::Ordering::Relaxed) as f32;
    let dft = |x: &[f32], f: f32| -> (f32, f32) {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
            re += *v as f64 * a.cos();
            im -= *v as f64 * a.sin();
        }
        ((2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32, im.atan2(re) as f32)
    };
    // the leads as measured in the short test
    let lead = |f: f32| (0.476f32, 2.0 * std::f32::consts::PI * f * 4.9e-6);
    let mut rows: Vec<(f32, f32, f32)> = Vec::new();
    let n_pts = 40;
    println!("   f (Hz)    |Z| ohm   phase deg    L level dBFS");
    for k in 0..n_pts {
        let f = 20.0 * 1000f32.powf(k as f32 / (n_pts - 1) as f32);
        {
            let mut g = params.lock();
            g.on = true;
            g.wave = crate::tonegen::Wave::Sine;
            g.freq = f;
            g.level_db = [-10.0, -10.0];
        }
        std::thread::sleep(Duration::from_millis(if f < 60.0 { 1400 } else { 800 }));
        let n = (sr * if f < 60.0 { 0.8 } else { 0.4 }) as usize;
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        sh.ring.lock().copy_last(&mut l, &mut r);
        let ((al, pl), (ar, pr)) = (dft(&l, f), dft(&r, f));
        let mag = al / ar.max(1e-9) * DATS_GAIN;
        let ang = pl - pr;
        let z = z_from_h((mag * ang.cos(), mag * ang.sin()), DATS_RS);
        let ld = lead(f);
        let (zr, zx) = (z.0 - ld.0, z.1 - ld.1);
        let (m, ph) = ((zr * zr + zx * zx).sqrt(), zx.atan2(zr).to_degrees());
        println!("{f:>9.1}  {m:>8.3}  {ph:>9.1}    {:.1}", 20.0 * al.max(1e-9).log10());
        rows.push((f, m, ph));
    }
    params.lock().on = false;
    // the usual numbers: the peak (free air resonance), the minimum above it, the value at 1 kHz
    let (mut ip, mut im) = (0usize, 0usize);
    for i in 0..rows.len() {
        if rows[i].1 > rows[ip].1 {
            ip = i;
        }
    }
    for i in ip..rows.len() {
        if rows[i].1 < rows[im.max(ip)].1 || im < ip {
            im = i;
        }
    }
    println!("resonance peak {:.2} ohm at {:.0} Hz; minimum after it {:.2} ohm at {:.0} Hz; lowest value {:.2} ohm", rows[ip].1, rows[ip].0, rows[im].1, rows[im].0, rows.iter().map(|r| r.1).fold(f32::MAX, f32::min));
    std::thread::sleep(Duration::from_millis(300));
}

/// With a DATS V3 and a speaker: a real (smooth, logarithmic) sweep through the sweep engine of the program, then the impedance with the DATS
/// values and the short of the leads taken off; written to a CSV for the screenshot hook (cargo test --release sweep_a_speaker -- --ignored --nocapture).
#[test]
#[ignore]
fn sweep_a_speaker_with_the_dats() {
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;
    let ins = crate::audio::list_devices();
    let outs = crate::tonegen::list_output_devices();
    let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 is connected");
    let sh = Arc::new(crate::shared::Shared::new());
    let _a = crate::audio::start(sh.clone(), d.in_id.clone().unwrap());
    let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
    let _g = crate::tonegen::start(params.clone(), d.out_id.clone().unwrap(), Arc::new(Mutex::new(String::new())));
    std::thread::sleep(Duration::from_millis(1500));
    let mut sw = crate::tools::sweep::Sweep::new();
    sw.p.loopback = true;
    sw.p.amp_db = -10.0;
    sw.p.secs = 8.0;
    sw.p.tail = 1.5;
    sw.p.averages = 2;
    sw.p.f0 = 20.0;
    sw.p.f1 = 20000.0;
    sw.start();
    let mut req = crate::tools::Requests::default();
    let t0 = std::time::Instant::now();
    while sw.busy() && t0.elapsed() < Duration::from_secs(90) {
        sw.tick(&sh, true, &mut req);
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("sweep: {} after {:.1} s", match &sw.state { crate::tools::sweep::State::Done => "done".to_string(), crate::tools::sweep::State::Failed(e) => format!("failed: {e}"), _ => "not finished".to_string() }, t0.elapsed().as_secs_f32());
    let r = sw.result.as_ref().expect("a result");
    println!("{} run(s), capture peak {:.1} dBFS", r.runs, r.capture_peak_db);
    let setup = Setup { r_ref: DATS_RS, r_out: 0.0, gain: DATS_GAIN };
    // the leads as measured with the short (0.476 ohm, 4.9 microhenry)
    let f: Vec<f32> = (0..200).map(|i| 5.0 * 4000f32.powf(i as f32 / 199.0)).collect();
    let short = ZShort { re: vec![0.476; f.len()], im: f.iter().map(|x| 2.0 * std::f32::consts::PI * x * 4.9e-6).collect(), f };
    let c = curve_from_sweep(r, setup, Some(&short));
    let mut csv = String::from("freq_hz,db,phase_deg
");
    for i in 0..c.f.len() {
        csv += &format!("{:.3},{:.4},{:.3}
", c.f[i], c.db[i], c.phase_deg[i]);
    }
    let path = std::env::var("IMP_CSV_OUT").unwrap_or_else(|_| "impedance_sweep.csv".into());
    std::fs::write(&path, csv).unwrap();
    let (mut ip, mut lo) = (0usize, f32::MAX);
    for i in 0..c.f.len() {
        if c.db[i] > c.db[ip] {
            ip = i;
        }
        lo = lo.min(c.db[i]);
    }
    println!("peak {:.2} ohm at {:.1} Hz; lowest {:.2} ohm; written to {path}", c.ohm(ip), c.f[ip], 10f32.powf(lo / 20.0));
}

#[test]
fn a_sweep_that_failed_or_was_stopped_cancels_the_wish_for_its_result() {
    // asked for, running: kept
    assert!(!arm_lost(true, true, 4, 4));
    // asked for, nothing running, no new result: lost (stopped, or "nothing was captured")
    assert!(arm_lost(true, false, 4, 4));
    // a new result: not lost (the normal path takes it)
    assert!(!arm_lost(true, false, 5, 4));
    // not asked for: nothing to cancel
    assert!(!arm_lost(false, false, 4, 4));
}
