//! Project files: a small text file plus a `<name>_media` folder with one 32-bit float WAV per source.

use super::project::*;
use crate::tools::wavio::{Format, WavWriter};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

fn esc(s: &str) -> String {
    s.replace('%', "%25").replace('|', "%7C").replace('\n', "%0A").replace('\r', "")
}

fn unesc(s: &str) -> String {
    s.replace("%0A", "\n").replace("%7C", "|").replace("%25", "%")
}

pub fn media_dir(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "project".into());
    path.with_file_name(format!("{stem}_media"))
}

/// Write a WAV file so that a crash, a full disk or a failing write leaves the previous file as it was: the audio goes into a
/// temporary file next to it that replaces the target only when it is complete and on disk. `fill` writes the frames.
pub fn write_wav_atomic(path: &Path, fmt: Format, ch: u16, sr: u32, fill: impl FnOnce(&mut WavWriter) -> std::io::Result<()>) -> std::io::Result<()> {
    crate::fsutil::atomic_write_with(path, |tmp| {
        let mut w = WavWriter::create(tmp, fmt, ch, sr)?;
        fill(&mut w)?;
        w.finish()?;
        // (the writer has closed the file; the rename must not get ahead of the data)
        // (best effort: a virus scanner holding the fresh file must not fail the whole save)
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(tmp) {
            let _ = f.sync_all();
        }
        Ok(())
    })
}

pub fn write_wav(path: &Path, src: &Source, sr: u32) -> std::io::Result<()> {
    let ch = src.ch.len().min(2) as u16;
    write_wav_atomic(path, Format::Float32, ch, sr, |w| {
        let r = if src.ch.len() > 1 { &src.ch[1] } else { &src.ch[0] };
        for (a, b) in src.ch[0].chunks(1 << 16).zip(r.chunks(1 << 16)) {
            w.write(a, b)?;
        }
        Ok(())
    })
}

fn mode_str(m: Mode) -> String {
    match m {
        Mode::Chan(c) => format!("c{c}"),
        Mode::Pair(k) => format!("p{k}"),
        Mode::Mix => "m".into(),
    }
}

fn mode_parse(s: &str) -> Mode {
    match s.chars().next() {
        Some('c') => Mode::Chan(s[1..].parse().unwrap_or(0)),
        Some('p') => Mode::Pair(s[1..].parse().unwrap_or(0)),
        _ => Mode::Mix,
    }
}

pub fn save(proj: &Project, path: &Path) -> Result<(), String> {
    use std::fmt::Write;
    let media = media_dir(path);
    std::fs::create_dir_all(&media).map_err(|e| e.to_string())?;
    let media_name = media.file_name().unwrap().to_string_lossy().to_string();
    let mut out = String::from("VSDAW 1\n");
    let _ = writeln!(out, "sr {}\nmaster {}", proj.sr, proj.master_db);
    let mut seen: Vec<u32> = Vec::new();
    for c in proj.tracks.iter().flat_map(|t| t.clips.iter().chain(t.shelf.iter().map(|(_, c)| c))) {
        if seen.contains(&c.src.id) {
            continue;
        }
        seen.push(c.src.id);
        let file = format!("src{}.wav", c.src.id);
        let target = media.join(&file);
        if !c.src.is_saved_at(&target) {
            write_wav(&target, &c.src, proj.sr).map_err(|e| format!("{}: {e}", target.display()))?;
            c.src.mark_saved(target);
        }
        let _ = writeln!(out, "src {}|{}/{}|{}", c.src.id, media_name, file, esc(&c.src.name));
    }
    for m in &proj.marks {
        let _ = writeln!(out, "mark {}|{}|{}|{}", m.id, m.at, m.len, esc(&m.name));
    }
    for t in &proj.tracks {
        let _ = writeln!(
            out,
            "track {}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            t.id,
            esc(&t.name),
            t.color,
            t.vol_db,
            t.pan,
            t.mute as u8,
            t.solo as u8,
            t.h,
            esc(&t.input.dev),
            mode_str(t.input.mode),
            t.chan.index(),
            t.ch_db[0],
            t.ch_db[1],
            t.ch_mute[0] as u8,
            t.ch_mute[1] as u8,
            t.in_db,
            t.in_ms,
            t.invert as u8,
            t.beats as u8
        );
        if t.draw {
            let _ = writeln!(out, "drawtrack {}", t.id);
            if !t.curve.is_empty() {
                let pts: Vec<String> = t.curve.iter().map(|(f, v)| format!("{f}:{v}")).collect();
                let _ = writeln!(out, "curve {}|{}", t.id, pts.join(","));
            }
        }
        if !t.auto.is_empty() {
            let pts: Vec<String> = t.auto.iter().map(|(f, d)| format!("{f}:{d}")).collect();
            let _ = writeln!(out, "auto {}|{}", t.id, pts.join(","));
        }
        for (take, c) in &t.shelf {
            let _ = writeln!(out, "shelf {}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}", t.id, c.id, c.src.id, c.start, c.off, c.len, c.gain_db, c.fade_in, c.fade_out, c.reverse as u8, c.muted as u8, esc(&c.name), c.xf, take);
        }
        for c in &t.clips {
            let _ = writeln!(out, "clip {}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}", t.id, c.id, c.src.id, c.start, c.off, c.len, c.gain_db, c.fade_in, c.fade_out, c.reverse as u8, c.muted as u8, esc(&c.name), c.xf);
        }
    }
    atomic_write(path, out.as_bytes()).map_err(|e| e.to_string())
}

pub struct Loaded {
    pub proj: Project,
    /// clips dropped because their audio file is gone
    pub missing: usize,
    pub max_src_id: u32,
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines = text.lines();
    if lines.next().map(|l| l.trim()) != Some("VSDAW 1") {
        return Err("not a VisuScope timeline project".into());
    }
    let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut proj = Project::new(48000);
    let mut files: HashMap<u32, (PathBuf, String)> = HashMap::new();
    let mut srcs: HashMap<u32, Option<Arc<Source>>> = HashMap::new();
    let mut max_src = 0;
    let mut missing = 0;
    let num = |s: Option<&&str>| s.and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
    // sources first so that clips can refer to them wherever they appear
    for l in text.lines() {
        if let Some(r) = l.strip_prefix("src ") {
            let f: Vec<&str> = r.split('|').collect();
            if f.len() >= 3
                && let Ok(id) = f[0].parse::<u32>() {
                    files.insert(id, (base.join(f[1]), unesc(f[2])));
                    max_src = max_src.max(id);
                }
        }
    }
    for l in lines {
        if let Some(v) = l.strip_prefix("sr ") {
            proj.sr = v.trim().parse().unwrap_or(48000).clamp(2000, 768000);
        } else if let Some(v) = l.strip_prefix("master ") {
            proj.master_db = v.trim().parse().unwrap_or(0.0);
        } else if let Some(r) = l.strip_prefix("drawtrack ") {
            if let Some(ti) = r.trim().parse::<u32>().ok().and_then(|id| proj.track_index(id)) {
                proj.tracks[ti].draw = true;
            }
        } else if let Some(r) = l.strip_prefix("curve ") {
            if let Some((id, pts)) = r.split_once('|')
                && let Some(ti) = id.trim().parse::<u32>().ok().and_then(|id| proj.track_index(id)) {
                    for pt in pts.split(',') {
                        if let Some((fr, v)) = pt.split_once(':')
                            && let (Ok(fr), Ok(v)) = (fr.trim().parse::<i64>(), v.trim().parse::<f32>())
                                && v.is_finite() && fr >= 0 {
                                    proj.tracks[ti].curve_set(fr.min(i64::MAX / 4), v);
                                }
                    }
                }
        } else if let Some(r) = l.strip_prefix("auto ") {
            if let Some((id, pts)) = r.split_once('|')
                && let Some(ti) = id.trim().parse::<u32>().ok().and_then(|id| proj.track_index(id)) {
                    for pt in pts.split(',') {
                        if let Some((fr, db)) = pt.split_once(':')
                            && let (Ok(fr), Ok(db)) = (fr.trim().parse::<i64>(), db.trim().parse::<f32>())
                                && db.is_finite() && fr >= 0 {
                                    proj.tracks[ti].auto_set(fr.min(i64::MAX / 4), db);
                                }
                    }
                }
        } else if let Some(r) = l.strip_prefix("mark ") {
            let f: Vec<&str> = r.split('|').collect();
            if let (true, Ok(id), Ok(at), Ok(len)) = (f.len() >= 4, f[0].parse::<u32>(), f.get(1).unwrap_or(&"").parse::<i64>(), f.get(2).unwrap_or(&"").parse::<i64>())
                && proj.marks.len() < Project::MAX_MARKS {
                    proj.reserve_id(id);
                    proj.marks.push(Mark { id, at: at.clamp(0, i64::MAX / 4), len: len.clamp(0, i64::MAX / 4), name: unesc(f[3]) });
                }
        } else if let Some(r) = l.strip_prefix("track ") {
            let f: Vec<&str> = r.split('|').collect();
            if f.len() < 10 {
                continue;
            }
            let id: u32 = f[0].parse().map_err(|_| "bad track id")?;
            proj.reserve_id(id);
            if proj.tracks.len() >= MAX_TRACKS {
                continue;
            }
            proj.tracks.push(Track {
                id,
                chan: Chan::from_index(f.get(10).and_then(|v| v.trim().parse().ok()).unwrap_or(0)),
                ch_db: [num(f.get(11)).clamp(-90.0, 24.0), num(f.get(12)).clamp(-90.0, 24.0)],
                ch_mute: [f.get(13) == Some(&"1"), f.get(14) == Some(&"1")],
                in_db: num(f.get(15)).clamp(-60.0, 40.0),
                in_ms: num(f.get(16)).clamp(-1000.0, 1000.0),
                name: unesc(f[1]),
                color: f[2].parse::<usize>().unwrap_or(0) % COLOR_COUNT,
                clips: Vec::new(),
                shelf: Vec::new(),
                vol_db: num(f.get(3)).clamp(-90.0, 24.0),
                pan: num(f.get(4)).clamp(-1.0, 1.0),
                mute: f[5] == "1",
                solo: f[6] == "1",
                arm: false,
                auto: Vec::new(),
                invert: f.get(17) == Some(&"1"),
                beats: f.get(18) == Some(&"1"),
                input: Input { dev: unesc(f[8]), mode: mode_parse(f[9]) },
                h: f[7].parse::<u8>().unwrap_or(1).min(2),
                draw: false,
                curve: Vec::new(),
            });
        } else if let Some((r, shelved)) = l.strip_prefix("clip ").map(|r| (r, false)).or_else(|| l.strip_prefix("shelf ").map(|r| (r, true))) {
            let f: Vec<&str> = r.split('|').collect();
            if f.len() < 12 {
                continue;
            }
            let (Ok(tid), Ok(cid), Ok(sid)) = (f[0].parse::<u32>(), f[1].parse::<u32>(), f[2].parse::<u32>()) else { continue };
            proj.reserve_id(cid);
            let Some(ti) = proj.track_index(tid) else { continue };
            let sr = proj.sr;
            // (a clip further away than a day is a damaged file: rendering up to it would never fit into memory)
            let max_start = Project::max_start(sr);
            let src = srcs
                .entry(sid)
                .or_insert_with(|| {
                    let (p, name) = files.get(&sid)?;
                    let (fsr, chans) = crate::tools::decode::read_all(p).ok()?;
                    let chans: Vec<Vec<f32>> = chans.into_iter().take(2).map(|c| resample(&c, fsr, sr)).collect();
                    let s = Source::new(sid, name.clone(), chans);
                    s.mark_saved(p.clone());
                    Some(Arc::new(s))
                })
                .clone();
            let Some(src) = src else {
                missing += 1;
                continue;
            };
            let (off, len) = (f[4].parse::<usize>().unwrap_or(0), f[5].parse::<usize>().unwrap_or(0));
            if len == 0 || off.checked_add(len).is_none_or(|e| e > src.len()) {
                missing += 1;
                continue;
            }
            let mut c = Clip {
                id: cid,
                src,
                start: f[3].parse::<i64>().unwrap_or(0).clamp(-max_start, max_start),
                off,
                len,
                gain_db: num(f.get(6)).clamp(-90.0, 36.0),
                fade_in: f[7].parse().unwrap_or(0),
                fade_out: f[8].parse().unwrap_or(0),
                reverse: f[9] == "1",
                muted: f[10] == "1",
                xf: f.get(12).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0).min(48000 * 30),
                name: unesc(f[11]),
            };
            c.clamp_fades();
            if shelved {
                let take = f.get(13).and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(1);
                proj.tracks[ti].shelf.push((take, c));
            } else {
                proj.tracks[ti].clips.push(c);
            }
        }
    }
    proj.marks.sort_by_key(|m| m.at);
    Ok(Loaded { proj, missing, max_src_id: max_src })
}

/// Ids for sources created anywhere (import, record, bounce).
pub fn next_src_id(c: &AtomicU32) -> u32 {
    c.fetch_add(1, Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_and_crossfade_lengths_survive_the_project_file() {
        let dir = std::env::temp_dir().join("visuscope_daw_test_takes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.vsproj");
        let mut p = Project::new(44100);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let flat = |id: u32, v: f32| Arc::new(Source::new(id, format!("s{id}"), vec![vec![v; 8000]]));
        p.add_clip(ti, flat(1, 0.25), 0);
        let c2 = p.record_over(ti, flat(2, 0.5), 2000, None).unwrap();
        p.clip_mut(c2).unwrap().xf = 1234;
        save(&p, &path).unwrap();
        let l = load(&path).unwrap();
        let t = &l.proj.tracks[0];
        assert_eq!(t.shelf.len(), p.tracks[0].shelf.len());
        assert_eq!(t.shelf[0].0, 1);
        assert_eq!(t.shelf[0].1.len, p.tracks[0].shelf[0].1.len);
        assert_eq!(t.clips.iter().map(|c| c.xf).max(), Some(1234));
        assert_eq!(t.clips.len(), p.tracks[0].clips.len());
    }

    #[test]
    fn project_roundtrip() {
        let dir = std::env::temp_dir().join("visuscope_daw_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.vsproj");
        let mut p = Project::new(44100);
        let ti = p.add_track(Some("Gtr | left".into()), Input { dev: "dev|1".into(), mode: Mode::Pair(1) });
        let l: Vec<f32> = (0..5000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let src = Arc::new(Source::new(7, "take 1".into(), vec![l.clone(), l.iter().map(|v| -v).collect()]));
        let cid = p.add_clip(ti, src, 1234);
        {
            let c = p.clip_mut(cid).unwrap();
            c.gain_db = -3.0;
            c.fade_in = 100;
            c.reverse = true;
            c.set_start(1534);
        }
        p.tracks[0].in_db = -3.5;
        p.tracks[0].in_ms = 12.5;
        p.tracks[0].chan = Chan::Right;
        p.tracks[0].vol_db = -4.5;
        p.tracks[0].pan = 0.25;
        p.tracks[0].solo = true;
        p.tracks[0].invert = true;
        p.tracks[0].auto_set(300, -6.0);
        p.tracks[0].auto_set(900, 3.5);
        p.add_mark(2000, 0, Some("drop | here".into()));
        p.add_mark(500, 1500, None);
        p.master_db = -1.0;
        save(&p, &path).unwrap();
        let q = load(&path).unwrap();
        assert_eq!(q.missing, 0);
        let q = q.proj;
        assert_eq!((q.sr, q.master_db), (44100, -1.0));
        let t = &q.tracks[0];
        assert_eq!((t.name.as_str(), t.input.dev.as_str(), t.input.mode), ("Gtr | left", "dev|1", Mode::Pair(1)));
        assert!((t.in_db + 3.5).abs() < 1e-6 && (t.in_ms - 12.5).abs() < 1e-6 && t.chan == Chan::Right);
        assert!(t.solo && (t.vol_db + 4.5).abs() < 1e-6 && (t.pan - 0.25).abs() < 1e-6);
        assert!(t.invert);
        assert_eq!(t.auto, vec![(300, -6.0), (900, 3.5)]);
        assert_eq!(q.marks.len(), 2);
        assert_eq!((q.marks[0].at, q.marks[0].len, q.marks[0].name.as_str()), (500, 1500, "Region 1"));
        assert_eq!((q.marks[1].at, q.marks[1].len, q.marks[1].name.as_str()), (2000, 0, "drop | here"));
        let (a, b) = (&p.tracks[0].clips[0], &t.clips[0]);
        assert_eq!((a.start, a.off, a.len, a.fade_in, a.reverse), (b.start, b.off, b.len, b.fade_in, b.reverse));
        assert!(b.src.stereo());
        let (x, _) = p.render(1200, 5200, None);
        let (y, _) = q.render(1200, 5200, None);
        for i in 0..x.len() {
            assert!((x[i] - y[i]).abs() < 1e-6, "frame {i}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_project_files_never_panic_and_play_safely() {
        let dir = std::env::temp_dir().join(format!("visuscope_daw_fuzz_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.vsproj");
        let mut p = Project::new(48000);
        for k in 0..3 {
            let ti = p.add_track(Some(format!("tr {k}")), Input { dev: String::new(), mode: Mode::Mix });
            let l: Vec<f32> = (0..3000).map(|i| (i as f32 * 0.02).sin() * 0.4).collect();
            let src = Arc::new(Source::new(10 + k, format!("s{k}"), vec![l.clone(), l]));
            let c = p.add_clip(ti, src.clone(), 100 * k as i64);
            p.clip_mut(c).unwrap().fade_in = 50;
            p.add_clip(ti, src, 4000);
        }
        p.tracks[0].auto_set(100, -3.0);
        p.tracks[0].auto_set(3000, 5.0);
        p.tracks[1].invert = true;
        p.add_mark(500, 0, Some("a".into()));
        p.add_mark(900, 400, Some("b".into()));
        save(&p, &path).unwrap();
        let good = std::fs::read(&path).unwrap();
        let mut seed = 0xD1B5_4A32_D192_ED03u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let fuzz = dir.join("fuzz.vsproj");
        for case in 0..800 {
            let mut b = good.clone();
            for _ in 0..1 + next() % 8 {
                let i = (next() % b.len() as u64) as usize;
                b[i] = match next() % 5 {
                    0 => b'9',
                    1 => b'|',
                    2 => b'-',
                    3 => b'\n',
                    _ => next() as u8,
                };
            }
            if next() % 4 == 0 {
                b.truncate((next() % b.len() as u64) as usize);
            }
            std::fs::write(&fuzz, &b).unwrap();
            let r = std::panic::catch_unwind(|| {
                if let Ok(l) = load(&fuzz) {
                    // a loaded project can be mixed everywhere without a panic, and the clips lie inside their audio
                    let q = l.proj;
                    for t in &q.tracks {
                        for c in &t.clips {
                            assert!(c.len >= 1 && c.off + c.len <= c.src.len(), "clip out of its source");
                        }
                    }
                    let end = q.end_frame().clamp(0, 20_000);
                    let (l, r) = q.render(0, end, None);
                    assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()), "damaged automation made a non-finite sample");
                    let _ = q.render(-500, 500, None);
                    assert!(q.marks.iter().all(|m| m.at >= 0 && m.len >= 0));
                    assert!(q.tracks.iter().all(|t| t.auto.windows(2).all(|w| w[0].0 < w[1].0) && t.auto.iter().all(|(f, d)| *f >= 0 && d.is_finite())));
                }
            });
            assert!(r.is_ok(), "case {case} panicked");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_wav_write_leaves_the_previous_file_untouched() {
        let dir = std::env::temp_dir().join(format!("visuscope_daw_wav_atomic_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("src1.wav");
        let src = Source::new(1, "s".into(), vec![(0..5000).map(|i| (i as f32 * 0.01).sin()).collect()]);
        write_wav(&path, &src, 48000).unwrap();
        let good = std::fs::read(&path).unwrap();
        // a write that breaks half way (disk full, crash): the file that was there stays as it was
        let err = write_wav_atomic(&path, Format::Float32, 1, 48000, |w| {
            w.write(&[0.5; 1000], &[0.5; 1000])?;
            Err(std::io::Error::other("disk full"))
        });
        assert!(err.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), good);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file stays behind");
        // and a good write replaces it completely
        let other = Source::new(2, "t".into(), vec![vec![0.25; 100]]);
        write_wav(&path, &other, 48000).unwrap();
        assert_eq!(crate::tools::decode::read_all(&path).unwrap().1[0].len(), 100);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clip_placed_years_away_is_pulled_back_on_load() {
        let dir = std::env::temp_dir().join(format!("visuscope_daw_far_clip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.vsproj");
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        p.add_clip(ti, Arc::new(Source::new(1, "s".into(), vec![vec![0.1; 500]])), 700);
        save(&p, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let damaged: Vec<String> = text.lines().map(|l| if l.starts_with("clip ") { l.replacen("|700|", "|1000000000000|", 1) } else { l.to_string() }).collect();
        assert_ne!(damaged.join("
"), text.trim_end());
        std::fs::write(&path, damaged.join("
")).unwrap();
        let q = load(&path).unwrap().proj;
        let c = &q.tracks[0].clips[0];
        assert_eq!(c.start, Project::max_start(48000));
        // what a mixdown of that project would ask for is refused instead of allocated
        assert!(Project::render_refusal(0, q.end_frame(), 150_000_000, 48000).is_some());
        assert!(Project::render_refusal(0, 48000 * 600, 150_000_000, 48000).is_none());
        assert!(Project::render_refusal(5, 3, 10, 48000).is_none(), "an empty or backwards range is not too long");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_to_the_autosave_and_the_project_in_turn_writes_each_source_once_per_folder() {
        let dir = std::env::temp_dir().join(format!("visuscope_daw_two_targets_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (proj_file, auto_file) = (dir.join("mine.vsproj"), dir.join("auto").join("session.vsproj"));
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        p.add_clip(ti, Arc::new(Source::new(3, "s".into(), vec![vec![0.3; 2000]])), 0);
        save(&p, &proj_file).unwrap();
        save(&p, &auto_file).unwrap();
        // the copy in the project's folder is marked: the next save to it must not write it again
        let media = media_dir(&proj_file).join("src3.wav");
        std::fs::write(&media, b"sentinel").unwrap();
        save(&p, &proj_file).unwrap();
        assert_eq!(std::fs::read(&media).unwrap(), b"sentinel", "an autosave in between made the project rewrite its media");
        // a file that has gone is written again
        std::fs::remove_file(&media).unwrap();
        save(&p, &proj_file).unwrap();
        assert!(media.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), SERIAL.fetch_add(1, Relaxed)));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(windows)] {
            use std::os::windows::ffi::OsStrExt;
            let a: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let b: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe extern "system" { fn MoveFileExW(a: *const u16, b: *const u16, flags: u32) -> i32; }
            if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 1 | 8) } == 0 { return Err(std::io::Error::last_os_error()); }
        }
        #[cfg(not(windows))] std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(tmp); }
    result
}

#[cfg(test)] mod review_tests {
 use super::*;
 #[test] fn supported_rates_roundtrip_without_changing_clip_length() {
 let dir = std::env::temp_dir().join(format!("vs-rates-{}", std::process::id()));
 std::fs::create_dir_all(&dir).unwrap();
 for sr in [2000, 4000, 8000, 44100, 384000, 768000] {
 let mut p = Project::new(sr);
 let t = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
 p.add_clip(t, Arc::new(Source::new(sr, "tone".into(), vec![vec![0.2; 128]])), 13);
 let path = dir.join(format!("{sr}.vsproj")); save(&p, &path).unwrap();
 let loaded = load(&path).unwrap(); assert_eq!(loaded.proj.sr, sr); assert_eq!(loaded.missing, 0);
 assert_eq!(loaded.proj.tracks[0].clips[0].len, 128);
 }
 std::fs::remove_dir_all(dir).unwrap();
 }
 #[test] fn atomic_replace_preserves_previous_file_on_failure() {
 let dir = std::env::temp_dir().join(format!("vs-atomic-{}", std::process::id()));
 std::fs::create_dir_all(&dir).unwrap(); let p = dir.join("project");
 atomic_write(&p, b"old").unwrap(); atomic_write(&p, b"new").unwrap();
 assert_eq!(std::fs::read(&p).unwrap(), b"new");
 assert!(atomic_write(&dir, b"invalid").is_err());
 assert_eq!(std::fs::read(&p).unwrap(), b"new");
 std::fs::remove_dir_all(dir).unwrap();
 }
}
