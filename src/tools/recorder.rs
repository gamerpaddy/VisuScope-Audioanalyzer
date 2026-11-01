//! Recorder (input -> WAV, with pre-roll) and file player (most audio formats -> analysers).

use super::dialogs::{self, Pending};
use super::plot::{canvas, heading};
use super::monitor::{self, Queue};
use super::wavio::{Format, WavData, WavWriter};
use super::*;
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

struct RecShared {
    stop: AtomicBool,
    frames: AtomicU64,
    bytes: AtomicU64,
    lost: AtomicU64,
    done: AtomicBool,
    err: Mutex<String>,
}

struct RecJob {
    sh: Arc<RecShared>,
    path: std::path::PathBuf,
    sr: u32,
}

struct PlayShared {
    pos: AtomicU64,
    playing: AtomicBool,
    stop: AtomicBool,
    looping: AtomicBool,
    seek: AtomicI64,
    /// also send the samples to the sound output
    monitor: AtomicBool,
}

struct Loaded {
    name: String,
    path: std::path::PathBuf,
    data: Arc<WavData>,
    /// min / max per overview block
    overview: Vec<(f32, f32)>,
}

const OV_BLOCK: usize = 1024;

pub struct Recorder {
    fmt: Format,
    pre_roll: f32,
    rec: Option<RecJob>,
    last_file: Option<std::path::PathBuf>,
    last_err: String,
    open_dlg: Option<Pending>,
    load_slot: Arc<Mutex<Option<Result<Loaded, String>>>>,
    loading: bool,
    file: Option<Arc<Loaded>>,
    play: Option<Arc<PlayShared>>,
    looping: bool,
    play_err: String,
    mon_on: bool,
    mon_dev: String,
    mon_vol: f32,
    mon: Option<(monitor::Handle, String)>,
    mon_q: Arc<Mutex<Queue>>,
    mon_gain: Arc<AtomicU32>,
    mon_status: Arc<Mutex<String>>,
    /// visible part of the waveform as (start, span) fractions of the file
    view: (f64, f64),
    follow: bool,
    test_stage: u8,
}

fn rec_thread(sh: Arc<Shared>, job: Arc<RecShared>, path: std::path::PathBuf, fmt: Format, sr: u32, pre_roll: f32) {
    let mut w = match WavWriter::create(&path, fmt, 2, sr) {
        Ok(w) => w,
        Err(e) => {
            *job.err.lock() = format!("cannot create file: {e}");
            job.done.store(true, Relaxed);
            return;
        }
    };
    let (mut l, mut r) = (vec![0f32; 1 << 16], vec![0f32; 1 << 16]);
    let mut pos = {
        let ring = sh.ring.lock();
        ring.total.saturating_sub((pre_roll * sr as f32) as u64).min(ring.total).max(ring.total.saturating_sub((RING_CAP - 8192) as u64))
    };
    while !job.stop.load(Relaxed) || {
        // drain what is left after a stop request
        let t = sh.ring.lock().total;
        pos < t
    } {
        let ring = sh.ring.lock();
        if ring.total < pos {
            // capture restarted: continue with the new stream
            pos = 0;
        }
        let behind = ring.total - pos;
        if behind > (RING_CAP - 8192) as u64 {
            job.lost.fetch_add(behind - 4096, Relaxed);
            pos = ring.total - 4096;
        }
        let n = ((ring.total - pos) as usize).min(l.len());
        if n > 0 {
            ring.copy_abs(pos as usize, &mut l[..n], &mut r[..n]);
        }
        drop(ring);
        if n > 0 {
            pos += n as u64;
            if let Err(e) = w.write(&l[..n], &r[..n]) {
                *job.err.lock() = format!("write failed: {e}");
                break;
            }
            job.frames.store(w.frames, Relaxed);
            job.bytes.store(w.bytes_written(), Relaxed);
        } else {
            if job.stop.load(Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    if let Err(e) = w.finish() {
        *job.err.lock() = format!("finalising failed: {e}");
    }
    job.done.store(true, Relaxed);
}

fn play_thread(sh: Arc<Shared>, ctl: Arc<PlayShared>, file: Arc<Loaded>, mon: Arc<Mutex<Queue>>) {
    // make sure the live capture is gone before the file takes over the ring
    sh.audio_gen.fetch_add(1, Relaxed);
    std::thread::sleep(Duration::from_millis(150));
    let d = &file.data;
    let total = d.l.len();
    {
        let mut ring = sh.ring.lock();
        ring.total = 0;
        ring.l.fill(0.0);
        ring.r.fill(0.0);
    }
    sh.sr.store(d.sr, Relaxed);
    sh.chans.store(2, Relaxed);
    sh.feeding_silence.store(false, Relaxed);
    *sh.status.lock() = format!("File: {} ({} Hz)", file.name, d.sr);
    {
        let mut q = mon.lock();
        q.buf.clear();
        q.src_sr = d.sr;
    }
    // `audible`: also feed the sound output (not for the silent preview after a seek)
    let push = |a: usize, b: usize, audible: bool| {
        let mut ring = sh.ring.lock();
        for i in a..b.min(total) {
            ring.push(d.l[i], d.r[i]);
        }
        drop(ring);
        if audible && ctl.monitor.load(Relaxed) && b > a {
            mon.lock().push(&d.l[a..b.min(total)], &d.r[a..b.min(total)]);
        }
        sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
        sh.cv.notify_one();
    };
    let mut pos = ctl.pos.load(Relaxed) as usize;
    let mut carry = 0f64;
    let mut last = Instant::now();
    while !ctl.stop.load(Relaxed) {
        std::thread::sleep(Duration::from_millis(4));
        // the timeline took over the analysers: step aside
        if sh.ring_owner.load(Relaxed) != 1 {
            ctl.playing.store(false, Relaxed);
            ctl.stop.store(true, Relaxed);
            break;
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.1);
        last = now;
        let seek = ctl.seek.swap(-1, Relaxed);
        if seek >= 0 {
            pos = (seek as usize).min(total.saturating_sub(1));
            ctl.pos.store(pos as u64, Relaxed);
            mon.lock().buf.clear();
            if !ctl.playing.load(Relaxed) {
                // paused: show what is under the playhead
                push(pos.saturating_sub(1 << 16), pos, false);
            }
        }
        if !ctl.playing.load(Relaxed) {
            continue;
        }
        carry += dt * d.sr as f64;
        let n = carry.floor() as usize;
        carry -= n as f64;
        let mut left = n;
        while left > 0 {
            let take = left.min(total - pos);
            push(pos, pos + take, true);
            pos += take;
            left -= take;
            if pos >= total {
                if ctl.looping.load(Relaxed) {
                    pos = 0;
                } else {
                    ctl.playing.store(false, Relaxed);
                    break;
                }
            }
        }
        ctl.pos.store(pos as u64, Relaxed);
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            fmt: Format::Pcm24,
            pre_roll: 0.0,
            rec: None,
            last_file: None,
            last_err: String::new(),
            open_dlg: None,
            load_slot: Arc::new(Mutex::new(None)),
            loading: false,
            file: None,
            play: None,
            looping: false,
            play_err: String::new(),
            mon_on: true,
            mon_dev: String::new(),
            mon_vol: 0.7,
            mon: None,
            mon_q: Arc::new(Mutex::new(Queue::default())),
            mon_gain: Arc::new(AtomicU32::new(0.49f32.to_bits())),
            mon_status: Arc::new(Mutex::new(String::new())),
            view: (0.0, 1.0),
            follow: true,
            test_stage: 0,
        }
    }

    fn start_rec(&mut self, cx: &Ctx) {
        let sr = cx.sh.sr.load(Relaxed);
        if sr == 0 {
            self.last_err = "no input running".into();
            return;
        }
        let (stamp, _) = utc_stamp(now_unix());
        let path = docs_dir().join(format!("rec_{stamp}.wav"));
        let job = Arc::new(RecShared { stop: AtomicBool::new(false), frames: AtomicU64::new(0), bytes: AtomicU64::new(44), lost: AtomicU64::new(0), done: AtomicBool::new(false), err: Mutex::new(String::new()) });
        let (sh, j2, p2, fmt, pre) = (cx.sh.clone(), job.clone(), path.clone(), self.fmt, self.pre_roll);
        std::thread::Builder::new().name("recorder".into()).spawn(move || rec_thread(sh, j2, p2, fmt, sr, pre)).ok();
        self.rec = Some(RecJob { sh: job, path, sr });
        self.last_err.clear();
    }

    /// Keep the sound output stream in line with the player state and the chosen device.
    fn manage_monitor(&mut self, cx: &Ctx) {
        let dev = if self.mon_dev.is_empty() || !cx.out_devs.iter().any(|d| d.id == self.mon_dev) { cx.out_devs.first().map(|d| d.id.clone()).unwrap_or_default() } else { self.mon_dev.clone() };
        let want = self.mon_on && self.play.as_ref().map(|p| !p.stop.load(Relaxed)).unwrap_or(false);
        if let Some(p) = &self.play {
            p.monitor.store(self.mon_on, Relaxed);
        }
        match (&self.mon, want) {
            (Some((_, d)), true) if *d == dev => {}
            (_, true) => {
                self.mon_q.lock().buf.clear();
                let h = monitor::start(self.mon_q.clone(), self.mon_gain.clone(), Some(dev.clone()), self.mon_status.clone());
                self.mon = Some((h, dev));
            }
            (Some(_), false) => self.mon = None,
            _ => {}
        }
    }

    fn stop_rec(&mut self) {
        if let Some(j) = &self.rec {
            j.sh.stop.store(true, Relaxed);
        }
    }

    fn load_file(&mut self, path: std::path::PathBuf) {
        self.loading = true;
        let slot = self.load_slot.clone();
        std::thread::Builder::new()
            .name("wav-load".into())
            .spawn(move || {
                let r = decode::read(&path).map(|d| {
                    let overview = d.l.chunks(OV_BLOCK).zip(d.r.chunks(OV_BLOCK)).map(|(a, b)| a.iter().chain(b).fold((0f32, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))).collect();
                    Loaded { name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), path: path.clone(), data: Arc::new(d), overview }
                });
                *slot.lock() = Some(r);
            })
            .ok();
    }

    fn start_play(&mut self, cx: &mut Ctx) {
        let Some(file) = self.file.clone() else { return };
        self.stop_play_thread();
        cx.sh.ring_owner.store(1, Relaxed);
        let ctl = Arc::new(PlayShared { pos: AtomicU64::new(0), playing: AtomicBool::new(true), stop: AtomicBool::new(false), looping: AtomicBool::new(self.looping), seek: AtomicI64::new(-1), monitor: AtomicBool::new(self.mon_on) });
        let (sh, c2) = (cx.sh.clone(), ctl.clone());
        let q = self.mon_q.clone();
        std::thread::Builder::new().name("file-player".into()).spawn(move || play_thread(sh, c2, file, q)).ok();
        self.play = Some(ctl);
        cx.req.audio = AudioReq::Stop;
    }

    fn stop_play_thread(&mut self) {
        if let Some(p) = self.play.take() {
            p.stop.store(true, Relaxed);
        }
    }

    fn rec_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        heading(ui, "Record");
        let recording = self.rec.is_some();
        ui.horizontal(|ui| {
            let (txt, col) = if recording { ("Stop", Color32::from_rgb(170, 50, 50)) } else { ("Record", Color32::from_rgb(60, 60, 66)) };
            let b = egui::Button::new(RichText::new(if recording { "Stop" } else { "Record" }).strong().color(if recording { Color32::WHITE } else { Color32::from_rgb(255, 110, 110) })).fill(col).min_size(vec2(96.0, 26.0));
            let _ = txt;
            if ui.add(b).clicked() {
                if recording {
                    self.stop_rec();
                } else {
                    self.start_rec(cx);
                }
            }
            ui.add_enabled_ui(!recording, |ui| {
                egui::ComboBox::from_id_salt("rec_fmt").selected_text(self.fmt.name()).width(110.0).show_ui(ui, |ui| {
                    for f in Format::ALL {
                        ui.selectable_value(&mut self.fmt, f, f.name());
                    }
                });
            });
        });
        ui.add_enabled(!recording, egui::Slider::new(&mut self.pre_roll, 0.0..=15.0).text("pre-roll s").fixed_decimals(1))
            .on_hover_text("Include this many seconds from before the button press (taken from the live buffer)");
        if let Some(j) = &self.rec {
            let fr = j.sh.frames.load(Relaxed);
            let mb = j.sh.bytes.load(Relaxed) as f32 / 1048576.0;
            ui.label(RichText::new(format!("{}   {:.1} MB   {}", fmt_time(fr as f32 / j.sr as f32), mb, j.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())).monospace());
            let lost = j.sh.lost.load(Relaxed);
            if lost > 0 {
                ui.colored_label(Color32::from_rgb(255, 170, 70), format!("{lost} frames lost (disk too slow)"));
            }
        }
        for k in 0..2 {
            let db = cx.an.peak_db[k];
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(380.0), 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, Color32::from_gray(28));
            let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let col = if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) };
            ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * t, rect.height())), 2.0, col);
        }
        if let Some(p) = &self.last_file {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Saved: {}", p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())).small().weak());
                if ui.small_button("Show folder").clicked() {
                    let _ = std::process::Command::new("explorer").arg(docs_dir()).spawn();
                }
                if ui.small_button("Open in Timeline").on_hover_text("Edit and analyse this recording in the multitrack timeline").clicked() {
                    cx.req.send_to_daw.push(p.clone());
                }
            });
        }
        if !self.last_err.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 120, 100), &self.last_err);
        }
    }

    fn play_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        heading(ui, "Play file");
        ui.horizontal(|ui| {
            if ui.add_enabled(self.open_dlg.is_none() && !self.loading, egui::Button::new("Open audio file...")).clicked() {
                self.open_dlg = Some(dialogs::open("Open audio file", "Audio files", decode::EXTENSIONS));
            }
            if self.loading {
                ui.spinner();
            }
            if self.play.is_some() && ui.button("Back to live input").on_hover_text("Stop the file and restart the capture device").clicked() {
                self.stop_play_thread();
                if cx.sh.ring_owner.compare_exchange(1, 0, Relaxed, Relaxed).is_ok() {
                    cx.req.audio = AudioReq::Restart;
                }
            }
        });
        if !self.play_err.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 120, 100), &self.play_err);
        }
        let Some(file) = self.file.clone() else {
            ui.label(RichText::new("Load an audio file (WAV, FLAC, MP3, Ogg Vorbis, AAC / M4A, ALAC, MKV) to analyse it with every view (spectrum, scope, measurements ...).").small().weak());
            return;
        };
        let total = file.data.l.len();
        let sr = file.data.sr as f32;
        let pos = self.play.as_ref().map(|p| p.pos.load(Relaxed) as usize).unwrap_or(0);
        let playing = self.play.as_ref().map(|p| p.playing.load(Relaxed)).unwrap_or(false);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  -  {} Hz, {} ch, {}", file.name, file.data.sr, file.data.src_channels, fmt_time(total as f32 / sr))).small());
            if ui.small_button("Open in Timeline").on_hover_text("Edit and analyse this file in the multitrack timeline").clicked() {
                cx.req.send_to_daw.push(file.path.clone());
            }
        });
        ui.horizontal(|ui| {
            let icon = if playing { "Pause" } else { "Play" };
            if ui.add(egui::Button::new(RichText::new(icon).strong()).min_size(vec2(84.0, 24.0))).clicked() {
                match &self.play {
                    Some(p) => {
                        let now = p.playing.load(Relaxed);
                        if !now && pos + 1 >= total {
                            p.seek.store(0, Relaxed);
                        }
                        p.playing.store(!now, Relaxed);
                    }
                    None => self.start_play(cx),
                }
            }
            if ui.button("|<").on_hover_text("To start").clicked() {
                if let Some(p) = &self.play {
                    p.seek.store(0, Relaxed);
                }
            }
            if ui.checkbox(&mut self.looping, "Loop").changed() {
                if let Some(p) = &self.play {
                    p.looping.store(self.looping, Relaxed);
                }
            }
            ui.label(RichText::new(format!("{} / {}", fmt_time(pos as f32 / sr), fmt_time(total as f32 / sr))).monospace());
        });
        // sound output
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.mon_on, "Sound").on_hover_text("Play the file through an output device while it is analysed");
            ui.add_enabled_ui(self.mon_on, |ui| {
                let cur = cx.out_devs.iter().find(|d| d.id == self.mon_dev).or(cx.out_devs.first()).map(|d| d.label.clone()).unwrap_or_else(|| "no output".into());
                egui::ComboBox::from_id_salt("mon_dev").width(190.0).selected_text(cur).show_ui(ui, |ui| {
                    for d in cx.out_devs {
                        ui.selectable_value(&mut self.mon_dev, d.id.clone(), &d.label);
                    }
                });
                ui.add(egui::Slider::new(&mut self.mon_vol, 0.0..=1.0).text("vol").show_value(false));
            });
            let st = self.mon_status.lock().clone();
            if self.mon_on && self.mon.is_some() && !st.is_empty() {
                ui.label(RichText::new(st).small().weak());
            }
        });
        self.mon_gain.store(self.mon_vol.powi(2).to_bits(), Relaxed);
        // waveform with zoom (wheel), pan (drag when zoomed) and seek (click)
        ui.horizontal(|ui| {
            if ui.small_button("Fit").on_hover_text("Show the whole file (double-click the waveform too)").clicked() {
                self.view = (0.0, 1.0);
            }
            ui.checkbox(&mut self.follow, "Follow playhead");
            let (a, b) = self.view;
            ui.label(RichText::new(format!("{} - {}", fmt_time((a * total as f64) as f32 / sr), fmt_time(((a + b) * total as f64) as f32 / sr))).small().weak());
        });
        let (resp, painter, rect) = canvas(ui, 90.0);
        let r = rect.shrink2(vec2(2.0, 3.0));
        let r_top = rect.top() + 14.0;
        let r_wave = Rect::from_min_max(Pos2::new(r.left(), rect.top() + 16.0), Pos2::new(r.right(), r.bottom()));
        let w = r.width().max(1.0);
        let tot = total.max(1) as f64;
        // input: wheel zoom about the pointer, drag to pan
        let min_span = (512.0 / tot).min(1.0);
        if let Some(h) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                ui.input_mut(|i| i.smooth_scroll_delta = vec2(0.0, 0.0));
                let t = ((h.x - r.left()) / w).clamp(0.0, 1.0) as f64;
                let anchor = self.view.0 + t * self.view.1;
                let span = (self.view.1 * (-scroll as f64 * 0.003).exp()).clamp(min_span, 1.0);
                self.view = ((anchor - t * span).clamp(0.0, 1.0 - span), span);
            }
        }
        let zoomed = self.view.1 < 0.999;
        let drag_seek = !zoomed || ui.input(|i| i.modifiers.shift);
        if resp.dragged() && zoomed && !drag_seek {
            let dx = resp.drag_delta().x as f64 / w as f64 * self.view.1;
            self.view.0 = (self.view.0 - dx).clamp(0.0, 1.0 - self.view.1);
        }
        if self.follow && playing && zoomed {
            let p = pos as f64 / tot;
            if p < self.view.0 || p > self.view.0 + self.view.1 * 0.92 {
                self.view.0 = (p - self.view.1 * 0.08).clamp(0.0, 1.0 - self.view.1);
            }
        }
        let (v0, v1) = (self.view.0 * tot, (self.view.0 + self.view.1) * tot);
        let per_px = (v1 - v0) / w as f64;
        let mid = r_wave.center().y;
        let half = r_wave.height() * 0.5;
        // time ruler
        let secs_span = ((v1 - v0) / sr as f64) as f32;
        let step = crate::views::nice_step(secs_span, (w / 90.0).clamp(2.0, 10.0));
        let t0s = (v0 / sr as f64) as f32;
        let mut t = (t0s / step).ceil() * step;
        while t < t0s + secs_span {
            let x = r.left() + ((t as f64 * sr as f64 - v0) / (v1 - v0)) as f32 * w;
            painter.line_segment([Pos2::new(x, r_top), Pos2::new(x, rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(14)));
            painter.text(Pos2::new(x + 2.0, rect.top() + 1.0), Align2::LEFT_TOP, fmt_time(t), FontId::proportional(9.0), Color32::from_white_alpha(110));
            t += step;
        }
        painter.line_segment([Pos2::new(r.left(), mid), Pos2::new(r.right(), mid)], Stroke::new(1.0, Color32::from_white_alpha(18)));
        let nb = file.overview.len();
        let d = &file.data;
        for x in 0..w as usize {
            let (a, b) = (v0 + x as f64 * per_px, v0 + (x + 1) as f64 * per_px);
            let (lo, hi) = if per_px >= OV_BLOCK as f64 * 2.0 {
                let b0 = (a as usize / OV_BLOCK).min(nb.saturating_sub(1));
                let b1 = ((b as usize / OV_BLOCK) + 1).min(nb).max(b0 + 1).min(nb);
                file.overview[b0..b1].iter().fold((0f32, 0f32), |(l, h), &(x, y)| (l.min(x), h.max(y)))
            } else {
                let (i0, i1) = (a as usize, (b.ceil() as usize + 1).min(total));
                if i0 >= i1 {
                    continue;
                }
                d.l[i0..i1].iter().chain(&d.r[i0..i1]).fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)))
            };
            let px = r.left() + x as f32 + 0.5;
            painter.line_segment([Pos2::new(px, mid - hi.min(1.0) * half), Pos2::new(px, mid - lo.max(-1.0) * half - 0.5)], Stroke::new(1.0, Color32::from_rgb(70, 150, 200)));
        }
        if (pos as f64) >= v0 && (pos as f64) <= v1 {
            let hx = r.left() + ((pos as f64 - v0) / (v1 - v0)) as f32 * w;
            painter.line_segment([Pos2::new(hx, r_top), Pos2::new(hx, rect.bottom())], Stroke::new(1.5, Color32::from_rgb(255, 210, 80)));
        }
        if resp.clicked() || (resp.dragged() && drag_seek) {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.x - r.left()) / w).clamp(0.0, 1.0) as f64;
                let target = (v0 + t * (v1 - v0)) as i64;
                if self.play.is_none() {
                    self.start_play(cx);
                    if let Some(pl) = &self.play {
                        pl.playing.store(false, Relaxed);
                        pl.pos.store(target as u64, Relaxed);
                    }
                }
                if let Some(pl) = &self.play {
                    pl.seek.store(target, Relaxed);
                }
            }
        }
        painter.text(rect.right_bottom() - vec2(4.0, 2.0), Align2::RIGHT_BOTTOM, "wheel: zoom   drag: pan   click: seek", FontId::proportional(9.0), Color32::from_white_alpha(70));
    }
}

impl Tool for Recorder {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        if let Some(j) = &self.rec {
            if j.sh.done.load(Relaxed) {
                let e = j.sh.err.lock().clone();
                self.last_err = e;
                self.last_file = Some(j.path.clone());
                cx.req.note = Some(format!("Recording saved: {}", j.path.display()));
                self.rec = None;
            }
        }
        // test hook: VISUSCOPE_TEST_PLAY=file loads and plays a file through the virtual cable
        if let Some(p) = std::env::var_os("VISUSCOPE_TEST_PLAY") {
            if self.test_stage == 0 {
                self.test_stage = 1;
                self.load_file(p.into());
                if let Some(d) = cx.out_devs.iter().find(|d| d.label.contains("CABLE Input")) {
                    self.mon_dev = d.id.clone();
                }
            } else if self.test_stage == 1 && self.file.is_some() {
                self.test_stage = 2;
                self.start_play(cx);
            }
        }
        if self.play.as_ref().is_some_and(|p| p.stop.load(Relaxed)) {
            self.play = None;
        }
        self.manage_monitor(cx);
        if let Some(d) = &self.open_dlg {
            if let Some(res) = d.take() {
                self.open_dlg = None;
                if let Some(p) = res {
                    self.play_err.clear();
                    self.load_file(p);
                }
            }
        }
        if let Some(res) = self.load_slot.try_lock().and_then(|mut g| g.take()) {
            self.loading = false;
            match res {
                Ok(f) => {
                    self.stop_play_thread();
                    self.view = (0.0, 1.0);
                    self.file = Some(Arc::new(f));
                }
                Err(e) => self.play_err = format!("cannot open file: {e}"),
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            self.rec_ui(ui, cx);
            ui.separator();
            self.play_ui(ui, cx);
        });
    }

    fn status(&self) -> Option<(String, Color32)> {
        if let Some(j) = &self.rec {
            return Some((format!("REC {}", fmt_time(j.sh.frames.load(Relaxed) as f32 / j.sr as f32)), Color32::from_rgb(255, 100, 100)));
        }
        let (p, f) = (self.play.as_ref()?, self.file.as_ref()?);
        let state = if p.playing.load(Relaxed) { "playing" } else { "paused" };
        Some((format!("{} {}", state, f.name), Color32::from_rgb(120, 190, 255)))
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("fmt", Format::ALL.iter().position(|f| *f == self.fmt).unwrap_or(0));
        o.put("pre", self.pre_roll);
        o.put("loop", self.looping);
        o.put("mon_on", self.mon_on);
        o.put("mon_dev", &self.mon_dev);
        o.put("mon_vol", self.mon_vol);
        o.put("follow", self.follow);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(k) = i.get::<usize>("fmt") {
            self.fmt = Format::ALL[k.min(2)];
        }
        if let Some(v) = i.get::<f32>("pre") {
            self.pre_roll = v.clamp(0.0, 15.0);
        }
        i.set("loop", &mut self.looping);
        i.set("mon_on", &mut self.mon_on);
        i.set("follow", &mut self.follow);
        i.set("mon_vol", &mut self.mon_vol);
        self.mon_vol = self.mon_vol.clamp(0.0, 1.0);
        if let Some(d) = i.str("mon_dev") {
            self.mon_dev = d;
        }
    }
}
