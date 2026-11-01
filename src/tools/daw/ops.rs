//! Editing actions, project files, export and analysis scheduling.

use super::*;

impl Daw {
    pub(super) fn fit_view(&mut self) {
        let end = self.proj.lock().end_frame();
        let secs = (end as f64 / self.sr() as f64).max(4.0) * 1.04;
        self.t0 = 0.0;
        self.pps = (self.lane_w as f64 / secs).clamp(1.0, 50000.0);
    }

    // ------------------------------------------------------------------ clip edits (on the UI's copy of the project)

    fn sel_vec(&self) -> Vec<u32> {
        self.sel.iter().copied().collect()
    }

    pub(super) fn act_split(&mut self, p: &mut Project) {
        let t = self.pos();
        let sel = self.sel_vec();
        let any_sel = sel.iter().any(|id| p.find_clip(*id).is_some_and(|(ti, ci)| p.tracks[ti].clips[ci].start < t && p.tracks[ti].clips[ci].end() > t));
        self.checkpoint(p);
        let only = if any_sel { Some(&sel[..]) } else { None };
        // with a selected track, only that track is split
        if only.is_none() {
            if let Some(ti) = self.sel_track.and_then(|id| p.track_index(id)) {
                let ids: Vec<u32> = p.tracks[ti].clips.iter().map(|c| c.id).collect();
                p.split_at(t, Some(&ids));
                return;
            }
        }
        p.split_at(t, only);
    }

    pub(super) fn act_delete(&mut self, p: &mut Project) {
        if !self.sel.is_empty() {
            self.checkpoint(p);
            let sel = std::mem::take(&mut self.sel);
            for t in p.tracks.iter_mut() {
                t.clips.retain(|c| !sel.contains(&c.id));
            }
        } else if let Some((a, b)) = self.range {
            self.checkpoint(p);
            let tr: Vec<u32> = self.sel_track.into_iter().collect();
            p.delete_range(a, b, &tr);
        }
    }

    pub(super) fn act_duplicate(&mut self, p: &mut Project) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint(p);
        let ids = self.sel_vec();
        let (a, b) = ids.iter().filter_map(|id| p.find_clip(*id).map(|(ti, ci)| (p.tracks[ti].clips[ci].start, p.tracks[ti].clips[ci].end()))).fold((i64::MAX, 0), |(a, b), (s, e)| (a.min(s), b.max(e)));
        let shift = b - a;
        self.sel.clear();
        for id in ids {
            if let Some(start) = p.find_clip(id).map(|(ti, ci)| p.tracks[ti].clips[ci].start) {
                if let Some(n) = p.duplicate_clip(id, Some(start + shift)) {
                    self.sel.insert(n);
                }
            }
        }
    }

    pub(super) fn act_copy(&mut self, p: &mut Project, cut: bool) {
        let mut items: Vec<(Clip, i64, usize)> = Vec::new();
        let ids = self.sel_vec();
        let a = ids.iter().filter_map(|id| p.find_clip(*id).map(|(ti, ci)| p.tracks[ti].clips[ci].start)).min().unwrap_or(0);
        for id in &ids {
            if let Some((ti, ci)) = p.find_clip(*id) {
                let c = p.tracks[ti].clips[ci].clone();
                items.push((c.clone(), c.start - a, ti));
            }
        }
        if items.is_empty() {
            return;
        }
        self.clip_buf = items;
        if cut {
            self.act_delete(p);
        }
    }

    pub(super) fn act_paste(&mut self, p: &mut Project) {
        if self.clip_buf.is_empty() || p.tracks.is_empty() {
            return;
        }
        self.checkpoint(p);
        let first_ti = self.clip_buf.iter().map(|c| c.2).min().unwrap_or(0);
        let base = self.sel_track.and_then(|id| p.track_index(id)).unwrap_or(first_ti);
        let at = self.pos();
        self.sel.clear();
        for (c, rel, ti) in self.clip_buf.clone() {
            let target = (base + ti - first_ti).min(p.tracks.len() - 1);
            let mut n = c;
            n.id = p.next();
            n.start = at + rel;
            self.sel.insert(n.id);
            p.tracks[target].clips.push(n);
        }
    }

    pub(super) fn act_normalize(&mut self, p: &mut Project) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint(p);
        for id in self.sel_vec() {
            p.normalize_clip(id, -1.0);
        }
    }

    pub(super) fn act_clip_prop(&mut self, p: &mut Project, f: impl Fn(&mut Clip)) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint(p);
        for id in self.sel_vec() {
            if let Some(c) = p.clip_mut(id) {
                f(c);
            }
        }
    }

    pub(super) fn act_trim_to_range(&mut self, p: &mut Project) {
        let Some((a, b)) = self.range else { return };
        self.checkpoint(p);
        let ids: Vec<u32> = if self.sel.is_empty() { p.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect() } else { self.sel_vec() };
        for id in ids {
            if !p.trim_clip_to(id, a, b) {
                // outside the range: remove
                if let Some((ti, ci)) = p.find_clip(id) {
                    p.tracks[ti].clips.remove(ci);
                    self.sel.remove(&id);
                }
            }
        }
    }

    pub(super) fn act_select_all(&mut self, p: &Project) {
        self.sel = p.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
    }

    /// Render the range (or everything) of the mix, or of the selected track, into a new track.
    pub(super) fn act_bounce(&mut self, p: &Project, track_only: bool) {
        let (a, b) = self.analysis_range(p);
        if b <= a {
            return;
        }
        let only = if track_only { self.sel_track } else { None };
        let (pc, id) = (p.clone(), self.new_src_id());
        let name = match only.and_then(|t| p.track_index(t)) {
            Some(ti) => format!("{} (bounce)", p.tracks[ti].name),
            None => "Mix (bounce)".to_string(),
        };
        self.spawn_job("daw-bounce", move |jobs| {
            let src = pc.bounce(a, b, only, id, name);
            jobs.lock().push(Job::Bounced(src, a));
        });
    }

    /// Tracks chosen for merging: the ones selected with Ctrl + click plus the current track.
    pub(super) fn merge_ids(&self, p: &Project) -> Vec<u32> {
        let mut ids: Vec<u32> = self.sel_tracks.iter().copied().collect();
        if let Some(t) = self.sel_track {
            if !ids.contains(&t) && !ids.is_empty() {
                ids.push(t);
            }
        }
        ids.retain(|id| p.track_index(*id).is_some());
        ids
    }

    /// Mix the chosen tracks (as they sound: fader, pan, mute) into one new track over the time they cover. With
    /// `replace` the original tracks are removed afterwards (one undo step brings them back).
    pub(super) fn act_merge_tracks(&mut self, p: &Project, replace: bool) {
        let ids = self.merge_ids(p);
        if ids.len() < 2 {
            self.note = "Select two or more tracks first: Ctrl + click on their names".into();
            return;
        }
        let clips = p.tracks.iter().filter(|t| ids.contains(&t.id)).flat_map(|t| t.clips.iter());
        let (a, b) = clips.fold((i64::MAX, i64::MIN), |(a, b), c| (a.min(c.start), b.max(c.end())));
        if b <= a {
            self.note = "The selected tracks have no clips".into();
            return;
        }
        let first = ids.iter().filter_map(|id| p.track_index(*id)).min().unwrap_or(0);
        let mut q = p.clone();
        q.tracks.retain(|t| ids.contains(&t.id) && !t.mute);
        q.tracks.iter_mut().for_each(|t| t.solo = false);
        q.master_db = 0.0;
        let name = format!("Merged ({})", ids.iter().filter_map(|id| p.track_index(*id)).map(|i| p.tracks[i].name.clone()).take(3).collect::<Vec<_>>().join(" + "));
        let sid = self.new_src_id();
        let remove = if replace { ids } else { Vec::new() };
        self.spawn_job("daw-merge", move |jobs| {
            let src = q.bounce(a, b, None, sid, name);
            // the new track goes below the first merged track when the originals stay
            let index = if remove.is_empty() { first + 1 } else { first };
            jobs.lock().push(Job::Merged(src, a, remove, index));
        });
    }

    // ------------------------------------------------------------------ analysis

    /// The time range that analysis, bounce and looping refer to: the selection, else the selected
    /// clips, else the whole project.
    pub(super) fn analysis_range(&self, p: &Project) -> (i64, i64) {
        if let Some((a, b)) = self.range {
            return (a, b);
        }
        let ids = self.sel_vec();
        let clips: Vec<&Clip> = ids.iter().filter_map(|id| p.find_clip(*id).map(|(ti, ci)| &p.tracks[ti].clips[ci])).collect();
        if !clips.is_empty() {
            return (clips.iter().map(|c| c.start).min().unwrap(), clips.iter().map(|c| c.end()).max().unwrap());
        }
        (0, p.end_frame())
    }

    fn analysis_track(&self, p: &Project) -> Option<u32> {
        if self.an_src != AnSrc::Track {
            return None;
        }
        self.sel_track.filter(|t| p.track_index(*t).is_some()).or_else(|| self.sel.iter().next().and_then(|id| p.find_clip(*id)).map(|(ti, _)| p.tracks[ti].id))
    }

    pub(super) fn schedule_analysis(&mut self, p: &Project) {
        let (a, b) = self.analysis_range(p);
        let key = AnKey { rev: self.rev, a, b, track: self.analysis_track(p) };
        if self.an_want != Some(key) {
            self.an_want = Some(key);
            self.an_want_t = Instant::now();
        }
        // not while files are being loaded or recordings are written: that work comes first
        if self.an_done == Some(key) || self.an_running || self.busy.load(Relaxed) > 0 || self.an_want_t.elapsed() < Duration::from_millis(250) {
            return;
        }
        if b <= a {
            self.an_out = None;
            self.an_done = Some(key);
            return;
        }
        let (pc, sr) = (p.clone(), p.sr);
        self.an_running = true;
        let jobs = self.jobs.clone();
        // not counted as "busy": analysis runs quietly in the background
        std::thread::Builder::new()
            .name("daw-analysis".into())
            .spawn(move || {
                let max = (ANALYSIS_MAX_SECS * sr as usize) as i64;
                let b2 = key.b.min(key.a + max);
                let (l, r) = pc.render(key.a, b2, key.track);
                let out = analysis::analyze(l, r, sr, b2 < key.b);
                jobs.lock().push(Job::Analysis(key, Arc::new(out)));
            })
            .ok();
    }

    // ------------------------------------------------------------------ project files and export

    pub(super) fn save_project(&mut self, path: PathBuf, auto: bool) {
        let p = self.proj.lock().clone();
        self.saved_rev = self.rev;
        self.last_autosave = Instant::now();
        self.spawn_job("daw-save", move |jobs| {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let r = file::save(&p, &path).map(|_| path);
            jobs.lock().push(Job::Saved(r, auto));
        });
    }

    pub(super) fn open_project(&mut self, path: PathBuf, restore: bool) {
        self.spawn_job("daw-load", move |jobs| {
            let r = file::load(&path);
            jobs.lock().push(Job::Loaded(r, restore, path));
        });
    }

    pub(super) fn save_clicked(&mut self, save_as: bool) {
        match (&self.path, save_as) {
            (Some(p), false) => self.save_project(p.clone(), false),
            _ => {
                if self.proj_dlg.is_none() {
                    self.proj_dlg = Some((dialogs::save("Save timeline project", "Timeline project", &["vsproj"], "project.vsproj".into()), true));
                }
            }
        }
    }

    pub(super) fn open_clicked(&mut self) {
        if self.proj_dlg.is_none() {
            self.proj_dlg = Some((dialogs::open("Open timeline project", "Timeline project", &["vsproj"]), false));
        }
    }

    pub(super) fn new_project(&mut self, cx: &mut Ctx) {
        if self.rec.is_some() {
            self.stop_record(cx);
        }
        self.play_stop(cx);
        let sr = self.sr();
        *self.proj.lock() = Project::new(sr);
        self.undo.clear();
        self.redo.clear();
        self.sel.clear();
        self.sel_track = None;
        self.range = None;
        self.cursor = 0;
        self.path = None;
        self.an_out = None;
        self.an_done = None;
        self.rev += 1;
        self.saved_rev = self.rev;
    }

    pub(super) fn ask_export(&mut self, kind: Export) {
        if self.export_dlg.is_some() {
            return;
        }
        let d = match kind {
            Export::Mix(_) => dialogs::save("Export mix", "WAV", &["wav"], "mix.wav".into()),
            Export::Clip(_) => dialogs::save("Export clip", "WAV", &["wav"], "clip.wav".into()),
            Export::StemsDir => dialogs::pick_folder("Folder for the track files"),
        };
        self.export_dlg = Some((d, kind));
    }

    pub(super) fn run_export(&mut self, path: PathBuf, kind: Export) {
        let p = self.proj.lock().clone();
        let sr = p.sr;
        fn write(path: &std::path::Path, l: &[f32], r: &[f32], sr: u32) -> Result<(), String> {
            let peak = l.iter().chain(r).fold(0f32, |m, v| m.max(v.abs()));
            let fmt = if peak > 1.0 { Format::Float32 } else { Format::Pcm24 };
            let mut w = WavWriter::create(path, fmt, 2, sr).map_err(|e| e.to_string())?;
            for (a, b) in l.chunks(1 << 16).zip(r.chunks(1 << 16)) {
                w.write(a, b).map_err(|e| e.to_string())?;
            }
            w.finish().map_err(|e| e.to_string())
        }
        match kind {
            Export::Mix(range) => {
                let (a, b) = range.unwrap_or((0, p.end_frame()));
                self.spawn_job("daw-export", move |jobs| {
                    let (l, r) = p.render(a, b, None);
                    let m = match write(&path, &l, &r, sr) {
                        Ok(_) => format!("Exported {}", path.display()),
                        Err(e) => format!("Export failed: {e}"),
                    };
                    jobs.lock().push(Job::Done(m));
                });
            }
            Export::Clip(id) => {
                let Some((ti, ci)) = p.find_clip(id) else { return };
                let c = p.tracks[ti].clips[ci].clone();
                self.spawn_job("daw-export", move |jobs| {
                    // the clip alone, with its own gain and fades but without the track's mixer settings
                    let mut q = Project::new(sr);
                    let t = q.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
                    q.tracks[t].clips.push(c.clone());
                    let (l, r) = q.render(c.start, c.end(), None);
                    let m = match write(&path, &l, &r, sr) {
                        Ok(_) => format!("Exported {}", path.display()),
                        Err(e) => format!("Export failed: {e}"),
                    };
                    jobs.lock().push(Job::Done(m));
                });
            }
            Export::StemsDir => {
                self.spawn_job("daw-export", move |jobs| {
                    let end = p.end_frame();
                    let mut n = 0;
                    for (i, t) in p.tracks.iter().enumerate() {
                        if t.clips.is_empty() {
                            continue;
                        }
                        let (l, r) = p.render(0, end, Some(t.id));
                        let name: String = t.name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
                        if write(&path.join(format!("{:02}_{name}.wav", i + 1)), &l, &r, sr).is_ok() {
                            n += 1;
                        }
                    }
                    jobs.lock().push(Job::Done(format!("Exported {n} track files to {}", path.display())));
                });
            }
        }
    }

    // ------------------------------------------------------------------ demo content

    /// A handful of synthetic tracks (tone, sweep, noise, clicks) to try the editor and the analysis.
    pub(super) fn demo(&mut self) {
        let sr = self.sr();
        let n = sr as usize * 8;
        let tone: Vec<f32> = (0..n).map(|i| 0.4 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin() * (0.6 + 0.4 * (i as f32 / sr as f32 * 1.5).sin())).collect();
        let (f0, f1) = (30.0f64, 18000.0f64);
        let k = (f1 / f0).ln();
        let secs = n as f64 / sr as f64;
        let sweep: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (0.35 * (2.0 * std::f64::consts::PI * f0 * secs / k * ((t / secs * k).exp() - 1.0)).sin()) as f32
            })
            .collect();
        let mut seed = 12345u32;
        let mut lp = 0f32;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let w = (seed >> 8) as f32 / 8388608.0 - 1.0;
                lp += 0.08 * (w - lp);
                lp * 1.4
            })
            .collect();
        let clicks: Vec<f32> = (0..n)
            .map(|i| {
                let beat = sr as usize / 2;
                let ph = i % beat;
                if ph < 2000 { (1.0 - ph as f32 / 2000.0).powi(3) * 0.8 * (2.0 * std::f32::consts::PI * 90.0 * ph as f32 / sr as f32).sin() } else { 0.0 }
            })
            .collect();
        let mut made: Vec<(String, Vec<Vec<f32>>)> = vec![
            ("Tone 440 Hz".into(), vec![tone.clone(), tone.iter().map(|v| v * 0.7).collect()]),
            ("Sweep 30 Hz - 18 kHz".into(), vec![sweep]),
            ("Noise".into(), vec![noise.clone(), noise.iter().rev().copied().collect()]),
            ("Kick".into(), vec![clicks]),
        ];
        let ids: Vec<u32> = made.iter().map(|_| self.new_src_id()).collect();
        self.with_proj(|p| {
            for ((name, chans), id) in made.drain(..).zip(ids) {
                let ti = p.add_track(Some(name.clone()), Input { dev: String::new(), mode: Mode::Mix });
                p.add_clip(ti, Arc::new(Source::new(id, name, chans)), 0);
            }
            // the first stereo track shows its channels as sub-tracks
            p.tracks[0].h = 2;
            p.tracks[1].pan = -0.4;
            p.tracks[2].vol_db = -8.0;
            p.tracks[3].pan = 0.3;
        });
        self.fit_pending = true;
    }
}
