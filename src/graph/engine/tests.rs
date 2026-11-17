//! Tests of `engine.rs`.
use super::*;

fn engine_with(g: Graph, gens: Vec<GenParams>) -> (Run, Arc<Shared>) {
    let sh = Arc::new(Shared::new());
    let e = Engine::bare(sh.clone(), Arc::new(Mutex::new(gens)));
    *e.graph.lock() = g.clone();
    let mut run = Run::new(e);
    run.rate = 48000;
    run.rebuild(&g);
    (run, sh)
}

fn ring_tail(sh: &Shared, n: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    sh.ring.lock().copy_last(&mut l, &mut r);
    (l, r)
}

#[test]
fn generator_through_gain_into_the_analyzer() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    g.node_mut(gain).unwrap().p[0] = -6.0206;
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, gain, 0).unwrap();
    g.connect(gain, an, 0).unwrap();
    let p = GenParams { on: true, level_db: [-6.0206; 2], ..GenParams::default() };
    let (mut run, sh) = engine_with(g, vec![p]);
    for _ in 0..40 {
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 4096);
    let pk = l.iter().fold(0f32, |m, v| m.max(v.abs()));
    // -6 dB generator, -6 dB gain: a quarter of full scale
    assert!((pk - 0.25).abs() < 0.02, "peak {pk}");
    assert_eq!(sh.ring_owner.load(Relaxed), 3);
    assert_eq!(sh.sr.load(Relaxed), 48000);
}

#[test]
fn a_phase_node_cancels_a_copy_of_the_signal_in_a_mixer_and_the_new_effects_run_in_the_engine() {
    // generator -> mixer in 1, generator -> inverting phase node -> mixer in 2: the sum is silent
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let ph = g.add(Kind::Phase, 0.0, 0.0);
    g.node_mut(ph).unwrap().p[0] = 1.0;
    let mx = g.add(Kind::Mixer, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, mx, 0).unwrap();
    g.connect(gn, ph, 0).unwrap();
    g.connect(ph, mx, 1).unwrap();
    g.connect(mx, an, 0).unwrap();
    let p = GenParams { on: true, level_db: [-6.0; 2], ..GenParams::default() };
    let (mut run, sh) = engine_with(g, vec![p.clone()]);
    for _ in 0..40 {
        run.tick(Some(1024));
    }
    let (l, r) = ring_tail(&sh, 4096);
    assert!(l.iter().chain(r.iter()).all(|v| v.abs() < 1e-5), "the inverted copy cancels the original");
    // every new effect passes a signal without blowing up (with no file loaded the convolution passes the signal)
    for kind in [Kind::Phase, Kind::Shifter, Kind::Convolver, Kind::NoiseRed, Kind::Distortion] {
        let mut g = Graph::default();
        let gn = g.add(Kind::Generator, 0.0, 0.0);
        let fx = g.add(kind, 0.0, 0.0);
        if kind == Kind::Distortion {
            g.node_mut(fx).unwrap().p[1] = DIST_HARMONIC as f32;
        }
        let an = g.add(Kind::Analyzer, 0.0, 0.0);
        g.connect(gn, fx, 0).unwrap();
        g.connect(fx, an, 0).unwrap();
        let (mut run, sh) = engine_with(g, vec![p.clone()]);
        for _ in 0..40 {
            run.tick(Some(1024));
        }
        let (l, _) = ring_tail(&sh, 4096);
        let pk = l.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(pk.is_finite() && pk > 0.1 && pk < 1.5, "{kind:?} peak {pk}");
    }
}

#[test]
fn a_convolution_node_loads_its_file_in_the_background_and_then_delays_by_it() {
    let dir = std::env::temp_dir().join(format!("visuscope_ir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ir.wav");
    // an impulse response that is a single spike 480 samples (10 ms) in
    {
        let mut ir = vec![0f32; 2000];
        ir[480] = 1.0;
        let mut w = crate::tools::wavio::WavWriter::create(&path, crate::tools::wavio::Format::Float32, 1, 48000).unwrap();
        w.write(&ir, &ir).unwrap();
        w.finish().unwrap();
    }
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let cv = g.add(Kind::Convolver, 0.0, 0.0);
    {
        let n = g.node_mut(cv).unwrap();
        n.p[0] = 100.0;
        n.s = path.to_string_lossy().to_string();
    }
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, cv, 0).unwrap();
    g.connect(cv, an, 0).unwrap();
    // an impulse train would show the delay; a sine shows the level: 0.5 in, 0.5 out once the file is loaded
    let p = GenParams { on: true, level_db: [-6.0206; 2], ..GenParams::default() };
    let (mut run, sh) = engine_with(g, vec![p]);
    // until the file is read the node passes the signal on; the status tells when it is there
    let start = std::time::Instant::now();
    loop {
        run.tick(Some(1024));
        let st = run.e.node_status.lock().get(&cv).cloned().unwrap_or_default();
        if st.contains("s impulse response") {
            break;
        }
        assert!(start.elapsed().as_secs() < 10, "the impulse response never arrived: {st}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for _ in 0..16 {
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 4096);
    let pk = l.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!((pk - 0.5).abs() < 0.03, "level through a unit-energy spike: {pk}");
    let st = run.e.node_status.lock().get(&cv).cloned().unwrap_or_default();
    assert!(st.contains("0.04 s impulse response"), "{st}");
    // a missing file ends in a message, not in silence that nobody explains
    let mut g2 = run.e.graph_clone();
    g2.node_mut(cv).unwrap().s = dir.join("nothing.wav").to_string_lossy().to_string();
    run.rebuild(&g2);
    let start = std::time::Instant::now();
    loop {
        run.tick(Some(1024));
        let st = run.e.node_status.lock().get(&cv).cloned().unwrap_or_default();
        if !st.is_empty() && !st.contains("loading") {
            assert!(!st.contains("s impulse response"), "{st}");
            break;
        }
        assert!(start.elapsed().as_secs() < 5, "no message for a missing file");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_60_db_and_minus_60_db_again_is_lossless_and_a_parameter_change_leaves_no_gap() {
    let p = GenParams { on: true, level_db: [-6.0206; 2], ..GenParams::default() };
    let build = |chain: &[f32]| {
        let mut g = Graph::default();
        let gn = g.add(Kind::Generator, 0.0, 0.0);
        let mut prev = gn;
        for db in chain {
            let n = g.add(Kind::Gain, 0.0, 0.0);
            g.node_mut(n).unwrap().p[0] = *db;
            g.connect(prev, n, 0).unwrap();
            prev = n;
        }
        let an = g.add(Kind::Analyzer, 0.0, 0.0);
        g.connect(prev, an, 0).unwrap();
        g
    };
    let run_for = |g: Graph| {
        let (mut run, sh) = engine_with(g, vec![p.clone()]);
        for _ in 0..40 {
            run.tick(Some(1024));
        }
        ring_tail(&sh, 8192).0
    };
    let direct = run_for(build(&[]));
    // +24 +24 +12 = +60 dB (the most one node gives is +24), then back down by the same
    let there_and_back = run_for(build(&[24.0, 24.0, 12.0, -24.0, -24.0, -12.0]));
    let loud = run_for(build(&[24.0, 24.0, 12.0]));
    let peak = loud.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 400.0, "no clipping inside the graph: peak {peak}");
    let err = direct.iter().zip(&there_and_back).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(err < 1e-5, "error after +60 / -60 dB: {err} ({:.0} dB below the signal)", 20.0 * (err / 0.5).max(1e-12).log10());
    // an equaliser band moved while the sound runs: no gap, no jump
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let eq = g.add(Kind::Eq, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, eq, 0).unwrap();
    g.connect(eq, an, 0).unwrap();
    let (mut run, sh) = engine_with(g.clone(), vec![p.clone()]);
    for _ in 0..30 {
        run.tick(Some(1024));
    }
    let mut worst_step = 0f32;
    let mut silent_run = 0usize;
    let mut longest_silence = 0usize;
    for k in 0..60 {
        let mut g2 = g.clone();
        {
            let n = g2.node_mut(eq).unwrap();
            n.p[0] = 5.0; // a notch
            n.p[1] = 400.0 + k as f32 * 40.0;
            n.p[2] = -30.0;
            n.p[3] = 2.0;
        }
        run.e.set_graph(g2.clone());
        run.rebuild(&g2);
        run.tick(Some(512));
        let (l, _) = ring_tail(&sh, 512);
        for w in l.windows(2) {
            worst_step = worst_step.max((w[1] - w[0]).abs());
        }
        for v in &l {
            if v.abs() < 1e-7 {
                silent_run += 1;
                longest_silence = longest_silence.max(silent_run);
            } else {
                silent_run = 0;
            }
        }
    }
    assert!(longest_silence < 40, "a run of {longest_silence} silent samples after a parameter change");
    assert!(worst_step < 0.2, "jump of {worst_step} between samples after a parameter change");
}

#[test]
fn the_probe_hears_the_signal_at_a_port_even_on_a_branch_that_leads_nowhere() {
    // generator (-6 dB) -> gain +6 dB -> filter, and nothing connected to a sink at all
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    g.node_mut(gain).unwrap().p[0] = 6.0206;
    let fl = g.add(Kind::Filter, 0.0, 0.0);
    g.connect(gn, gain, 0).unwrap();
    g.connect(gain, fl, 0).unwrap();
    let p = GenParams { on: true, level_db: [-6.0206; 2], ..GenParams::default() };
    let (mut run, _sh) = engine_with(g.clone(), vec![p]);
    // (the generators are normally copied by `tick`)
    run.gens = run.e.gens.lock().clone();
    let peak_of = |run: &mut Run| {
        run.probe_q.lock().buf.clear();
        for _ in 0..6 {
            run.chunk(1024, false);
        }
        let q = run.probe_q.lock();
        (q.buf.len(), q.buf.iter().skip(2048).fold(0f32, |m, v| m.max(v.0.abs())))
    };
    // nothing listens: nothing is queued
    assert_eq!(peak_of(&mut run).0, 0);
    // the output of the generator: 0.5; the output of the gain: 1.0; the input of the filter is the gain's output again
    for (probe, want) in [(Probe { node: gn, input: false, port: 0 }, 0.5), (Probe { node: gain, input: false, port: 0 }, 1.0), (Probe { node: fl, input: true, port: 0 }, 1.0), (Probe { node: gain, input: true, port: 0 }, 0.5)] {
        *run.e.probe.lock() = Some(probe);
        run.rebuild(&g);
        let (n, pk) = peak_of(&mut run);
        assert_eq!(n, 6 * 1024, "{probe:?}");
        assert!((pk - want).abs() < 0.03, "{probe:?}: peak {pk}, wanted {want}");
    }
    // an unconnected input is silence, not a crash
    let mut g2 = g.clone();
    g2.disconnect(fl, 0);
    *run.e.probe.lock() = Some(Probe { node: fl, input: true, port: 0 });
    run.rebuild(&g2);
    let (n, pk) = peak_of(&mut run);
    assert!(n > 0 && pk == 0.0);
    // a node that was removed while probed does nothing
    *run.e.probe.lock() = Some(Probe { node: 999, input: false, port: 0 });
    run.rebuild(&g2);
    assert_eq!(peak_of(&mut run).0, 0);
}

#[test]
fn an_output_plays_a_pair_or_two_single_channels() {
    let (mut bl, mut br) = (vec![9f32; 4], vec![9f32; 4]);
    // left channel: 0.5 on both sides, right channel: 0.25 on both sides (what a split node delivers)
    let l: [Vec<f32>; 2] = [vec![0.5; 4], vec![0.5; 4]];
    let r: [Vec<f32>; 2] = [vec![0.25; 4], vec![0.25; 4]];
    let pair: [Vec<f32>; 2] = [vec![0.1; 4], vec![0.2; 4]];
    assert!(gather_out(Some(&pair), None, None, false, 4, &mut bl, &mut br));
    assert_eq!((bl[0], br[0]), (0.1, 0.2));
    assert!(!gather_out(None, None, None, false, 4, &mut bl, &mut br), "nothing connected: nothing to play");
    assert!(bl.iter().chain(br.iter()).all(|v| *v == 0.0));
    // separate: the first input is the left channel, the second the right one, each made mono
    assert!(gather_out(None, Some(&l), Some(&r), true, 4, &mut bl, &mut br));
    assert_eq!((bl[3], br[3]), (0.5, 0.25));
    // one of them missing: that side is silent, the other still plays
    assert!(gather_out(None, None, Some(&r), true, 4, &mut bl, &mut br));
    assert_eq!((bl[0], br[0]), (0.0, 0.25));
    // a stereo wire into one of them is averaged
    assert!(gather_out(None, Some(&pair), None, true, 4, &mut bl, &mut br));
    assert!((bl[0] - 0.15).abs() < 1e-6 && br[0] == 0.0);
}

#[test]
fn a_two_line_split_delivers_the_whole_stereo_signal_on_both_lines() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let d = g.add(Kind::Dual, 0.0, 0.0);
    let mx = g.add(Kind::Mixer, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, d, 0).unwrap();
    g.connect_from(d, 0, mx, 0).unwrap();
    g.connect_from(d, 1, mx, 1).unwrap();
    g.connect(mx, an, 0).unwrap();
    let p = GenParams { on: true, level_db: [-12.0, -12.0], ..GenParams::default() };
    let (mut run, sh) = engine_with(g, vec![p]);
    for _ in 0..40 {
        run.tick(Some(1024));
    }
    let (l, r) = ring_tail(&sh, 4096);
    let pk = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
    // -12 dB is 0.25 of full scale; both lines summed in the mixer: 0.5 on each side
    assert!((pk(&l) - 0.5).abs() < 0.03 && (pk(&r) - 0.5).abs() < 0.03, "{} {}", pk(&l), pk(&r));
}

#[test]
fn a_parameter_follows_a_generator_signal_and_stays_inside_its_limits() {
    // a sawtooth generator drives the gain of a node that passes a second, constant signal: the output level follows
    let mut g = Graph::default();
    let a = g.add(Kind::Generator, 0.0, 0.0);
    let b = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.node_mut(a).unwrap().p[0] = 1.0;
    g.node_mut(b).unwrap().p[0] = 2.0;
    g.connect(b, gain, 0).unwrap();
    g.connect(gain, an, 0).unwrap();
    // the signal of generator 1 is a DC-like level (a steady wave is enough: its mean over a block is near zero, so the value sits in the middle)
    assert!(g.set_modulation(Modulation { node: gain, param: 0, src: a, in_lo: -1.0, in_hi: 1.0, out_lo: -40.0, out_hi: 0.0, log: false }));
    let mut ga = GenParams::default();
    ga.wave = crate::tonegen::Wave::Sine;
    let (mut run, _sh) = engine_with(g, vec![ga, GenParams::default()]);
    for _ in 0..6 {
        run.chunk(CHUNK, false);
    }
    assert_eq!(run.mods.len(), 1);
    // the node follows: its gain is a value inside the range, set by the signal
    let v = run.mods[0].node.p[0];
    assert!((-40.0..=0.0).contains(&v), "{v}");
    // a signal outside the input range clamps
    run.mods[0].m.in_lo = 5.0;
    run.mods[0].m.in_hi = 6.0;
    run.chunk(CHUNK, false);
    assert_eq!(run.mods[0].node.p[0], -40.0);
}

#[test]
fn a_signal_switches_an_on_off_setting_with_hysteresis() {
    let mut g = Graph::default();
    let a = g.add(Kind::Generator, 0.0, 0.0);
    let b = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.node_mut(a).unwrap().p[0] = 1.0;
    g.node_mut(b).unwrap().p[0] = 2.0;
    g.connect(b, gain, 0).unwrap();
    g.connect(gain, an, 0).unwrap();
    // param 1 of a gain node is its polarity (two entries); the signal's mean is about 0
    assert!(g.set_modulation(Modulation { node: gain, param: 1, src: a, in_lo: -0.2, in_hi: -0.1, out_lo: 0.0, out_hi: 1.0, log: false }));
    let mut ga = GenParams::default();
    ga.wave = crate::tonegen::Wave::Sine;
    let (mut run, _sh) = engine_with(g, vec![ga, GenParams::default()]);
    for _ in 0..6 {
        run.chunk(CHUNK, false);
    }
    assert!(run.mods[0].sw);
    // above the upper threshold: on
    assert_eq!(run.mods[0].node.p[1], 1.0);
    // between the thresholds it keeps its state
    run.mods[0].m.in_lo = -1.0;
    run.mods[0].m.in_hi = 1.0;
    run.chunk(CHUNK, false);
    assert_eq!(run.mods[0].node.p[1], 1.0);
    // below the lower threshold: off, and again it stays when the thresholds widen
    run.mods[0].m.in_lo = 0.5;
    run.mods[0].m.in_hi = 0.6;
    run.chunk(CHUNK, false);
    assert_eq!(run.mods[0].node.p[1], 0.0);
    run.mods[0].m.in_lo = -1.0;
    run.mods[0].m.in_hi = 1.0;
    run.chunk(CHUNK, false);
    assert_eq!(run.mods[0].node.p[1], 0.0);
}

#[test]
fn two_generators_mixed_and_bypass() {
    let mut g = Graph::default();
    let (a, b) = (g.add(Kind::Generator, 0.0, 0.0), g.add(Kind::Generator, 0.0, 0.0));
    g.node_mut(b).unwrap().p[0] = 2.0;
    let mx = g.add(Kind::Mixer, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(a, mx, 0).unwrap();
    g.connect(b, mx, 1).unwrap();
    g.connect(mx, an, 0).unwrap();
    let sine = GenParams { on: true, level_db: [-12.0; 2], ..GenParams::default() };
    let square = GenParams { on: true, wave: crate::tonegen::Wave::Square, freq: 300.0, level_db: [-12.0; 2], ..GenParams::default() };
    let (mut run, sh) = engine_with(g.clone(), vec![sine.clone(), square.clone()]);
    for _ in 0..60 {
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 8192);
    let rms = (l.iter().map(|v| v * v).sum::<f32>() / l.len() as f32).sqrt();
    // sine -12 dB peak (0.25 -> rms 0.18) plus a square of 0.25: about 0.31 together
    assert!(rms > 0.25 && rms < 0.4, "rms {rms}");
    // muting one mixer input removes its share
    let mut g2 = g.clone();
    g2.node_mut(mx).unwrap().p[2 + MIX_MAX + 1] = 1.0;
    run.rebuild(&g2);
    for _ in 0..60 {
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 8192);
    let rms2 = (l.iter().map(|v| v * v).sum::<f32>() / l.len() as f32).sqrt();
    assert!(rms2 < rms * 0.7 && rms2 > 0.1, "{rms2} vs {rms}");
}

#[test]
fn extreme_parameters_do_not_make_noise_or_nan() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let mut prev = gn;
    // every effect in a chain, every parameter at its extremes, alternating low and high
    for (k, kind) in Kind::ALL.into_iter().filter(|k| !k.is_source() && !k.is_sink() && !matches!(k, Kind::Mixer | Kind::Split | Kind::Merge | Kind::Dual)).enumerate() {
        let id = g.add(kind, 0.0, 0.0);
        let defs = kind.info().params;
        let n = g.node_mut(id).unwrap();
        for (i, d) in defs.iter().enumerate() {
            n.p[i] = if (i + k) % 2 == 0 { d.min } else { d.max };
        }
        if kind == Kind::Eq {
            n.p.extend([5.0, 20000.0, -60.0, 10.0, 0.0, 20.0, 18.0, 0.1]);
        }
        g.connect(prev, id, 0).unwrap();
        prev = id;
    }
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(prev, an, 0).unwrap();
    let (mut run, sh) = engine_with(g, vec![GenParams { on: true, level_db: [-6.0; 2], ..GenParams::default() }]);
    for _ in 0..80 {
        run.tick(Some(1024));
    }
    let (l, r) = ring_tail(&sh, 16384);
    assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()), "NaN or infinity came out");
    assert!(l.iter().chain(r.iter()).all(|v| v.abs() < 1000.0), "the chain blew up");
}

/// Cost of every kind of node: milliseconds of processing for one second of audio (cargo test --release bench_nodes -- --ignored --nocapture).
#[test]
#[ignore]
fn bench_nodes() {
    for rate in [48000u32, 192000] {
        println!("---- {rate} Hz");
        for kind in Kind::ALL.into_iter().filter(|k| !k.is_source() && !k.is_sink() && !matches!(k, Kind::Mixer | Kind::Split | Kind::Merge | Kind::Dual)) {
            let mut g = Graph::default();
            let gn = g.add(Kind::Generator, 0.0, 0.0);
            let id = g.add(kind, 0.0, 0.0);
            g.connect(gn, id, 0).unwrap();
            let an = g.add(Kind::Analyzer, 0.0, 0.0);
            g.connect(id, an, 0).unwrap();
            let (mut run, _sh) = engine_with(g, vec![GenParams { on: true, level_db: [-6.0; 2], ..GenParams::default() }]);
            run.rate = rate;
            let g2 = run.e.graph.lock().clone();
            run.rebuild(&g2);
            for _ in 0..20 {
                run.tick(Some(1024));
            }
            let frames = rate as usize;
            let t = std::time::Instant::now();
            let mut done = 0;
            while done < frames {
                run.tick(Some(1024));
                done += 1024;
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 * (frames as f64 / done as f64);
            println!("{:28} {:7.2} ms per second of audio", kind.name(), ms);
        }
    }
}

#[test]
fn what_a_slower_device_cannot_play_is_filtered_out_before_it_folds_back() {
    // the filter an output uses for a 48 kHz device at 192 kHz
    let fs = 192000.0f32;
    let level = |f: f32| {
        let mut aa = FirLowpass::default();
        aa.design(fs, 48000.0);
        let n = 48000;
        let mut l: Vec<f32> = (0..n).map(|i| (std::f64::consts::TAU * f as f64 * i as f64 / fs as f64).sin() as f32).collect();
        let mut r = l.clone();
        // (in blocks, as the engine does)
        for (cl, cr) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
            aa.process(cl, cr);
        }
        let rms = (l[n / 2..].iter().map(|v| v * v).sum::<f32>() / (n / 2) as f32).sqrt();
        20.0 * (rms * std::f32::consts::SQRT_2).log10()
    };
    assert!(level(1000.0).abs() < 0.05);
    assert!(level(19000.0).abs() < 0.2, "the audible band stays: {}", level(19000.0));
    assert!(level(28000.0) < -90.0, "{}", level(28000.0));
    assert!(level(49000.0) < -90.0, "49 kHz would be heard as 1 kHz: {}", level(49000.0));
    assert!(level(80000.0) < -90.0, "{}", level(80000.0));
}

#[test]
fn a_generator_that_leads_to_a_sink_is_announced_for_the_sweeps() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    let (mut run, _sh) = engine_with(g.clone(), vec![GenParams::default()]);
    g.connect(gn, an, 0).unwrap();
    run.rebuild(&g);
    assert!(bus().gen_live.load(Relaxed), "a generator node that feeds a sink is running");
}

#[test]
fn changing_a_setting_does_not_restart_the_clock() {
    // a graph without an input runs on the clock of the engine: a rebuild for a changed setting (60 times a second while a slider
    // is dragged) must not throw away the time that has passed since the last block
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, gain, 0).unwrap();
    g.connect(gain, an, 0).unwrap();
    let (mut run, _sh) = engine_with(g.clone(), vec![GenParams { on: true, ..GenParams::default() }]);
    let before = Instant::now() - Duration::from_millis(3);
    run.last_tick = before;
    run.carry = 0.37;
    let mut g2 = g.clone();
    g2.node_mut(gain).unwrap().p[0] = -9.0;
    run.rebuild(&g2);
    assert_eq!(run.last_tick, before);
    assert_eq!(run.carry, 0.37);
}

#[test]
fn a_gain_that_changes_is_eased_and_a_mute_fades() {
    // a steady signal through a gain node: when the gain jumps from 0 dB to -20 dB the output has no step
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let gain = g.add(Kind::Gain, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, gain, 0).unwrap();
    g.connect(gain, an, 0).unwrap();
    let (mut run, sh) = engine_with(g.clone(), vec![GenParams { on: true, level_db: [-6.0; 2], freq: 100.0, ..GenParams::default() }]);
    for _ in 0..8 {
        run.tick(Some(1024));
    }
    let mut g2 = g.clone();
    g2.node_mut(gain).unwrap().p[0] = -20.0;
    run.rebuild(&g2);
    for _ in 0..4 {
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 4096);
    // a 100 Hz sine at 48 kHz changes by at most 2 pi 100 / 48000 = 0.013 per sample (times 0.5 for -6 dB): a click would be larger
    let worst = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(worst < 0.02, "a step of {worst} in the signal");
}

#[test]
fn dead_branches_are_not_processed_and_unconnected_analyzer_is_silent() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let eq = g.add(Kind::Eq, 0.0, 0.0);
    g.connect(gn, eq, 0).unwrap();
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    let (mut run, sh) = engine_with(g, vec![GenParams { on: true, ..GenParams::default() }]);
    assert_eq!(run.nodes.len(), 1, "only the analyzer is live");
    for _ in 0..10 {
        run.tick(Some(512));
    }
    let (l, _) = ring_tail(&sh, 1024);
    assert!(l.iter().all(|v| *v == 0.0));
    let _ = an;
    assert!(sh.ring.lock().total >= 5120);
}

#[test]
fn timeline_bus_reaches_a_sink_through_the_rate_converter() {
    let mut g = Graph::default();
    let tl = g.add(Kind::TimelineOut, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(tl, an, 0).unwrap();
    let (mut run, sh) = engine_with(g, vec![GenParams::default()]);
    bus().tl_clear();
    // a constant 0.5 at 44.1 kHz delivered in real-time sized pieces (941 frames per 1024 output frames)
    let x = vec![0.5f32; 941];
    for _ in 0..30 {
        bus().tl_push(44100, None, &x, &x);
        run.tick(Some(1024));
    }
    let (l, _) = ring_tail(&sh, 16384);
    let loud = l.iter().filter(|v| (**v - 0.5).abs() < 0.01).count();
    assert!(loud > 14000, "{loud} samples carried the signal");
    bus().tl_clear();
}

#[test]
fn an_impulse_response_of_another_rate_is_resampled_by_the_loader_not_by_the_engine_thread() {
    let dir = std::env::temp_dir().join(format!("visuscope_ir96_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ir96.wav");
    // a stereo response at 96 kHz: a spike 960 frames (10 ms) in on the left, the same one a little weaker on the right
    {
        let (mut l, mut r) = (vec![0f32; 4000], vec![0f32; 4000]);
        l[960] = 1.0;
        r[960] = 0.5;
        let mut w = crate::tools::wavio::WavWriter::create(&path, crate::tools::wavio::Format::Float32, 2, 96000).unwrap();
        w.write(&l, &r).unwrap();
        w.finish().unwrap();
    }
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let cv = g.add(Kind::Convolver, 0.0, 0.0);
    {
        let n = g.node_mut(cv).unwrap();
        n.p[0] = 100.0;
        n.s = path.to_string_lossy().to_string();
    }
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, cv, 0).unwrap();
    g.connect(cv, an, 0).unwrap();
    let p = GenParams { on: true, level_db: [-6.0206; 2], ..GenParams::default() };
    let (mut run, sh) = engine_with(g, vec![p]);
    let start = std::time::Instant::now();
    loop {
        run.tick(Some(1024));
        let st = run.e.node_status.lock().get(&cv).cloned().unwrap_or_default();
        if st.contains("s impulse response") {
            assert!(st.contains("0.04 s impulse response"), "the length is the one at the rate of the graph: {st}");
            break;
        }
        assert!(start.elapsed().as_secs() < 10, "the impulse response never arrived: {st}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for _ in 0..16 {
        run.tick(Some(1024));
    }
    // (the energy of the two channels together is 1: 0.5 * 2 / sqrt(1.25 / 2) on the left, half of it on the right)
    let (l, r) = ring_tail(&sh, 4096);
    let (pl, pr) = (l.iter().fold(0f32, |m, v| m.max(v.abs())), r.iter().fold(0f32, |m, v| m.max(v.abs())));
    let want_l = 0.5 / (1.25f32 / 2.0).sqrt();
    assert!((pl - want_l).abs() < 0.06 * want_l, "left {pl}, wanted about {want_l}");
    assert!((pr / pl - 0.5).abs() < 0.03, "the right channel keeps its own response: {pr} / {pl}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cost of one `rebuild` of a graph of 30 nodes, as it runs for every step of a slider that is dragged
/// (cargo test --locked bench_rebuild -- --ignored --nocapture).
#[test]
#[ignore]
fn bench_rebuild() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let mut prev = gn;
    let kinds = [Kind::Gain, Kind::Eq, Kind::Comp, Kind::Filter, Kind::Limiter, Kind::Gate, Kind::Delay, Kind::Distortion, Kind::Fuzz, Kind::Tremolo];
    for k in 0..30 {
        let id = g.add(kinds[k % kinds.len()], 0.0, 0.0);
        g.connect(prev, id, 0).unwrap();
        prev = id;
    }
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(prev, an, 0).unwrap();
    let (mut run, _sh) = engine_with(g.clone(), vec![GenParams { on: true, ..GenParams::default() }]);
    let eq = g.nodes.iter().find(|n| n.kind == Kind::Eq).unwrap().id;
    let t = std::time::Instant::now();
    let n = 2000;
    for k in 0..n {
        g.node_mut(eq).unwrap().p[1] = 100.0 + k as f32;
        run.rebuild(&g);
    }
    println!("rebuild of 32 nodes: {:.1} us", t.elapsed().as_secs_f64() * 1e6 / n as f64);
}

#[test]
fn the_delay_of_a_convolution_and_a_limiter_counts_in_the_total() {
    let mut g = Graph::default();
    let gn = g.add(Kind::Generator, 0.0, 0.0);
    let cv = g.add(Kind::Convolver, 0.0, 0.0);
    let lim = g.add(Kind::Limiter, 0.0, 0.0);
    let ph = g.add(Kind::Phase, 0.0, 0.0);
    let an = g.add(Kind::Analyzer, 0.0, 0.0);
    g.connect(gn, cv, 0).unwrap();
    g.connect(cv, lim, 0).unwrap();
    g.connect(lim, ph, 0).unwrap();
    g.connect(ph, an, 0).unwrap();
    let (mut run, _sh) = engine_with(g.clone(), vec![GenParams::default()]);
    let delay = |run: &Run, id: NodeId| {
        let rn = run.nodes.iter().find(|n| n.id == id).expect("a live node");
        run.node_delay_ms(rn, &g)
    };
    // a limiter looks 1 ms ahead; the convolution has no file yet and passes the signal on at once; a phase node that only turns the polarity adds nothing
    assert!((delay(&run, lim) - 1.0).abs() < 0.05, "{}", delay(&run, lim));
    assert_eq!(delay(&run, cv), 0.0);
    assert_eq!(delay(&run, ph), 0.0);
    // with a response it is a block (256 frames for a short one) late
    for n in run.nodes.iter_mut() {
        if let Rt::Conv { u, .. } = &mut n.rt {
            u.set_ir(&[vec![0.0, 1.0, 0.5]], 48000, 48000);
        }
    }
    assert!((delay(&run, cv) - 256.0 * 1000.0 / 48000.0).abs() < 1e-3, "{}", delay(&run, cv));
    // and bypassed, nothing
    for n in run.nodes.iter_mut() {
        n.bypass = true;
    }
    assert_eq!(delay(&run, cv), 0.0);
    assert_eq!(delay(&run, lim), 0.0);
}
