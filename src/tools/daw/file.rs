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

pub fn write_wav(path: &Path, src: &Source, sr: u32) -> std::io::Result<()> {
    let ch = src.ch.len().min(2) as u16;
    let mut w = WavWriter::create(path, Format::Float32, ch, sr)?;
    let r = if src.ch.len() > 1 { &src.ch[1] } else { &src.ch[0] };
    for (a, b) in src.ch[0].chunks(1 << 16).zip(r.chunks(1 << 16)) {
        w.write(a, b)?;
    }
    w.finish()
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
    for c in proj.tracks.iter().flat_map(|t| t.clips.iter()) {
        if seen.contains(&c.src.id) {
            continue;
        }
        seen.push(c.src.id);
        let file = format!("src{}.wav", c.src.id);
        let target = media.join(&file);
        let already = c.src.saved.lock().as_ref().is_some_and(|p| *p == target && p.exists());
        if !already {
            write_wav(&target, &c.src, proj.sr).map_err(|e| format!("{}: {e}", target.display()))?;
            *c.src.saved.lock() = Some(target);
        }
        let _ = writeln!(out, "src {}|{}/{}|{}", c.src.id, media_name, file, esc(&c.src.name));
    }
    for t in &proj.tracks {
        let _ = writeln!(
            out,
            "track {}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
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
            t.ch_mute[1] as u8
        );
        for c in &t.clips {
            let _ = writeln!(out, "clip {}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}", t.id, c.id, c.src.id, c.start, c.off, c.len, c.gain_db, c.fade_in, c.fade_out, c.reverse as u8, c.muted as u8, esc(&c.name));
        }
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
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
            if f.len() >= 3 {
                if let Ok(id) = f[0].parse::<u32>() {
                    files.insert(id, (base.join(f[1]), unesc(f[2])));
                    max_src = max_src.max(id);
                }
            }
        }
    }
    for l in lines {
        if let Some(v) = l.strip_prefix("sr ") {
            proj.sr = v.trim().parse().unwrap_or(48000).clamp(8000, 384000);
        } else if let Some(v) = l.strip_prefix("master ") {
            proj.master_db = v.trim().parse().unwrap_or(0.0);
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
                name: unesc(f[1]),
                color: f[2].parse::<usize>().unwrap_or(0) % COLOR_COUNT,
                clips: Vec::new(),
                vol_db: num(f.get(3)).clamp(-90.0, 24.0),
                pan: num(f.get(4)).clamp(-1.0, 1.0),
                mute: f[5] == "1",
                solo: f[6] == "1",
                arm: false,
                input: Input { dev: unesc(f[8]), mode: mode_parse(f[9]) },
                h: f[7].parse::<u8>().unwrap_or(1).min(2),
            });
        } else if let Some(r) = l.strip_prefix("clip ") {
            let f: Vec<&str> = r.split('|').collect();
            if f.len() < 12 {
                continue;
            }
            let (Ok(tid), Ok(cid), Ok(sid)) = (f[0].parse::<u32>(), f[1].parse::<u32>(), f[2].parse::<u32>()) else { continue };
            proj.reserve_id(cid);
            let Some(ti) = proj.track_index(tid) else { continue };
            let sr = proj.sr;
            let src = srcs
                .entry(sid)
                .or_insert_with(|| {
                    let (p, name) = files.get(&sid)?;
                    let (fsr, chans) = crate::tools::decode::read_all(p).ok()?;
                    let chans: Vec<Vec<f32>> = chans.into_iter().take(2).map(|c| resample(&c, fsr, sr)).collect();
                    let s = Source::new(sid, name.clone(), chans);
                    *s.saved.lock() = Some(p.clone());
                    Some(Arc::new(s))
                })
                .clone();
            let Some(src) = src else {
                missing += 1;
                continue;
            };
            let (off, len) = (f[4].parse::<usize>().unwrap_or(0), f[5].parse::<usize>().unwrap_or(0));
            if len == 0 || off + len > src.len() {
                missing += 1;
                continue;
            }
            let mut c = Clip {
                id: cid,
                src,
                start: f[3].parse().unwrap_or(0),
                off,
                len,
                gain_db: num(f.get(6)).clamp(-90.0, 36.0),
                fade_in: f[7].parse().unwrap_or(0),
                fade_out: f[8].parse().unwrap_or(0),
                reverse: f[9] == "1",
                muted: f[10] == "1",
                name: unesc(f[11]),
            };
            c.clamp_fades();
            proj.tracks[ti].clips.push(c);
        }
    }
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
        p.tracks[0].vol_db = -4.5;
        p.tracks[0].pan = 0.25;
        p.tracks[0].solo = true;
        p.master_db = -1.0;
        save(&p, &path).unwrap();
        let q = load(&path).unwrap();
        assert_eq!(q.missing, 0);
        let q = q.proj;
        assert_eq!((q.sr, q.master_db), (44100, -1.0));
        let t = &q.tracks[0];
        assert_eq!((t.name.as_str(), t.input.dev.as_str(), t.input.mode), ("Gtr | left", "dev|1", Mode::Pair(1)));
        assert!(t.solo && (t.vol_db + 4.5).abs() < 1e-6 && (t.pan - 0.25).abs() < 1e-6);
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
}
