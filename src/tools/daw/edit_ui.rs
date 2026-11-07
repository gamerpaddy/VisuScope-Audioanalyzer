//! The windows and jobs for the offline edits of the timeline: the spectral editor, click / hum removal, and time-stretch / pitch-shift.

use super::edit::{self, Region};
use super::*;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke};

/// What an edit does to the audio of a clip.
#[derive(Clone)]
pub(super) enum EditKind {
    Regions(Vec<Region>),
    Declick(f32),
    Dehum(f32),
    /// the tempo changes (1.25 = a quarter faster), the pitch stays
    Tempo(f64),
    /// the pitch changes (semitones), the duration stays
    Pitch(f32),
    /// tempo and pitch together, like a tape
    Speed(f64),
    /// tempo and pitch separately: (tempo, pitch factor)
    TempoPitch(f64, f64),
}

/// The spectral editor: the spectrogram of one clip, the regions drawn on it.
pub(super) struct SpecEd {
    clip: u32,
    /// the source the picture was made from (the picture is made again when the clip's audio changes)
    src_id: u32,
    tex: Option<egui::TextureHandle>,
    regions: Vec<Region>,
    sel: Option<usize>,
    /// level change of a new region (dB; -80 removes)
    gain_db: f32,
    /// the corner where the drag started (screen position)
    drag: Option<egui::Pos2>,
    len: usize,
    sr: u32,
}

pub(super) struct StretchDlg {
    pub tempo_pct: f32,
    pub semis: f32,
    /// 0: tempo and pitch separately, 1: speed (both together)
    pub mode: usize,
}

const F_LO: f32 = 20.0;
const RANGE_DB: f32 = 100.0;

/// The mono mix of what a clip plays, in timeline order.
fn clip_mono(c: &Clip) -> Vec<f32> {
    (0..c.len)
        .map(|t| {
            let i = c.src_index(t);
            if c.src.stereo() { 0.5 * (c.src.at(0, i) + c.src.at(1, i)) } else { c.src.at(0, i) }
        })
        .collect()
}

impl Daw {
    /// Process the audio of the clips with `kind` on a worker; the result replaces the audio of the clips (one undo step).
    pub(super) fn act_process(&mut self, p: &Project, ids: Vec<u32>, kind: EditKind) {
        let items: Vec<(u32, Arc<Source>, usize, usize, bool)> = ids.iter().filter_map(|id| p.find_clip(*id).map(|(ti, ci)| p.tracks[ti].clips[ci].clone())).map(|c| (c.id, c.src.clone(), c.off, c.len, c.reverse)).collect();
        if items.is_empty() {
            self.note = "Select a clip first".into();
            return;
        }
        let sr = p.sr;
        let src_ids = self.src_ids.clone();
        self.spawn_job("daw-edit", move |jobs| {
            let mut out = Vec::new();
            let mut clicks = 0;
            for (id, src, off, len, rev) in items {
                let chans: Vec<Vec<f32>> = (0..src.ch.len())
                    .map(|ch| (0..len).map(|t| src.at(ch, if rev { off + len - 1 - t } else { off + t })).collect())
                    .collect();
                let mut scale = 1.0;
                let done: Vec<Vec<f32>> = chans
                    .iter()
                    .map(|x| match &kind {
                        EditKind::Regions(r) => edit::apply_regions(x, sr, r),
                        EditKind::Declick(s) => {
                            let (y, n) = edit::declick(x, *s);
                            clicks += n;
                            y
                        }
                        EditKind::Dehum(f0) => edit::dehum(x, sr, *f0, 12, 35.0),
                        EditKind::Tempo(t) => {
                            scale = 1.0 / t;
                            edit::stretch(x, 1.0 / t)
                        }
                        EditKind::Pitch(s) => edit::pitch_shift(x, *s),
                        EditKind::TempoPitch(t, f) => {
                            scale = 1.0 / t;
                            let stretched = edit::stretch(x, f / t);
                            edit::resample_ratio(&stretched, *f, ((x.len() as f64 / t).round() as usize).max(1))
                        }
                        EditKind::Speed(t) => {
                            scale = 1.0 / t;
                            edit::resample_ratio(x, *t, ((x.len() as f64 / t).round() as usize).max(1))
                        }
                    })
                    .collect();
                let name = format!("{} (edited)", src.name);
                let new = Source::new(file::next_src_id(&src_ids), name, done);
                let n = new.len();
                out.push((id, Arc::new(new), n, scale));
            }
            let note = match &kind {
                EditKind::Declick(_) => format!("Removed {clicks} clicks"),
                EditKind::Regions(r) => format!("Applied {} region(s)", r.len()),
                EditKind::Dehum(_) => "Removed the hum".to_string(),
                EditKind::Tempo(_) | EditKind::Pitch(_) | EditKind::Speed(_) | EditKind::TempoPitch(..) => "Stretched".to_string(),
            };
            jobs.lock().push(Job::Edited(out, note));
        });
    }

    /// The finished edit: the clips take the new audio (the fades keep their proportion).
    pub(super) fn finish_edit(&mut self, items: Vec<(u32, Arc<Source>, usize, f64)>, note: String) {
        self.with_proj(|p| {
            for (id, src, len, scale) in items {
                if let Some(c) = p.clip_mut(id) {
                    c.src = src;
                    c.off = 0;
                    c.reverse = false;
                    c.len = len.max(1);
                    c.fade_in = (c.fade_in as f64 * scale) as usize;
                    c.fade_out = (c.fade_out as f64 * scale) as usize;
                    c.clamp_fades();
                }
            }
        });
        self.note = note;
    }

    /// Open the spectral editor on a clip.
    pub(super) fn open_spec(&mut self, p: &Project, id: u32) {
        let Some((ti, ci)) = p.find_clip(id) else { return };
        let c = &p.tracks[ti].clips[ci];
        self.spec = Some(SpecEd { clip: id, src_id: 0, tex: None, regions: Vec::new(), sel: None, gain_db: -80.0, drag: None, len: c.len, sr: p.sr });
    }

    /// The editor windows (spectral editor, stretch dialog).
    pub(super) fn edit_windows(&mut self, ui: &mut Ui, p: &Project) {
        // test hooks (screenshots): VISUSCOPE_SPEC=1 opens the spectral editor on the first clip, VISUSCOPE_STRETCH=1 the stretch dialog
        static HOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !HOOK.swap(true, Relaxed) {
            if let Some(c) = p.tracks.iter().flat_map(|t| t.clips.iter()).next() {
                if std::env::var_os("VISUSCOPE_SPEC").is_some() {
                    self.open_spec(p, c.id);
                    if let Some(sp) = self.spec.as_mut() {
                        let n = sp.len;
                        sp.regions.push(Region { t0: n / 5, t1: n / 2, f0: 800.0, f1: 3000.0, gain_db: -80.0 });
                        sp.regions.push(Region { t0: n / 2, t1: n * 4 / 5, f0: 100.0, f1: 600.0, gain_db: -12.0 });
                    }
                }
                if std::env::var_os("VISUSCOPE_EDIT_TEST").is_some() {
                    // runs an edit for real (the first clip played at half the tempo, with a pitch shift)
                    self.act_process(p, vec![c.id], EditKind::TempoPitch(0.5, 2f64.powf(5.0 / 12.0)));
                }
                if std::env::var_os("VISUSCOPE_STRETCH").is_some() {
                    self.sel.insert(c.id);
                    self.stretch = Some(StretchDlg { tempo_pct: 125.0, semis: 3.0, mode: 0 });
                }
            }
        }
        self.stretch_window(ui, p);
        self.spec_window(ui, p);
    }

    fn stretch_window(&mut self, ui: &mut Ui, p: &Project) {
        let Some(mut d) = self.stretch.take() else { return };
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Time-stretch / pitch-shift").order(egui::Order::Foreground).default_pos(pos2(80.0, 120.0)).open(&mut open).collapsible(false).resizable(false).default_width(330.0).show(ui.ctx(), |ui| {
            ui.label(RichText::new(format!("{} clip(s) selected. The audio of the clips is processed (Ctrl+Z undoes it).", self.sel.len())).small().weak());
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.mode, 0, "Tempo and pitch separately");
                ui.radio_value(&mut d.mode, 1, "Speed (both, like tape)");
            });
            let r = ui.add(egui::Slider::new(&mut d.tempo_pct, 25.0..=400.0).logarithmic(true).suffix(" %").text(if d.mode == 0 { "Tempo" } else { "Speed" }));
            crate::slider::reset(ui, &r, &mut d.tempo_pct, 100.0);
            if d.mode == 0 {
                ui.add(egui::Slider::new(&mut d.semis, -24.0..=24.0).suffix(" semitones").text("Pitch"));
            } else {
                ui.label(RichText::new(format!("The pitch changes by {:+.2} semitones", 12.0 * (d.tempo_pct as f64 / 100.0).log2())).small().weak());
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.sel.is_empty(), egui::Button::new("Apply")).clicked() {
                    apply = true;
                }
                if ui.button("Reset").clicked() {
                    d.tempo_pct = 100.0;
                    d.semis = 0.0;
                }
            });
        });
        if apply {
            let ids: Vec<u32> = self.sel.iter().copied().collect();
            if d.mode == 1 {
                self.act_process(p, ids, EditKind::Speed(d.tempo_pct as f64 / 100.0));
            } else {
                // tempo first (changes the length), then the pitch on the result: two jobs in a row would race, so do both in one
                self.act_process_chain(p, ids, d.tempo_pct as f64 / 100.0, d.semis);
            }
        }
        if open {
            self.stretch = Some(d);
        }
    }

    /// Tempo and pitch in one go.
    fn act_process_chain(&mut self, p: &Project, ids: Vec<u32>, tempo: f64, semis: f32) {
        if (tempo - 1.0).abs() < 1e-4 {
            self.act_process(p, ids, EditKind::Pitch(semis));
        } else if semis.abs() < 1e-3 {
            self.act_process(p, ids, EditKind::Tempo(tempo));
        } else {
            // the pitch shift is a tempo change followed by a speed change: combine into one stretch and one resample
            let f = 2f64.powf(semis as f64 / 12.0);
            // stretch by (1/tempo) * f (pitch stays), then read f times as fast (pitch * f, length back by 1/f)
            self.act_process(p, ids, EditKind::TempoPitch(tempo, f));
        }
    }

    fn spec_window(&mut self, ui: &mut Ui, p: &Project) {
        let Some(mut s) = self.spec.take() else { return };
        let Some((ti, ci)) = p.find_clip(s.clip) else { return };
        let clip = p.tracks[ti].clips[ci].clone();
        // (re)make the picture when the audio is new
        if s.src_id != clip.src.id || s.len != clip.len || s.tex.is_none() {
            let (w, h) = (1400usize, 360usize);
            let mono = clip_mono(&clip);
            let img = edit::spectrogram(&mono, p.sr, w, h, F_LO, RANGE_DB);
            let lut = crate::colormap::Cmap::Inferno.lut();
            let pixels: Vec<Color32> = img.iter().map(|v| lut[*v as usize]).collect();
            let ci = egui::ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels };
            match &mut s.tex {
                Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
                None => s.tex = Some(ui.ctx().load_texture("daw_spec_edit", ci, egui::TextureOptions::LINEAR)),
            }
            if s.src_id != 0 && s.src_id != clip.src.id {
                s.regions.clear();
                s.sel = None;
            }
            s.src_id = clip.src.id;
            s.len = clip.len;
        }
        let mut open = true;
        let mut run: Option<EditKind> = None;
        let nyq = p.sr as f32 * 0.5;
        egui::Window::new(format!("Spectral editor: {}", clip.name)).id(egui::Id::new("daw_spec_win")).order(egui::Order::Foreground).open(&mut open).default_size([920.0, 520.0]).resizable(true).show(ui.ctx(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Drag on the picture to mark a region.").small().weak());
                ui.add(egui::Slider::new(&mut s.gain_db, -80.0..=12.0).text("Level of new regions").custom_formatter(|v, _| if v <= -79.9 { "removed".to_string() } else { format!("{v:+.0} dB") }));
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!s.regions.is_empty(), egui::Button::new(format!("Apply {} region(s)", s.regions.len()))).on_hover_text("Process the clip's audio (Ctrl+Z undoes it)").clicked() {
                    run = Some(EditKind::Regions(s.regions.clone()));
                }
                if ui.add_enabled(s.sel.is_some(), egui::Button::new("Delete region")).clicked() {
                    if let Some(i) = s.sel.take() {
                        if i < s.regions.len() {
                            s.regions.remove(i);
                        }
                    }
                }
                if ui.add_enabled(!s.regions.is_empty(), egui::Button::new("Clear regions")).clicked() {
                    s.regions.clear();
                    s.sel = None;
                }
                ui.separator();
                if ui.button("Remove clicks").on_hover_text("Finds short bursts far above the signal's own roughness and bridges them").clicked() {
                    run = Some(EditKind::Declick(12.0));
                }
                if ui.button("Remove 50 Hz hum").clicked() {
                    run = Some(EditKind::Dehum(50.0));
                }
                if ui.button("Remove 60 Hz hum").clicked() {
                    run = Some(EditKind::Dehum(60.0));
                }
            });
            let size = vec2(ui.available_width(), (ui.available_height() - 4.0).max(120.0));
            let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
            let plot = Rect::from_min_max(resp.rect.min + vec2(40.0, 2.0), resp.rect.max - vec2(2.0, 16.0));
            painter.rect_filled(resp.rect, 0.0, Color32::from_rgb(10, 12, 16));
            if let Some(t) = &s.tex {
                painter.image(t.id(), plot, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            }
            // axes: time (seconds in the clip) and frequency (log)
            let secs = s.len as f32 / p.sr as f32;
            let step = [0.1f32, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|st| plot.width() / (secs / st) >= 60.0).unwrap_or(600.0);
            let mut t = 0.0;
            while t <= secs {
                let x = plot.left() + t / secs * plot.width();
                painter.line_segment([pos2(x, plot.bottom()), pos2(x, plot.bottom() + 3.0)], Stroke::new(1.0, Color32::GRAY));
                painter.text(pos2(x + 2.0, plot.bottom() + 2.0), Align2::LEFT_TOP, format!("{t:.1} s"), FontId::proportional(9.0), Color32::from_gray(150));
                t += step;
            }
            let y_of = |f: f32| plot.bottom() - ((f.max(F_LO) / F_LO).ln() / (nyq / F_LO).ln()).clamp(0.0, 1.0) * plot.height();
            let f_of = |y: f32| F_LO * (nyq / F_LO).powf(((plot.bottom() - y) / plot.height()).clamp(0.0, 1.0));
            let t_of = |x: f32| (((x - plot.left()) / plot.width()).clamp(0.0, 1.0) * s.len as f32) as usize;
            for f in [50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
                if f < nyq {
                    let y = y_of(f);
                    painter.line_segment([pos2(plot.left() - 3.0, y), pos2(plot.left(), y)], Stroke::new(1.0, Color32::GRAY));
                    painter.text(pos2(plot.left() - 5.0, y), Align2::RIGHT_CENTER, if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") }, FontId::proportional(9.0), Color32::from_gray(150));
                }
            }
            // the regions
            for (i, r) in s.regions.iter().enumerate() {
                let (x0, x1) = (plot.left() + r.t0 as f32 / s.len as f32 * plot.width(), plot.left() + r.t1 as f32 / s.len as f32 * plot.width());
                let rr = Rect::from_min_max(pos2(x0, y_of(r.f1)), pos2(x1, y_of(r.f0)));
                let sel = s.sel == Some(i);
                painter.rect_filled(rr, 0.0, if r.gain_db <= -79.9 { Color32::from_rgba_unmultiplied(255, 60, 60, 50) } else { Color32::from_rgba_unmultiplied(80, 160, 255, 50) });
                painter.rect_stroke(rr, 0.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { Color32::WHITE } else { Color32::from_rgb(255, 200, 80) }), egui::StrokeKind::Inside);
                painter.text(rr.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, if r.gain_db <= -79.9 { "removed".to_string() } else { format!("{:+.0} dB", r.gain_db) }, FontId::proportional(10.0), Color32::WHITE);
            }
            // drawing a region
            if resp.drag_started_by(egui::PointerButton::Primary) {
                s.drag = resp.interact_pointer_pos();
            }
            if let (Some(a), Some(b)) = (s.drag, resp.interact_pointer_pos()) {
                painter.rect_stroke(Rect::from_two_pos(a, b), 0.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
            }
            if resp.drag_stopped() {
                if let (Some(a), Some(b)) = (s.drag.take(), resp.interact_pointer_pos()) {
                    if (a.x - b.x).abs() > 3.0 && (a.y - b.y).abs() > 3.0 {
                        let (t0, t1) = (t_of(a.x.min(b.x)), t_of(a.x.max(b.x)));
                        let (f0, f1) = (f_of(a.y.max(b.y)), f_of(a.y.min(b.y)));
                        s.regions.push(Region { t0, t1, f0, f1, gain_db: s.gain_db });
                        s.sel = Some(s.regions.len() - 1);
                    }
                }
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let (tt, ff) = (t_of(pos.x), f_of(pos.y));
                    s.sel = s.regions.iter().position(|r| tt >= r.t0 && tt <= r.t1 && ff >= r.f0 && ff <= r.f1);
                }
            }
            if resp.hovered() {
                if let Some(pos) = resp.hover_pos().filter(|p| plot.contains(*p)) {
                    resp.on_hover_text(format!("{:.2} s, {:.0} Hz", t_of(pos.x) as f32 / p.sr as f32, f_of(pos.y)));
                }
            }
        });
        if let Some(kind) = run {
            self.act_process(p, vec![s.clip], kind);
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete)) && s.sel.is_some() && open {
            if let Some(i) = s.sel.take() {
                if i < s.regions.len() {
                    s.regions.remove(i);
                }
            }
        }
        if open {
            self.spec = Some(s);
        }
    }
}
