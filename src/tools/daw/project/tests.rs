//! Tests of `project.rs`.
#[test]
fn the_beat_of_dense_music_is_found() {
    use super::*;
    let sr = 44100u32;
    // 100 bpm: a kick on every beat, a hat on the off-beats, over a loud steady pad and some noise
    let mut x = 777u32;
    let mut rnd = move || { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 8) as f32 / 8388608.0 - 1.0 };
    let beat = (sr as f32 * 0.6) as usize;
    let mut v = vec![0f32; sr as usize * 30];
    for (i, s) in v.iter_mut().enumerate() {
        let t = i as f32 / sr as f32;
        *s = 0.25 * (2.0 * std::f32::consts::PI * 220.0 * t).sin() + 0.2 * (2.0 * std::f32::consts::PI * 330.0 * t).sin() + 0.1 * rnd();
    }
    for k in 0..48 {
        let at = k * beat + 5000;
        for i in 0..6000 {
            if at + i < v.len() {
                v[at + i] += 0.6 * (-(i as f32) / 1500.0).exp() * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / sr as f32).sin();
            }
        }
        let h = at + beat / 2;
        for i in 0..1500 {
            if h + i < v.len() {
                v[h + i] += 0.25 * (-(i as f32) / 300.0).exp() * rnd();
            }
        }
    }
    let g = find_beat_grid(&[v], sr).expect("music with a kick has a beat");
    assert!((g.bpm - 100.0).abs() < 3.0 || (g.bpm - 200.0).abs() < 4.0, "{}", g.bpm);
    assert!(g.beats.len() > 25);
}

#[test]
fn the_beat_of_a_click_track_is_found_and_noise_has_none() {
    use super::*;
    let sr = 48000u32;
    // 120 bpm: a short decaying burst every half second, 20 s long
    let mut v = vec![0f32; sr as usize * 20];
    let beat = sr as usize / 2;
    for k in 0..40 {
        for i in 0..2400 {
            v[k * beat + 3000 + i] = 0.7 * (-(i as f32) / 500.0).exp() * (i as f32 * 0.4).sin();
        }
    }
    let s = Source::new(1, "t".into(), vec![v]);
    let g = find_beat_grid(&s.ch, sr).expect("a click track has a beat");
    assert!((g.bpm - 120.0).abs() < 2.0, "{}", g.bpm);
    let hit = g.beats.iter().filter(|&&b| (0..40).any(|k| (b as i64 - (k * beat + 3000) as i64).abs() < 1500)).count();
    assert!(hit as f32 >= 0.9 * g.beats.len() as f32 && g.beats.len() >= 30, "{} of {}", hit, g.beats.len());
    // a steady noise has no beat
    let mut x = 12345u32;
    let noise: Vec<f32> = (0..sr as usize * 20).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 8) as f32 / 8388608.0 - 1.0 }).map(|v| v * 0.3).collect();
    assert!(find_beat_grid(&[noise], sr).is_none());
}

use super::*;

fn src(n: usize) -> Arc<Source> {
    let l: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
    Arc::new(Source::new(1, "ramp".into(), vec![l]))
}

fn proj_with_clip(n: usize) -> (Project, u32) {
    let mut p = Project::new(48000);
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    let id = p.add_clip(ti, src(n), 100);
    (p, id)
}

#[test]
fn an_inverted_track_cancels_its_twin_and_markers_stay_ordered() {
    let mut p = Project::new(48000);
    let src = Arc::new(Source::new(1, "s".into(), vec![(0..2000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect()]));
    for _ in 0..2 {
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        p.add_clip(ti, src.clone(), 0);
    }
    let (l, _) = p.render(0, 2000, None);
    assert!(l.iter().any(|v| v.abs() > 0.3));
    p.tracks[1].invert = true;
    let (l, r) = p.render(0, 2000, None);
    assert!(l.iter().chain(r.iter()).all(|v| v.abs() < 1e-6), "two identical tracks, one inverted, are silent");
    p.add_mark(900, 0, None);
    p.add_mark(100, 50, None);
    assert_eq!(p.marks.iter().map(|m| m.at).collect::<Vec<_>>(), vec![100, 900]);
    assert_eq!(p.mark_near(500, true).map(|m| m.at), Some(900));
    assert_eq!(p.mark_near(500, false).map(|m| m.at), Some(100));
    assert!(p.mark_near(900, true).is_none());
    // a held key adds the same marker again and again: it stays one
    let n = p.marks.len();
    let a = p.add_mark(900, 0, None);
    assert_eq!(p.marks.len(), n);
    assert_eq!(a, p.marks.iter().find(|m| m.at == 900).map(|m| m.id));
}

#[test]
fn volume_automation_follows_its_points_across_block_boundaries() {
    let mut p = Project::new(48000);
    let src = Arc::new(Source::new(1, "dc".into(), vec![vec![0.5; 4000]]));
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    p.add_clip(ti, src, 0);
    p.tracks[0].pan = -1.0;
    let flat = p.render(0, 4000, None).0;
    p.tracks[0].auto_set(1000, 0.0);
    p.tracks[0].auto_set(2000, -20.0);
    p.tracks[0].auto_set(1000, 0.0);
    assert_eq!(p.tracks[0].auto.len(), 2);
    let a = p.render(0, 4000, None).0;
    assert!((a[500] - flat[500]).abs() < 1e-6, "held before the first point");
    assert!((a[1500] / flat[1500] - 10f32.powf(-10.0 / 20.0)).abs() < 1e-3, "halfway: -10 dB");
    assert!((a[3500] / flat[3500] - 0.1).abs() < 1e-3, "held after the last point");
    // the same result when rendered in small blocks
    let mut l = vec![0.0f32; 4000];
    let mut at = 0;
    for n in [300usize, 777, 1, 1500, 1422] {
        let (mut bl, mut br) = (vec![0.0f32; n], vec![0.0f32; n]);
        p.mix(at as i64, &mut bl, &mut br, None, &mut [[0.0; 2]]);
        l[at..at + n].copy_from_slice(&bl);
        at += n;
    }
    assert!(l.iter().zip(&a).all(|(x, y)| (x - y).abs() < 1e-5));
    assert!(p.tracks[0].auto_set(5, 99.0).is_some() && p.tracks[0].auto[0].1 == AUTO_RANGE.1);
}

#[test]
fn a_drawn_track_is_not_in_the_master_mix_but_a_node_can_take_it() {
    let mut p = Project::new(48000);
    let n = p.add_draw_track();
    let id = p.tracks[n].id;
    // a ramp from -1 at frame 100 to 1 at frame 300, drawn in two strokes (the second replaces part of the first)
    p.tracks[n].curve_stroke(100, -1.0, 200, 0.0);
    p.tracks[n].curve_stroke(150, 0.5, 300, 1.0);
    assert!(p.tracks[n].curve.windows(2).all(|w| w[0].0 < w[1].0));
    let (mut l, mut r, mut pk) = (vec![9.0f32; 400], vec![9.0f32; 400], vec![[0.0f32; 2]; 4]);
    p.mix(0, &mut l, &mut r, None, &mut pk);
    assert!(l.iter().chain(r.iter()).all(|v| *v == 0.0), "muted for the master");
    p.mix(0, &mut l, &mut r, Some(id), &mut pk);
    assert_eq!(l, r);
    assert_eq!((l[0], l[100], l[300], l[399]), (-1.0, -1.0, 1.0, 1.0), "held beyond the ends");
    assert!((l[150] - 0.5).abs() < 1e-5 && l[125] > -1.0 && l[125] < 0.5, "{} {}", l[150], l[125]);
}

#[test]
fn mix_places_clip_and_applies_gain_and_pan() {
    let (mut p, _) = proj_with_clip(1000);
    p.tracks[0].vol_db = -6.0206;
    p.tracks[0].pan = -1.0;
    let (mut l, mut r) = (vec![0f32; 1200], vec![0f32; 1200]);
    let mut pk = vec![[0f32; 2]; 1];
    p.mix(0, &mut l, &mut r, None, &mut pk);
    assert_eq!(l[50], 0.0);
    // frame 100 + 500 = ramp 0.5, -6 dB, hard left
    assert!((l[600] - 0.25).abs() < 1e-3, "{}", l[600]);
    assert!(r[600].abs() < 1e-6);
    assert!(pk[0][0] > 0.4);
    assert_eq!(l[1150], 0.0);
}

#[test]
fn channel_routing_and_split() {
    let mut p = Project::new(48000);
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    let s = Arc::new(Source::new(2, "st".into(), vec![vec![0.5; 200], vec![0.25; 200]]));
    p.add_clip(ti, s, 0);
    let at = |p: &Project| {
        let (l, r) = p.render(10, 20, None);
        (l[0], r[0])
    };
    assert_eq!(at(&p), (0.5, 0.25));
    p.tracks[0].ch_mute[1] = true;
    assert_eq!(at(&p), (0.5, 0.0));
    p.tracks[0].ch_mute[1] = false;
    p.tracks[0].chan = Chan::Mono;
    let (l, r) = at(&p);
    assert!((l - r).abs() < 1e-6 && l > 0.2);
    p.tracks[0].chan = Chan::Stereo;
    let (a, b) = (p.tracks[0].id, p.split_channels(0).unwrap());
    assert_eq!(p.tracks.len(), 2);
    assert_eq!((p.tracks[0].chan, p.tracks[b].chan), (Chan::Left, Chan::Right));
    assert_ne!(a, p.tracks[b].id);
    // left stays hard left at full level, right hard right
    let (l, r) = at(&p);
    assert!((l - 0.5).abs() < 1e-5 && (r - 0.25).abs() < 1e-5, "{l} {r}");
}

#[test]
fn overlapping_clips_crossfade_instead_of_clicking() {
    let mut p = Project::new(48000);
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    let flat = |v: f32, n: usize| Arc::new(Source::new(9, "c".into(), vec![vec![v; n]]));
    p.add_clip(ti, flat(1.0, 4800), 0);
    // a quieter clip lies on top of the middle of the first one (added last = on top)
    p.add_clip(ti, flat(0.5, 960), 2000);
    let (l, _) = p.render(0, 4800, None);
    let pan = 1.0 / std::f32::consts::SQRT_2; // mono material is panned with -3 dB at the centre
    // untouched parts are as before, the top clip replaces what is under it in the middle
    assert!((l[500] - pan).abs() < 1e-3);
    assert!((l[2480] - 0.5 * pan).abs() < 1e-3, "{}", l[2480]);
    assert!((l[4000] - pan).abs() < 1e-3);
    // around the edges the level moves smoothly: no jump bigger than a small step between samples
    let worst = (1990..3000).map(|i| (l[i + 1] - l[i]).abs()).fold(0f32, f32::max);
    assert!(worst < 0.05, "step {worst}");
    // a clip without anything under it keeps its hard edge
    let mut q = Project::new(48000);
    let t2 = q.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    q.add_clip(t2, flat(1.0, 1000), 100);
    let (m, _) = q.render(0, 1200, None);
    assert!((m[100] - pan).abs() < 1e-3 && (m[1099] - pan).abs() < 1e-3);
}

#[test]
fn the_crossfade_length_of_a_clip_can_be_set() {
    let flat = |v: f32, n: usize| Arc::new(Source::new(9, "c".into(), vec![vec![v; n]]));
    let level_at = |xf: usize, frame: usize| -> f32 {
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        p.add_clip(ti, flat(1.0, 20000), 0);
        let top = p.add_clip(ti, flat(0.0, 10000), 5000);
        p.tracks[ti].clips.iter_mut().find(|c| c.id == top).unwrap().xf = xf;
        p.render(0, 20000, None).0[frame]
    };
    let pan = 1.0 / std::f32::consts::SQRT_2;
    // 3000 frames into the overlap: with the default 12 ms the top clip covers everything, with 100 ms it is half way
    assert!(level_at(0, 5000 + 3000) < 0.01 * pan);
    let half = level_at(4800, 5000 + 2400) / pan;
    assert!((half - 0.5).abs() < 0.05, "{half}");
    // a long crossfade is smooth
    let a = level_at(4800, 5000 + 1200) / pan;
    let b = level_at(4800, 5000 + 3600) / pan;
    assert!(a > half && half > b);
}

#[test]
fn recording_over_shelves_the_old_take_and_comping_swaps_ranges() {
    let flat = |id: u32, v: f32, n: usize| Arc::new(Source::new(id, "c".into(), vec![vec![v; n]]));
    let mut p = Project::new(48000);
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    p.add_clip(ti, flat(1, 0.25, 10000), 0);
    // a second take over the middle: the old audio there goes to the shelf, the rest stays
    let id = p.record_over(ti, flat(2, 0.5, 4000), 3000, None).unwrap();
    assert_eq!(p.tracks[ti].shelf.len(), 1);
    assert_eq!(p.tracks[ti].shelf[0].0, 1);
    let pan = 1.0 / std::f32::consts::SQRT_2;
    let (l, _) = p.render(0, 10000, None);
    assert!((l[1000] / pan - 0.25).abs() < 1e-3 && (l[4000] / pan - 0.5).abs() < 1e-3 && (l[8000] / pan - 0.25).abs() < 1e-3, "{} {} {}", l[1000], l[4000], l[8000]);
    assert!(p.find_clip(id).is_some());
    // comping: take 1 for the range 3500..5500: the swap puts the old audio back there
    assert!(p.comp_swap(ti, 1, 3500, 5500));
    let (l, _) = p.render(0, 10000, None);
    assert!((l[4000] / pan - 0.25).abs() < 1e-3, "{}", l[4000]);
    assert!((l[3200] / pan - 0.5).abs() < 1e-3, "outside the range the new take stays");
    assert!((l[6500] / pan - 0.5).abs() < 1e-3 || (l[6500] / pan - 0.25).abs() < 1e-3);
    // doing it again swaps back
    assert!(p.comp_swap(ti, 1, 3500, 5500));
    let (l, _) = p.render(0, 10000, None);
    assert!((l[4000] / pan - 0.5).abs() < 1e-3, "{}", l[4000]);
    // nothing to swap for a take that does not exist there
    assert!(!p.comp_swap(ti, 9, 3500, 5500));
    // punch: only the range of the take is used
    let mut q = Project::new(48000);
    let tq = q.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    q.add_clip(tq, flat(1, 0.25, 10000), 0);
    q.record_over(tq, flat(2, 0.5, 10000), 0, Some((4000, 6000))).unwrap();
    let (l, _) = q.render(0, 10000, None);
    assert!((l[2000] / pan - 0.25).abs() < 1e-3 && (l[5000] / pan - 0.5).abs() < 1e-3 && (l[8000] / pan - 0.25).abs() < 1e-3);
}

#[test]
fn mute_and_solo() {
    let (mut p, _) = proj_with_clip(500);
    let t2 = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    let s = src(500);
    p.add_clip(t2, s, 100);
    p.tracks[0].mute = true;
    let (l, _) = p.render(100, 600, None);
    let (l1, _) = p.render(100, 600, Some(p.tracks[1].id));
    assert!((l[250] - l1[250]).abs() < 1e-6);
    p.tracks[0].mute = false;
    p.tracks[1].solo = true;
    let (l2, _) = p.render(100, 600, None);
    assert!((l2[250] - l1[250]).abs() < 1e-6);
    // `only` ignores mute
    p.tracks[1].mute = true;
    assert!(p.render(100, 600, Some(p.tracks[1].id)).0[250] > 0.0);
}

#[test]
fn split_and_reverse_split_cover_the_same_audio() {
    for rev in [false, true] {
        let (mut p, id) = proj_with_clip(1000);
        p.clip_mut(id).unwrap().reverse = rev;
        let before = p.render(0, 1200, None).0;
        assert_eq!(p.split_at(400, None), 1);
        assert_eq!(p.tracks[0].clips.len(), 2);
        let after = p.render(0, 1200, None).0;
        for i in 0..1200 {
            assert!((before[i] - after[i]).abs() < 1e-6, "rev {rev} frame {i}");
        }
        let total: usize = p.tracks[0].clips.iter().map(|c| c.len).sum();
        assert_eq!(total, 1000);
    }
}

#[test]
fn trim_keeps_alignment_of_the_audio() {
    for rev in [false, true] {
        let (mut p, id) = proj_with_clip(1000);
        p.clip_mut(id).unwrap().reverse = rev;
        let before = p.render(0, 1200, None).0;
        {
            let c = p.clip_mut(id).unwrap();
            c.set_start(300);
            c.set_end(900);
        }
        let c = &p.tracks[0].clips[0];
        assert_eq!((c.start, c.end()), (300, 900));
        let after = p.render(0, 1200, None).0;
        for i in 300..900 {
            assert!((before[i] - after[i]).abs() < 1e-6, "rev {rev} frame {i}");
        }
        assert_eq!(after[200], 0.0);
        // extending again restores the original audio
        let c = p.clip_mut(id).unwrap();
        c.set_start(100);
        c.set_end(1100);
        assert_eq!((c.start, c.len), (100, 1000));
        let again = p.render(0, 1200, None).0;
        for i in 0..1200 {
            assert!((before[i] - again[i]).abs() < 1e-6, "rev {rev} re-extend frame {i}");
        }
    }
}

#[test]
fn delete_range_leaves_gap() {
    let (mut p, _) = proj_with_clip(1000);
    let before = p.render(0, 1200, None).0;
    p.delete_range(300, 500, &[]);
    let after = p.render(0, 1200, None).0;
    for i in 0..1200 {
        let want = if (300..500).contains(&i) { 0.0 } else { before[i] };
        assert!((after[i] - want).abs() < 1e-6, "frame {i}");
    }
}

#[test]
fn fades_and_normalize() {
    let (mut p, id) = proj_with_clip(1000);
    {
        let c = p.clip_mut(id).unwrap();
        c.fade_in = 100;
        c.fade_out = 100;
    }
    let (l, _) = p.render(0, 1200, None);
    assert!(l[100] < 0.01);
    assert!(l[1099] < 0.2);
    assert!(p.normalize_clip(id, -6.0));
    let c = p.clip_mut(id).unwrap();
    assert!((c.peak() * db_to_lin(c.gain_db) - 0.5012).abs() < 1e-3);
}

#[test]
fn resampler_keeps_a_tone() {
    let sr = 44100.0f32;
    let x: Vec<f32> = (0..44100).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin() * 0.8).collect();
    let y = resample(&x, 44100, 48000);
    assert!((y.len() as i64 - 48000).abs() <= 1);
    let (mut num, mut den) = (0f64, 0f64);
    for (i, &v) in y.iter().enumerate().skip(2000).take(40000) {
        let want = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin() * 0.8;
        num += ((v - want) as f64).powi(2);
        den += (want as f64).powi(2);
    }
    assert!(num / den < 1e-4, "error {}", num / den);
    let z = resample(&y, 48000, 22050);
    assert!((z.len() as i64 - 22050).abs() <= 1);
}

#[test]
fn input_lanes() {
    assert_eq!(Mode::Chan(3).lanes(8), vec![3]);
    assert_eq!(Mode::Pair(1).lanes(8), vec![2, 3]);
    assert_eq!(Mode::Pair(1).lanes(3), vec![2]);
    assert_eq!(Mode::Mix.lanes(2), vec![2]);
    assert_eq!(Mode::Chan(9).lanes(2), vec![1]);
}

#[test]
fn ids_run_out_without_a_panic() {
    // a damaged project file can use the very last id
    let mut p = Project::new(48000);
    p.reserve_id(u32::MAX);
    let a = p.next();
    let b = p.next();
    assert_eq!(a, u32::MAX);
    assert_eq!(b, u32::MAX);
    let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
    assert_eq!(ti, 0);
}

#[test]
fn the_converted_length_is_known_before_converting() {
    for (n, from, to) in [(1000usize, 44100u32, 48000u32), (5000, 48000, 8000), (3001, 8000, 48000), (7, 48000, 48000), (123_456, 22050, 96000)] {
        let x = vec![0.1f32; n];
        assert_eq!(resampled_len(n, from, to), resample(&x, from, to).len() as u64, "{n} {from} -> {to}");
    }
}
