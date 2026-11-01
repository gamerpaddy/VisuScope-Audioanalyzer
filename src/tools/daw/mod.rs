//! Multitrack timeline: record several inputs (any channel, pair or mono mix of any device) onto tracks at
//! once, load audio files, edit non-destructively (split, trim, move, fades, gain, reverse ...) and analyse
//! the result. Playback feeds the same ring as live input, so every analyser window (spectrum, scope,
//! loudness ...) works on the timeline too.

mod analysis;
pub mod capture;
mod file;
mod ops;
mod project;
mod tool;
mod ui;

use super::dialogs::{self, Pending, PendingMany};
use super::monitor::{self, Queue};
use super::wavio::{Format, WavWriter};
use super::*;
use analysis::AnalysisOut;
use capture::{Capture, DevEx};
use eframe::egui::TextureHandle;
use parking_lot::Mutex;
use project::*;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

const NO_SEEK: i64 = i64::MIN;
/// Longest stretch that is analysed in one go (minutes of audio).
const ANALYSIS_MAX_SECS: usize = 300;

// ---------------------------------------------------------------------- playback

struct PlayCtl {
    pos: AtomicI64,
    stop: AtomicBool,
    seek: AtomicI64,
    loop_on: AtomicBool,
    loop_a: AtomicI64,
    loop_b: AtomicI64,
    /// do not stop at the end of the project (recording)
    keep: AtomicBool,
    sound: AtomicBool,
    /// the mix goes into the analysers (replacing the live input); otherwise the live input stays untouched
    feed: AtomicBool,
    /// track whose post-fader signal feeds the analysers (-1 = master mix)
    focus: AtomicI64,
    master: [AtomicU32; 2],
    tracks: Vec<AtomicU32>,
    ended: AtomicBool,
}

impl PlayCtl {
    fn new(pos: i64) -> Self {
        Self {
            pos: AtomicI64::new(pos),
            stop: AtomicBool::new(false),
            seek: AtomicI64::new(NO_SEEK),
            loop_on: AtomicBool::new(false),
            loop_a: AtomicI64::new(0),
            loop_b: AtomicI64::new(0),
            keep: AtomicBool::new(false),
            sound: AtomicBool::new(true),
            feed: AtomicBool::new(false),
            focus: AtomicI64::new(-1),
            master: [AtomicU32::new(0), AtomicU32::new(0)],
            tracks: (0..2 * MAX_TRACKS).map(|_| AtomicU32::new(0)).collect(),
            ended: AtomicBool::new(false),
        }
    }
}

fn play_thread(sh: Arc<Shared>, ctl: Arc<PlayCtl>, proj: Arc<Mutex<Project>>, mon: Arc<Mutex<Queue>>) {
    let feed = ctl.feed.load(Relaxed);
    let sr = proj.lock().sr;
    if feed {
        // make sure the live capture is gone before the timeline takes over the ring
        sh.audio_gen.fetch_add(1, Relaxed);
        std::thread::sleep(Duration::from_millis(150));
        {
            let mut ring = sh.ring.lock();
            ring.total = 0;
            ring.l.fill(0.0);
            ring.r.fill(0.0);
        }
        sh.sr.store(sr, Relaxed);
        sh.chans.store(2, Relaxed);
        sh.feeding_silence.store(false, Relaxed);
        *sh.status.lock() = format!("Timeline ({sr} Hz)");
    }
    {
        let mut q = mon.lock();
        q.buf.clear();
        q.src_sr = sr;
    }
    let mut pos = ctl.pos.load(Relaxed);
    const BLK: usize = 4096;
    let (mut l, mut r, mut fl, mut fr) = (vec![0f32; BLK], vec![0f32; BLK], vec![0f32; BLK], vec![0f32; BLK]);
    let mut pk = vec![[0f32; 2]; MAX_TRACKS];
    let (mut carry, mut last) = (0f64, Instant::now());
    while !ctl.stop.load(Relaxed) {
        std::thread::sleep(Duration::from_millis(4));
        // the file player took over the analysers: step aside
        if feed && sh.ring_owner.load(Relaxed) != 2 {
            break;
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.1);
        last = now;
        let s = ctl.seek.swap(NO_SEEK, Relaxed);
        if s != NO_SEEK {
            pos = s.max(0);
            mon.lock().buf.clear();
        }
        carry += dt * sr as f64;
        let mut n = carry.floor() as usize;
        carry -= n as f64;
        let (la, lb) = (ctl.loop_a.load(Relaxed), ctl.loop_b.load(Relaxed));
        let looping = ctl.loop_on.load(Relaxed) && lb > la + 64;
        while n > 0 {
            if looping && pos >= lb {
                pos = la;
            }
            let mut k = n.min(BLK);
            if looping {
                k = k.min((lb - pos).max(1) as usize);
            }
            let focus = ctl.focus.load(Relaxed);
            let to_graph = graph_on();
            {
                let p = proj.lock();
                p.mix(pos, &mut l[..k], &mut r[..k], None, &mut pk);
                if to_graph {
                    // the routing graph reads the master and the tracks its nodes ask for
                    let b = crate::graph::bus();
                    b.tl_push(sr, None, &l[..k], &r[..k]);
                    let want = b.want_tracks.lock().clone();
                    let mut scratch = vec![[0f32; 2]; MAX_TRACKS];
                    for tid in want {
                        p.mix(pos, &mut fl[..k], &mut fr[..k], Some(tid), &mut scratch);
                        b.tl_push(sr, Some(tid), &fl[..k], &fr[..k]);
                    }
                }
                if focus >= 0 && !to_graph {
                    let mut scratch = vec![[0f32; 2]; MAX_TRACKS];
                    p.mix(pos, &mut fl[..k], &mut fr[..k], Some(focus as u32), &mut scratch);
                }
                for (i, t) in pk.iter().enumerate().take(p.tracks.len()) {
                    ctl.tracks[2 * i].fetch_max(t[0].to_bits(), Relaxed);
                    ctl.tracks[2 * i + 1].fetch_max(t[1].to_bits(), Relaxed);
                }
            }
            for (c, x) in [&l[..k], &r[..k]].into_iter().enumerate() {
                let m = x.iter().fold(0f32, |m, v| m.max(v.abs()));
                ctl.master[c].fetch_max(m.to_bits(), Relaxed);
            }
            let (al, ar) = if focus >= 0 { (&fl[..k], &fr[..k]) } else { (&l[..k], &r[..k]) };
            if feed {
                let mut ring = sh.ring.lock();
                for i in 0..k {
                    ring.push(al[i], ar[i]);
                }
            }
            if ctl.sound.load(Relaxed) && !to_graph {
                mon.lock().push(&l[..k], &r[..k]);
            }
            if feed {
                sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
                sh.cv.notify_one();
            }
            pos += k as i64;
            n -= k;
        }
        ctl.pos.store(pos, Relaxed);
        if !looping && !ctl.keep.load(Relaxed) && pos > proj.lock().end_frame() + sr as i64 / 4 {
            break;
        }
    }
    ctl.ended.store(true, Relaxed);
}

// ---------------------------------------------------------------------- jobs

enum Job {
    /// `target`: put the audio on this track at `at` instead of on new tracks
    Imported { name: String, res: Result<Vec<Arc<Source>>, String>, at: i64, target: Option<u32> },
    Recorded(Vec<(u32, Arc<Source>, i64)>, bool),
    Analysis(AnKey, Arc<AnalysisOut>),
    Saved(Result<PathBuf, String>, bool),
    Loaded(Result<file::Loaded, String>, bool, PathBuf),
    Bounced(Arc<Source>, i64),
    /// the mix of several tracks: the new source, where it starts, the tracks it replaces (may be empty), and where
    /// the new track goes (index of the first track that was merged)
    Merged(Arc<Source>, i64, Vec<u32>, usize),
    Done(String),
}

/// What an analysis was computed for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct AnKey {
    rev: u64,
    a: i64,
    b: i64,
    track: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum AnSrc {
    Master,
    Track,
}

struct RecPlan {
    track_id: u32,
    dev: String,
    lanes: Vec<usize>,
    name: String,
    /// input trim of the track at the start of the take
    gain_db: f32,
    shift_ms: f32,
}

struct RecState {
    start: i64,
    t0: Instant,
    plan: Vec<RecPlan>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum DragKind {
    Move,
    TrimL,
    TrimR,
    FadeIn,
    FadeOut,
}

pub(super) struct ClipDrag {
    kind: DragKind,
    press_frame: i64,
    press_track: usize,
    grab: u32,
    orig: Vec<(Clip, usize)>,
}

enum Export {
    Mix(Option<(i64, i64)>),
    Clip(u32),
    StemsDir,
}

pub struct Daw {
    proj: Arc<Mutex<Project>>,
    src_ids: Arc<AtomicU32>,
    undo: Vec<Project>,
    redo: Vec<Project>,
    /// the project in the UI differs from the shared one and must be stored
    edited: bool,
    /// bumped by every edit (analysis cache key)
    rev: u64,
    saved_rev: u64,
    last_edit: Instant,
    // view
    t0: f64,
    pps: f64,
    vscroll: f32,
    /// width of the lanes in pixels (for fitting)
    lane_w: f32,
    fit_pending: bool,
    // selection
    cursor: i64,
    range: Option<(i64, i64)>,
    sel: BTreeSet<u32>,
    sel_track: Option<u32>,
    /// more tracks selected with Ctrl + click on the track headers (for merging)
    sel_tracks: BTreeSet<u32>,
    clip_buf: Vec<(Clip, i64, usize)>,
    // options
    snap: bool,
    follow: bool,
    loop_on: bool,
    sound: bool,
    mon_dev: String,
    mon_vol: f32,
    rec_off_ms: f32,
    // transport
    play: Option<Arc<PlayCtl>>,
    mon: Option<(monitor::Handle, String)>,
    mon_q: Arc<Mutex<Queue>>,
    mon_gain: Arc<AtomicU32>,
    mon_status: Arc<Mutex<String>>,
    // recording
    devs: Arc<Mutex<Option<Vec<DevEx>>>>,
    devs_v: Vec<DevEx>,
    capt: HashMap<String, Arc<Capture>>,
    rec: Option<RecState>,
    in_level: HashMap<u32, [f32; 2]>,
    out_level: Vec<[f32; 2]>,
    master_level: [f32; 2],
    // interaction
    drag: Option<ClipDrag>,
    range_anchor: Option<i64>,
    ctx_clip: Option<u32>,
    renaming: Option<(u32, String)>,
    visible: bool,
    // background work
    jobs: Arc<Mutex<Vec<Job>>>,
    busy: Arc<AtomicU32>,
    open_dlg: Option<PendingMany>,
    /// an import already decided the rate of the (empty) project
    rate_decided: Arc<AtomicBool>,
    /// show the timeline in the analysers while playing (takes over the live input)
    feed_analysers: bool,
    /// one-shot override of `feed_analysers` (Loop & play)
    feed_once: bool,
    /// track and frame the file chosen in `open_dlg` goes to (right-click import)
    import_target: Option<(u32, i64)>,
    bus_t: Instant,
    /// the analysis is switched on but the window has no room for it
    an_hidden: bool,
    /// track whose channels should be split into two tracks after this frame
    split_req: Option<u32>,
    /// timeline frame under the last right-click
    ctx_frame: i64,
    /// slider that is being edited by typing a value
    val_edit: Option<(eframe::egui::Id, String, bool)>,
    export_dlg: Option<(Pending, Export)>,
    proj_dlg: Option<(Pending, bool)>,
    // analysis
    an_open: bool,
    an_src: AnSrc,
    an_out: Option<Arc<AnalysisOut>>,
    an_done: Option<AnKey>,
    an_want: Option<AnKey>,
    an_want_t: Instant,
    an_running: bool,
    spg_tex: Option<TextureHandle>,
    spg_ptr: usize,
    // files
    path: Option<PathBuf>,
    last_autosave: Instant,
    note: String,
    startup: u8,
}

/// The routing graph is running: tracks record from graph nodes and playback goes to the graph.
fn graph_on() -> bool {
    crate::graph::bus().on.load(Relaxed)
}

/// Delay (ms) between the timeline playing a sound and a record node of the routing graph receiving it, for a
/// record node that is fed (also) from a timeline output node: the take is placed that much earlier.
fn graph_loop_ms(dev: &str) -> f32 {
    if dev.starts_with("graph:") && crate::graph::bus().virt_loop.lock().contains(dev) {
        // the engine cushions its timeline sources (25 ms + the safety buffer), plus the blocks of the player and the engine
        25.0 + crate::prof::safety_ms() as f32 + 6.0
    } else {
        0.0
    }
}

/// Input of a track that records what its "Timeline record" node in the routing graph receives (stereo).
fn graph_input(track: u32) -> Input {
    Input { dev: format!("graph:{track}"), mode: Mode::Pair(0) }
}

fn autosave_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("visuscope").join("timeline_autosave").join("session.vsproj")
}

impl Daw {
    pub fn new() -> Self {
        let devs: Arc<Mutex<Option<Vec<DevEx>>>> = Arc::new(Mutex::new(None));
        let d2 = devs.clone();
        std::thread::Builder::new().name("daw-probe".into()).spawn(move || *d2.lock() = Some(capture::probe_all())).ok();
        Self {
            proj: Arc::new(Mutex::new(Project::new(48000))),
            src_ids: Arc::new(AtomicU32::new(1)),
            undo: Vec::new(),
            redo: Vec::new(),
            edited: false,
            rev: 1,
            saved_rev: 1,
            last_edit: Instant::now(),
            t0: 0.0,
            pps: 80.0,
            vscroll: 0.0,
            lane_w: 800.0,
            fit_pending: false,
            cursor: 0,
            range: None,
            sel: BTreeSet::new(),
            sel_track: None,
            sel_tracks: BTreeSet::new(),
            clip_buf: Vec::new(),
            snap: true,
            follow: true,
            loop_on: false,
            sound: true,
            mon_dev: String::new(),
            mon_vol: 0.7,
            rec_off_ms: 0.0,
            play: None,
            mon: None,
            mon_q: Arc::new(Mutex::new(Queue::default())),
            mon_gain: Arc::new(AtomicU32::new(0.49f32.to_bits())),
            mon_status: Arc::new(Mutex::new(String::new())),
            devs,
            devs_v: Vec::new(),
            capt: HashMap::new(),
            rec: None,
            in_level: HashMap::new(),
            out_level: vec![[0.0; 2]; MAX_TRACKS],
            master_level: [0.0; 2],
            drag: None,
            range_anchor: None,
            ctx_clip: None,
            renaming: None,
            visible: false,
            jobs: Arc::new(Mutex::new(Vec::new())),
            busy: Arc::new(AtomicU32::new(0)),
            open_dlg: None,
            rate_decided: Arc::new(AtomicBool::new(false)),
            feed_analysers: false,
            feed_once: false,
            import_target: None,
            bus_t: Instant::now(),
            an_hidden: false,
            split_req: None,
            ctx_frame: 0,
            val_edit: None,
            export_dlg: None,
            proj_dlg: None,
            an_open: true,
            an_src: AnSrc::Master,
            an_out: None,
            an_done: None,
            an_want: None,
            an_want_t: Instant::now(),
            an_running: false,
            spg_tex: None,
            spg_ptr: 0,
            path: None,
            last_autosave: Instant::now(),
            note: String::new(),
            startup: 0,
        }
    }

    // ------------------------------------------------------------------ helpers

    fn sr(&self) -> u32 {
        self.proj.lock().sr
    }

    fn pos(&self) -> i64 {
        self.play.as_ref().map_or(self.cursor, |p| p.pos.load(Relaxed))
    }

    fn seek(&mut self, f: i64) {
        self.cursor = f.max(0);
        if let Some(p) = &self.play {
            p.seek.store(self.cursor, Relaxed);
            p.pos.store(self.cursor, Relaxed);
        }
    }

    /// Remember the project before an edit (one undo step per call).
    fn checkpoint(&mut self, p: &Project) {
        self.undo.push(p.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.touch();
    }

    fn touch(&mut self) {
        self.edited = true;
        self.last_edit = Instant::now();
    }

    fn undo(&mut self, p: &mut Project) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(p, prev));
            self.after_history(p);
        }
    }

    fn redo(&mut self, p: &mut Project) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(p, next));
            self.after_history(p);
        }
    }

    fn after_history(&mut self, p: &Project) {
        // selections may point at clips that are gone now
        self.sel.retain(|id| p.find_clip(*id).is_some());
        if self.sel_track.is_some_and(|t| p.track_index(t).is_none()) {
            self.sel_track = None;
        }
        self.touch();
    }

    fn new_src_id(&self) -> u32 {
        file::next_src_id(&self.src_ids)
    }

    /// Modify the shared project outside of the UI pass.
    fn with_proj<R>(&mut self, f: impl FnOnce(&mut Project) -> R) -> R {
        let mut p = self.proj.lock();
        let before = p.clone();
        let r = f(&mut p);
        drop(p);
        self.undo.push(before);
        self.redo.clear();
        self.rev += 1;
        self.last_edit = Instant::now();
        r
    }

    fn spawn_job(&self, name: &str, f: impl FnOnce(&Mutex<Vec<Job>>) + Send + 'static) {
        let (jobs, busy) = (self.jobs.clone(), self.busy.clone());
        busy.fetch_add(1, Relaxed);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // a task that panics (a damaged file, a bug) must not leave the program waiting for it
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&jobs))).is_err() {
                    jobs.lock().push(Job::Done("A background task failed (see the log); the file may be damaged".into()));
                }
                busy.fetch_sub(1, Relaxed);
            })
            .ok();
    }

    // ------------------------------------------------------------------ import

    pub(super) fn import(&mut self, paths: Vec<PathBuf>) {
        self.import_at(paths, None);
    }

    /// Import files; the first one goes onto the given track at the given frame, the others get new tracks.
    pub(super) fn import_at(&mut self, paths: Vec<PathBuf>, target: Option<(u32, i64)>) {
        let mut at = self.cursor.max(0);
        if self.pos() == 0 {
            at = 0;
        }
        for (k, path) in paths.into_iter().enumerate() {
            let (at, target) = match target {
                Some((t, f)) if k == 0 => (f, Some(t)),
                _ => (at, None),
            };
            let ids = self.src_ids.clone();
            let (proj, decided) = (self.proj.clone(), self.rate_decided.clone());
            let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "audio".into());
            let name2 = name.clone();
            self.spawn_job("daw-import", move |jobs| {
                let res = super::decode::read_all(&path).map(|(fsr, chans)| {
                    // the first file into an empty project decides the project rate (no conversion then)
                    let sr = {
                        let mut p = proj.lock();
                        if p.is_empty() && p.tracks.is_empty() && !decided.swap(true, Relaxed) {
                            p.sr = fsr;
                        }
                        p.sr
                    };
                    let chans: Vec<Vec<f32>> = chans.iter().map(|c| resample(c, fsr, sr)).collect();
                    let mk = |n: String, c: Vec<Vec<f32>>| {
                        let s = Source::new(file::next_src_id(&ids), n, c);
                        Arc::new(s)
                    };
                    let srcs: Vec<Arc<Source>> = if chans.len() <= 2 {
                        vec![mk(name2.clone(), chans)]
                    } else {
                        chans.into_iter().enumerate().map(|(i, c)| mk(format!("{name2} ch{}", i + 1), vec![c])).collect()
                    };
                    srcs
                });
                jobs.lock().push(Job::Imported { name, res, at, target });
            });
        }
    }

    // ------------------------------------------------------------------ transport

    fn play_start(&mut self, cx: &mut Ctx) {
        if self.play.is_some() {
            return;
        }
        let ctl = Arc::new(PlayCtl::new(self.cursor));
        ctl.sound.store(self.sound, Relaxed);
        // by default the analysers keep showing the live input; only on request does the timeline replace it
        let feed = (self.feed_analysers || std::mem::take(&mut self.feed_once)) && !graph_on();
        ctl.feed.store(feed, Relaxed);
        if feed {
            cx.sh.ring_owner.store(2, Relaxed);
        }
        let (sh, c2, p2, q) = (cx.sh.clone(), ctl.clone(), self.proj.clone(), self.mon_q.clone());
        std::thread::Builder::new().name("timeline-player".into()).spawn(move || play_thread(sh, c2, p2, q)).ok();
        self.play = Some(ctl);
        if feed {
            cx.req.audio = AudioReq::Stop;
        }
    }

    fn play_stop(&mut self, cx: &mut Ctx) {
        if self.play.is_some() && graph_on() {
            crate::graph::bus().tl_clear();
        }
        if let Some(p) = self.play.take() {
            p.stop.store(true, Relaxed);
            self.cursor = p.pos.load(Relaxed).max(0);
            if cx.sh.ring_owner.compare_exchange(2, 0, Relaxed, Relaxed).is_ok() {
                cx.req.audio = AudioReq::Restart;
            }
        }
    }

    fn toggle_play(&mut self, cx: &mut Ctx) {
        if self.rec.is_some() {
            self.stop_record(cx);
        } else if self.play.is_some() {
            self.play_stop(cx);
        } else {
            self.play_start(cx);
        }
    }

    /// Keep the loop points and the analysis target of the running player current; notice its end.
    fn sync_play(&mut self, cx: &mut Ctx) {
        let Some(p) = self.play.clone() else { return };
        if p.ended.load(Relaxed) {
            if self.rec.is_some() {
                self.stop_record(cx);
            } else {
                self.play_stop(cx);
            }
            return;
        }
        if graph_on() {
            // the timeline sends its mix to the routing graph; a seek must not replay old audio
            if p.seek.load(Relaxed) != NO_SEEK {
                crate::graph::bus().tl_clear();
            }
        }
        let (a, b) = self.range.unwrap_or((0, self.proj.lock().end_frame()));
        p.loop_on.store(self.loop_on, Relaxed);
        p.loop_a.store(a, Relaxed);
        p.loop_b.store(b, Relaxed);
        p.keep.store(self.rec.is_some(), Relaxed);
        p.sound.store(self.sound, Relaxed);
        let focus = match self.an_src {
            AnSrc::Master => -1,
            AnSrc::Track => self.sel_track.map_or(-1, |t| t as i64),
        };
        p.focus.store(focus, Relaxed);
    }

    // ------------------------------------------------------------------ recording

    fn default_input(&self, p: &Project) -> Option<Input> {
        let dev = self.devs_v.first()?;
        let used: Vec<usize> = p.tracks.iter().filter(|t| t.arm && t.input.dev == dev.id).flat_map(|t| t.input.mode.lanes(dev.ch)).collect();
        let c = (0..dev.ch).find(|c| !used.contains(c)).unwrap_or(0);
        Some(Input { dev: dev.id.clone(), mode: Mode::Chan(c) })
    }

    fn start_record(&mut self, cx: &mut Ctx) {
        if self.rec.is_some() {
            return;
        }
        // nothing armed: arm a track on the default input so one click is enough to record
        {
            let mut p = self.proj.lock();
            if !p.tracks.iter().any(|t| t.arm) {
                let graph = graph_on();
                let Some(mut input) = (if graph { Some(Input { dev: String::new(), mode: Mode::Pair(0) }) } else { self.default_input(&p) }) else {
                    self.note = "Timeline: no input device found".into();
                    return;
                };
                if let Some(d) = self.devs_v.iter().find(|d| d.id == input.dev) {
                    if d.ch >= 2 {
                        input.mode = Mode::Pair(0);
                    }
                }
                let ti = match self.sel_track.and_then(|id| p.track_index(id)) {
                    Some(ti) => ti,
                    None => p.add_track(None, input.clone()),
                };
                if graph {
                    // with the routing graph a track records what its "Timeline record" node receives
                    input = graph_input(p.tracks[ti].id);
                    crate::graph::bus().request_rec_node(p.tracks[ti].id, &p.tracks[ti].name);
                }
                p.tracks[ti].input = input;
                p.tracks[ti].arm = true;
                self.sel_track = Some(p.tracks[ti].id);
            }
        }
        self.manage_capture();
        let p = self.proj.lock().clone();
        let mut plan: Vec<RecPlan> = Vec::new();
        for t in p.tracks.iter().filter(|t| t.arm) {
            let Some(cap) = self.capt.get(&t.input.dev) else {
                if t.input.dev.starts_with("graph:") {
                    crate::graph::bus().request_rec_node(t.id, &t.name);
                    self.note = format!("Timeline: connect something to the record node of {} in the Routing window", t.name);
                }
                continue;
            };
            let Some((_, ch)) = cap.format() else {
                self.note = format!("Timeline: input of {} is not ready ({})", t.name, cap.status.lock());
                return;
            };
            plan.push(RecPlan { track_id: t.id, dev: t.input.dev.clone(), lanes: t.input.mode.lanes(ch), name: t.name.clone(), gain_db: t.in_db, shift_ms: t.in_ms + graph_loop_ms(&t.input.dev) });
        }
        if plan.is_empty() {
            self.note = "Timeline: arm a track with the R button first".into();
            return;
        }
        let mut per_dev: HashMap<&str, Vec<usize>> = HashMap::new();
        for r in &plan {
            per_dev.entry(r.dev.as_str()).or_default().extend(r.lanes.iter().copied());
        }
        let start = self.pos();
        for (dev, lanes) in per_dev {
            if let Some(c) = self.capt.get(dev) {
                c.begin(&lanes);
            }
        }
        self.rec = Some(RecState { start, t0: Instant::now(), plan });
        self.play_start(cx);
        self.note.clear();
    }

    fn stop_record(&mut self, cx: &mut Ctx) {
        let Some(rec) = self.rec.take() else { return };
        let mut takes: HashMap<String, capture::Take> = HashMap::new();
        for r in &rec.plan {
            if !takes.contains_key(&r.dev) {
                if let Some(c) = self.capt.get(&r.dev) {
                    takes.insert(r.dev.clone(), c.finish());
                }
            }
        }
        self.play_stop(cx);
        let (sr, off) = (self.sr(), (self.rec_off_ms * self.sr() as f32 / 1000.0) as i64);
        let ids = self.src_ids.clone();
        let plan = rec.plan;
        let start = rec.start;
        self.spawn_job("daw-take", move |jobs| {
            let dir = docs_dir().join("Timeline");
            let _ = std::fs::create_dir_all(&dir);
            let (stamp, _) = utc_stamp(now_unix());
            let mut out = Vec::new();
            let mut gap = false;
            for r in &plan {
                let Some(t) = takes.get(&r.dev) else { continue };
                gap |= t.gap;
                let g = db_to_lin(r.gain_db);
                let mut chans: Vec<Vec<f32>> = r
                    .lanes
                    .iter()
                    .filter_map(|&l| t.lanes.get(l))
                    .map(|c| {
                        let mut v = resample(c, t.sr, sr);
                        if (g - 1.0).abs() > 1e-6 {
                            v.iter_mut().for_each(|x| *x *= g);
                        }
                        v
                    })
                    .collect();
                // an input that arrives late is placed earlier (its own trim on top of the global compensation);
                // what would start before the beginning of the timeline is cut off instead of being moved
                let own = (r.shift_ms * sr as f32 / 1000.0) as i64;
                let mut place = start - off - own;
                if place < 0 {
                    let cut = (-place) as usize;
                    for c in chans.iter_mut() {
                        c.drain(..cut.min(c.len()));
                    }
                    place = 0;
                }
                if chans.is_empty() || chans[0].is_empty() {
                    continue;
                }
                let s = Source::new(file::next_src_id(&ids), format!("{} {}", r.name, &stamp[9..]), chans);
                let path = dir.join(format!("take_{stamp}_{}.wav", r.track_id));
                if file::write_wav(&path, &s, sr).is_ok() {
                    *s.saved.lock() = Some(path);
                }
                out.push((r.track_id, Arc::new(s), place));
            }
            jobs.lock().push(Job::Recorded(out, gap));
        });
    }

    /// Open exactly the capture streams the armed tracks need.
    fn manage_capture(&mut self) {
        if self.rec.is_some() {
            return;
        }
        let want: BTreeSet<String> = self.proj.lock().tracks.iter().filter(|t| t.arm && !t.input.dev.is_empty()).map(|t| t.input.dev.clone()).collect();
        self.capt.retain(|k, _| want.contains(k));
        for d in want {
            if d.starts_with("graph:") {
                // the routing graph feeds this one; it may be replaced when the graph is rebuilt
                if let Some(c) = crate::graph::bus().virt.lock().get(&d).cloned() {
                    if self.capt.get(&d).map_or(true, |old| !Arc::ptr_eq(old, &c)) {
                        self.capt.insert(d, c);
                    }
                }
            } else {
                self.capt.entry(d.clone()).or_insert_with(|| Arc::new(Capture::start(d)));
            }
        }
    }

    fn update_meters(&mut self) {
        let decay = 0.88f32;
        let n = self.proj.lock().tracks.len();
        self.out_level.resize(MAX_TRACKS.max(n), [0.0; 2]);
        match &self.play {
            Some(p) => {
                for i in 0..n.min(MAX_TRACKS) {
                    for c in 0..2 {
                        let v = f32::from_bits(p.tracks[2 * i + c].swap(0, Relaxed));
                        self.out_level[i][c] = v.max(self.out_level[i][c] * decay);
                    }
                }
                for c in 0..2 {
                    let v = f32::from_bits(p.master[c].swap(0, Relaxed));
                    self.master_level[c] = v.max(self.master_level[c] * decay);
                }
            }
            None => {
                for l in self.out_level.iter_mut() {
                    *l = [l[0] * decay, l[1] * decay];
                }
                self.master_level = [self.master_level[0] * decay, self.master_level[1] * decay];
            }
        }
        let p = self.proj.lock();
        let mut seen = Vec::new();
        for t in p.tracks.iter().filter(|t| t.arm) {
            let Some(cap) = self.capt.get(&t.input.dev) else { continue };
            let Some((_, ch)) = cap.format() else { continue };
            let lanes = t.input.mode.lanes(ch);
            seen.push((t.id, cap.id.clone(), lanes));
        }
        drop(p);
        // peaks are taken once per device and shared by the tracks that read it
        let mut dev_peaks: HashMap<String, Vec<f32>> = HashMap::new();
        for (_, dev, _) in &seen {
            if !dev_peaks.contains_key(dev) {
                if let Some(c) = self.capt.get(dev) {
                    let ch = c.format().map_or(2, |f| f.1);
                    dev_peaks.insert(dev.clone(), c.take_peaks(ch));
                }
            }
        }
        for (id, dev, lanes) in seen {
            let pk = &dev_peaks[&dev];
            let g = |i: usize| lanes.get(i).and_then(|&l| pk.get(l)).copied().unwrap_or(0.0);
            let cur = self.in_level.entry(id).or_insert([0.0; 2]);
            let new = [g(0), if lanes.len() > 1 { g(1) } else { g(0) }];
            for c in 0..2 {
                cur[c] = new[c].max(cur[c] * decay);
            }
        }
    }

    /// Keep the sound output stream in line with the player state and the chosen device.
    fn manage_monitor(&mut self, cx: &Ctx) {
        let dev = if self.mon_dev.is_empty() || !cx.out_devs.iter().any(|d| d.id == self.mon_dev) { cx.out_devs.first().map(|d| d.id.clone()).unwrap_or_default() } else { self.mon_dev.clone() };
        let want = self.sound && self.play.is_some() && !graph_on();
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
        self.mon_gain.store(self.mon_vol.powi(2).to_bits(), Relaxed);
    }

    // ------------------------------------------------------------------ results of background work

    fn poll(&mut self, cx: &mut Ctx) {
        if let Some(v) = self.devs.try_lock().and_then(|mut g| g.take()) {
            self.devs_v = v;
        }
        if let Some(d) = &self.open_dlg {
            if let Some(files) = d.take() {
                self.open_dlg = None;
                let target = self.import_target.take();
                if !files.is_empty() {
                    self.import_at(files, target);
                }
            }
        }
        if let Some((d, _)) = &self.export_dlg {
            if let Some(res) = d.take() {
                if let Some((_, kind)) = self.export_dlg.take() {
                    if let Some(path) = res {
                        self.run_export(path, kind);
                    }
                }
            }
        }
        if let Some((d, save)) = &self.proj_dlg {
            if let Some(res) = d.take() {
                let save = *save;
                self.proj_dlg = None;
                if let Some(path) = res {
                    if save {
                        self.save_project(path, false);
                    } else {
                        self.open_project(path, false);
                    }
                }
            }
        }
        if self.busy.load(Relaxed) == 0 && self.jobs.lock().is_empty() && self.proj.lock().tracks.is_empty() {
            self.rate_decided.store(false, Relaxed);
        }
        let jobs: Vec<Job> = std::mem::take(&mut *self.jobs.lock());
        for j in jobs {
            match j {
                Job::Imported { name, res, at, target } => match res {
                    Err(e) => self.note = format!("Cannot open {name}: {e}"),
                    Ok(srcs) => {
                        let n = srcs.len();
                        let mut first = None;
                        self.with_proj(|p| {
                            let mut target = target.and_then(|t| p.track_index(t));
                            for s in srcs {
                                // the first source of a targeted import lands on that track
                                let ti = match target.take() {
                                    Some(ti) => ti,
                                    None => {
                                        if p.tracks.len() >= MAX_TRACKS {
                                            break;
                                        }
                                        p.add_track(Some(s.name.clone()), Input { dev: String::new(), mode: Mode::Mix })
                                    }
                                };
                                let id = p.add_clip(ti, s, at);
                                first.get_or_insert(id);
                            }
                        });
                        self.sel.clear();
                        self.sel.extend(first);
                        self.note = format!("Loaded {name} ({n} track{})", if n == 1 { "" } else { "s" });
                    }
                },
                Job::Recorded(items, gap) => {
                    let n = items.len();
                    let mut last = None;
                    self.with_proj(|p| {
                        for (tid, src, start) in items {
                            if let Some(ti) = p.track_index(tid) {
                                last = Some(p.add_clip(ti, src, start));
                            }
                        }
                    });
                    self.sel.clear();
                    self.sel.extend(last);
                    self.note = if gap { format!("Recorded {n} track(s) - the device was reconnected during the take, there may be a gap") } else { format!("Recorded {n} track(s), saved in Documents\\VisuScope\\Timeline") };
                }
                Job::Analysis(key, out) => {
                    self.an_running = false;
                    self.an_done = Some(key);
                    self.an_out = Some(out);
                }
                Job::Saved(res, auto) => match res {
                    Ok(p) => {
                        if !auto {
                            self.path = Some(p.clone());
                            self.note = format!("Project saved: {}", p.display());
                        }
                    }
                    Err(e) => self.note = format!("Save failed: {e}"),
                },
                Job::Loaded(res, restore, path) => match res {
                    Err(e) => {
                        if !restore {
                            self.note = format!("Cannot open project: {e}")
                        }
                    }
                    Ok(l) => {
                        let fresh = self.proj.lock().tracks.is_empty();
                        if restore && !fresh {
                            continue;
                        }
                        self.src_ids.fetch_max(l.max_src_id + 1, Relaxed);
                        *self.proj.lock() = l.proj;
                        self.undo.clear();
                        self.redo.clear();
                        self.sel.clear();
                        self.rev += 1;
                        self.saved_rev = self.rev;
                        if !restore {
                            self.path = Some(path.clone());
                            self.note = if l.missing > 0 { format!("Opened {} ({} clips lost: audio files missing)", path.display(), l.missing) } else { format!("Opened {}", path.display()) };
                        }
                        self.fit_pending = true;
                    }
                },
                Job::Bounced(src, at) => {
                    self.with_proj(|p| {
                        let ti = p.add_track(Some(src.name.clone()), Input { dev: String::new(), mode: Mode::Mix });
                        p.add_clip(ti, src, at);
                    });
                    self.note = "Bounced to a new track".into();
                }
                Job::Merged(src, at, remove, index) => {
                    let n = remove.len();
                    self.with_proj(|p| {
                        p.tracks.retain(|t| !remove.contains(&t.id));
                        let ti = p.add_track(Some(src.name.clone()), Input { dev: String::new(), mode: Mode::Mix });
                        p.add_clip(ti, src, at);
                        // the merged track takes the place of the first of the tracks
                        let t = p.tracks.remove(ti);
                        p.tracks.insert(index.min(p.tracks.len()), t);
                    });
                    self.sel_tracks.clear();
                    self.note = if n > 0 { format!("Merged {n} tracks into one") } else { "Mixed the tracks to a new track".into() };
                }
                Job::Done(m) => self.note = m,
            }
        }
        if !self.note.is_empty() && self.visible {
            // surface messages in the status bar of the app
            cx.req.note = Some(format!("Timeline: {}", self.note));
            self.note.clear();
        }
    }
}
