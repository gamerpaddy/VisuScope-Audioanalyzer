//! Runs the routing graph on its own thread. Sources (input devices, generators, the timeline, the file
//! player) are pulled block by block, effects run in dependency order and the sinks (analysers, output
//! devices, timeline tracks) are fed. The clock is the first input device (its callbacks deliver the
//! frames); without one the engine follows the wall clock.

use super::dsp::*;
use super::model::*;
use crate::prof;
use crate::shared::Shared;
use crate::tonegen::{GenParams, GenState};
use crate::tools::daw::capture::{Capture, DevEx};
use crate::tools::monitor::{self, Queue};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Frames processed per pass through the graph.
const CHUNK: usize = 1024;
/// Backlog the timeline / file buses keep (seconds).
const FIFO_SECS: usize = 1;
static ZERO: [f32; CHUNK] = [0.0; CHUNK];

// ---------------------------------------------------------------------- bus to the rest of the program

#[derive(Default)]
pub struct TlBus {
    pub sr: u32,
    pub master: VecDeque<[f32; 2]>,
    pub tracks: HashMap<u32, VecDeque<[f32; 2]>>,
}

#[derive(Default)]
pub struct Bus {
    /// the graph is on: the timeline and the file player hand their audio to it
    pub on: AtomicBool,
    pub tl: Mutex<TlBus>,
    /// timeline tracks (id, name), published by the timeline for the node editor
    pub tracks: Mutex<Vec<(u32, String)>>,
    /// timeline tracks whose own signal the graph reads
    pub want_tracks: Mutex<Vec<u32>>,
    /// captures the graph feeds for timeline tracks ("graph:<track id>")
    pub virt: Mutex<HashMap<String, Arc<Capture>>>,
    /// tracks that asked for a record node
    pub rec_requests: Mutex<Vec<(u32, String)>>,
    /// record nodes (capture ids) that receive audio from a timeline output: a loopback inside the program
    pub virt_loop: Mutex<std::collections::HashSet<String>>,
}

pub fn bus() -> &'static Bus {
    static B: OnceLock<Bus> = OnceLock::new();
    B.get_or_init(Bus::default)
}

fn push_capped(q: &mut VecDeque<[f32; 2]>, sr: u32, l: &[f32], r: &[f32]) {
    q.extend(l.iter().copied().zip(r.iter().copied()).map(|(a, b)| [a, b]));
    let cap = sr.max(8000) as usize * FIFO_SECS;
    if q.len() > cap {
        let d = q.len() - cap / 2;
        q.drain(..d);
    }
}

impl Bus {
    /// Timeline output: the master (`track` = None) or one track, post fader.
    pub fn tl_push(&self, sr: u32, track: Option<u32>, l: &[f32], r: &[f32]) {
        let mut t = self.tl.lock();
        t.sr = sr;
        let q = match track {
            None => &mut t.master,
            Some(id) => t.tracks.entry(id).or_default(),
        };
        push_capped(q, sr, l, r);
    }

    pub fn tl_clear(&self) {
        let mut t = self.tl.lock();
        t.master.clear();
        t.tracks.clear();
    }

    /// Ask the node editor for a "Timeline record" node of this track.
    pub fn request_rec_node(&self, track: u32, name: &str) {
        let mut v = self.rec_requests.lock();
        if !v.iter().any(|r| r.0 == track) {
            v.push((track, name.to_string()));
        }
    }
}

// ---------------------------------------------------------------------- shared state with the UI

#[derive(Clone, Copy, Default)]
pub struct Meter {
    pub peak: [f32; 2],
    /// compressor / limiter gain reduction in dB (<= 0), gate: 0 closed / 1 open
    pub extra: f32,
}

pub struct Engine {
    pub graph: Mutex<Graph>,
    pub rev: AtomicU64,
    pub active: AtomicBool,
    pub quit: AtomicBool,
    pub meters: Mutex<HashMap<NodeId, Meter>>,
    pub status: Mutex<String>,
    pub node_status: Mutex<HashMap<NodeId, String>>,
    /// 0 = follow the first input device
    pub rate_pref: AtomicU32,
    pub rate: AtomicU32,
    pub devs: Mutex<Vec<DevEx>>,
    pub default_in: Mutex<String>,
    pub gens: Arc<Mutex<Vec<GenParams>>>,
    pub sh: Arc<Shared>,
    /// milliseconds of audio the sinks hold back (for the latency display)
    pub block_ms: Mutex<f32>,
}

impl Engine {
    /// The shared state without any thread (tests drive a `Run` themselves).
    fn bare(sh: Arc<Shared>, gens: Arc<Mutex<Vec<GenParams>>>) -> Arc<Engine> {
        Arc::new(Engine {
            graph: Mutex::new(Graph::default()),
            rev: AtomicU64::new(1),
            active: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            meters: Mutex::new(HashMap::new()),
            status: Mutex::new(String::new()),
            node_status: Mutex::new(HashMap::new()),
            rate_pref: AtomicU32::new(0),
            rate: AtomicU32::new(48000),
            devs: Mutex::new(Vec::new()),
            default_in: Mutex::new(String::new()),
            gens,
            sh,
            block_ms: Mutex::new(0.0),
        })
    }

    pub fn new(sh: Arc<Shared>, gens: Arc<Mutex<Vec<GenParams>>>) -> Arc<Engine> {
        let e = Engine::bare(sh, gens);
        let e2 = e.clone();
        std::thread::Builder::new().name("graph-probe".into()).spawn(move || {
            // the generators measure their noise levels on first use: do that here, not on the engine thread
            let _ = GenState::new(48000.0);
            *e2.devs.lock() = crate::tools::daw::capture::probe_all();
            e2.rev.fetch_add(1, Relaxed);
        }).ok();
        let e3 = e.clone();
        std::thread::Builder::new().name("graph-engine".into()).spawn(move || Run::new(e3).main()).ok();
        e
    }

    pub fn set_graph(&self, g: Graph) {
        *self.graph.lock() = g;
        self.rev.fetch_add(1, Relaxed);
    }

    pub fn graph_clone(&self) -> Graph {
        self.graph.lock().clone()
    }

    pub fn meter(&self, id: NodeId) -> Meter {
        self.meters.lock().get(&id).copied().unwrap_or_default()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.quit.store(true, Relaxed);
    }
}

// ---------------------------------------------------------------------- runtime

enum Rt {
    None,
    Input { dev: String, rs: Option<Rs>, li: usize, ri: usize, gain: f32, delay: DelayUnit, primed: bool },
    Gen { st: GenState, idx: usize },
    Tl { track: Option<u32>, rs: Option<Rs>, sr: u32, primed: bool },
    Gain(f32),
    Width { w: f32, b: f32 },
    Util(usize),
    Eq(EqUnit),
    Filter(FilterUnit),
    Comp(CompUnit),
    Lim(LimiterUnit),
    Gate(GateUnit),
    Dly(DelayUnit),
    Harm(HarmUnit),
    Dist(DistUnit),
    Fuzz(FuzzUnit),
    Crush(CrushUnit),
    Jit(JitterUnit),
    Trem(TremUnit),
    Mix { gain: [f32; MIX_MAX], mute: [bool; MIX_MAX], master: f32 },
    Split,
    Merge,
    Out { dev: String, q: Arc<Mutex<Queue>>, status: Arc<Mutex<String>>, _h: Option<monitor::Handle>, gain: f32, mute: bool },
    Rec { id: String },
}

struct RtNode {
    id: NodeId,
    kind: Kind,
    bypass: bool,
    /// per input port: the feeding node (index in the processing order) and which of its outputs
    src: Vec<Option<(usize, u8)>>,
    out: [Vec<f32>; 2],
    /// second output (split nodes only)
    out2: [Vec<f32>; 2],
    peak: [f32; 2],
    extra: f32,
    rt: Rt,
}

struct Run {
    e: Arc<Engine>,
    rev: u64,
    rate: u32,
    nodes: Vec<RtNode>,
    caps: HashMap<String, (Arc<Capture>, Vec<usize>)>,
    master: Option<usize>,
    last_total: u64,
    last_progress: Instant,
    last_tick: Instant,
    carry: f64,
    last_publish: Instant,
    was_active: bool,
    /// the graph owns the analysers (ring) since the last rebuild
    claimed: bool,
    gens: Vec<GenParams>,
    gen_buf: Vec<f32>,
}

fn blank() -> [Vec<f32>; 2] {
    [vec![0.0; CHUNK], vec![0.0; CHUNK]]
}

impl Run {
    fn new(e: Arc<Engine>) -> Run {
        Run {
            e,
            rev: 0,
            rate: 48000,
            nodes: Vec::new(),
            caps: HashMap::new(),
            master: None,
            last_total: 0,
            last_progress: Instant::now(),
            last_tick: Instant::now(),
            carry: 0.0,
            last_publish: Instant::now(),
            was_active: false,
            claimed: false,
            gens: Vec::new(),
            gen_buf: vec![0.0; CHUNK * 2],
        }
    }

    fn main(mut self) {
        // the audio path should win against the UI
        prof::raise_thread_priority();
        let mut last_loop = Instant::now();
        while !self.e.quit.load(Relaxed) {
            if !self.e.active.load(Relaxed) {
                if self.was_active {
                    self.shutdown();
                }
                std::thread::sleep(Duration::from_millis(60));
                continue;
            }
            std::thread::sleep(Duration::from_millis(2));
            let gap = last_loop.elapsed();
            last_loop = Instant::now();
            // the engine thread itself was not scheduled for a long time: the audio will have dropped out
            if gap > Duration::from_millis(25) {
                prof::C.eng_late.fetch_add(1, Relaxed);
            }
            self.was_active = true;
            let rev = self.e.rev.load(Relaxed);
            if rev != self.rev {
                self.rev = rev;
                let g = self.e.graph_clone();
                self.rebuild(&g);
            }
            let t = Instant::now();
            self.tick(None);
            let us = t.elapsed().as_micros() as u64;
            prof::C.eng_tick_max_us.fetch_max(us, Relaxed);
            prof::C.eng_tick_sum_us.fetch_add(us, Relaxed);
            prof::C.eng_ticks.fetch_add(1, Relaxed);
        }
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.nodes.clear();
        self.caps.clear();
        self.master = None;
        bus().virt.lock().clear();
        bus().want_tracks.lock().clear();
        bus().on.store(false, Relaxed);
        bus().tl_clear();
        self.e.sh.ring_owner.compare_exchange(3, 0, Relaxed, Relaxed).ok();
        self.e.meters.lock().clear();
        self.was_active = false;
        self.claimed = false;
        self.rev = 0;
    }

    fn pick_rate(&self, g: &Graph) -> u32 {
        let pref = self.e.rate_pref.load(Relaxed);
        if pref > 0 {
            return pref;
        }
        let devs = self.e.devs.lock();
        let default_in = self.e.default_in.lock().clone();
        g.nodes
            .iter()
            .filter(|n| n.kind == Kind::Input)
            .find_map(|n| {
                let id = if n.s.is_empty() { &default_in } else { &n.s };
                devs.iter().find(|d| &d.id == id).map(|d| d.sr)
            })
            .unwrap_or(48000)
    }

    fn make(&self, n: &Node) -> RtNode {
        let fs = self.rate as f32;
        let rt = match n.kind {
            Kind::Input => Rt::Input { dev: String::new(), rs: None, li: 0, ri: 1, gain: 1.0, delay: DelayUnit::new(0.5, fs), primed: false },
            Kind::Generator => Rt::Gen { st: GenState::new(self.rate as f64), idx: 0 },
            Kind::TimelineOut => Rt::Tl { track: None, rs: None, sr: 0, primed: false },
            Kind::Eq => Rt::Eq(EqUnit::default()),
            Kind::Filter => Rt::Filter(FilterUnit::default()),
            Kind::Comp => Rt::Comp(CompUnit::default()),
            Kind::Limiter => Rt::Lim(LimiterUnit::default()),
            Kind::Gate => Rt::Gate(GateUnit::default()),
            Kind::Delay => Rt::Dly(DelayUnit::new(2.0, fs)),
            Kind::Harmonics => Rt::Harm(HarmUnit::default()),
            Kind::Distortion => Rt::Dist(DistUnit::default()),
            Kind::Fuzz => Rt::Fuzz(FuzzUnit::default()),
            Kind::Crusher => Rt::Crush(CrushUnit::default()),
            Kind::Jitter => Rt::Jit(JitterUnit::new(fs)),
            Kind::Tremolo => Rt::Trem(TremUnit::default()),
            Kind::Mixer => Rt::Mix { gain: [1.0; MIX_MAX], mute: [false; MIX_MAX], master: 1.0 },
            Kind::Split => Rt::Split,
            Kind::Merge => Rt::Merge,
            Kind::Output => {
                let q = Arc::new(Mutex::new(Queue { buf: VecDeque::new(), src_sr: self.rate }));
                // the sentinel device name makes `apply` open the stream
                Rt::Out { dev: "\u{1}".into(), q, status: Arc::new(Mutex::new(String::new())), _h: None, gain: 1.0, mute: false }
            }
            Kind::TimelineRec => Rt::Rec { id: String::new() },
            Kind::Gain => Rt::Gain(1.0),
            Kind::Stereo => Rt::Width { w: 1.0, b: 0.0 },
            Kind::Utility => Rt::Util(0),
            Kind::Analyzer => Rt::None,
        };
        let out2 = if n.kind == Kind::Split { blank() } else { [Vec::new(), Vec::new()] };
        RtNode { id: n.id, kind: n.kind, bypass: false, src: Vec::new(), out: blank(), out2, peak: [0.0; 2], extra: 0.0, rt }
    }

    /// Bring a runtime node in line with the parameters of its graph node.
    fn apply(&mut self, rn: &mut RtNode, n: &Node) {
        let fs = self.rate as f32;
        let default_in = self.e.default_in.lock().clone();
        match &mut rn.rt {
            Rt::Input { dev, rs, li, ri, gain, delay, primed } => {
                let want = if n.s.is_empty() { default_in } else { n.s.clone() };
                if *dev != want {
                    *dev = want;
                    *rs = None;
                    *primed = false;
                }
                *li = (n.param(0).round() as usize).clamp(1, 64) - 1;
                *ri = (n.param(1).round() as usize).clamp(1, 64) - 1;
                *gain = db_to_lin(n.param(2));
                delay.set_time(n.param(3), fs);
            }
            Rt::Gen { idx, .. } => *idx = (n.param(0).round() as usize).clamp(1, 8) - 1,
            Rt::Tl { track, .. } => *track = n.s.parse().ok(),
            Rt::Gain(g) => *g = db_to_lin(n.param(0)) * if n.param(1) >= 0.5 { -1.0 } else { 1.0 },
            Rt::Width { w, b } => {
                *w = n.param(0) * 0.01;
                *b = n.param(1) * 0.01;
            }
            Rt::Util(m) => *m = n.param(0).round() as usize,
            Rt::Eq(u) => u.set(&n.p, fs),
            Rt::Filter(u) => u.set(&n.p, fs),
            Rt::Comp(u) => u.set(&n.p, fs),
            Rt::Lim(u) => u.set(&n.p, fs),
            Rt::Gate(u) => u.set(&n.p, fs),
            Rt::Dly(u) => u.set(&n.p, fs),
            Rt::Harm(u) => u.set(&n.p),
            Rt::Dist(u) => u.set(&n.p, fs),
            Rt::Fuzz(u) => u.set(&n.p, fs),
            Rt::Crush(u) => u.set(&n.p),
            Rt::Jit(u) => u.set(&n.p),
            Rt::Trem(u) => u.set(&n.p, fs),
            Rt::Mix { gain, mute, master } => {
                *master = db_to_lin(n.param(1));
                for k in 0..MIX_MAX {
                    gain[k] = db_to_lin(n.param(2 + k));
                    mute[k] = n.param(2 + MIX_MAX + k) >= 0.5;
                }
            }
            Rt::Out { dev, q, status, _h, gain, mute } => {
                let want = n.s.clone();
                if *dev != want {
                    *dev = want.clone();
                    q.lock().buf.clear();
                    *status.lock() = String::new();
                    *_h = Some(monitor::start(q.clone(), Arc::new(AtomicU32::new(1.0f32.to_bits())), Some(want).filter(|d| !d.is_empty()), status.clone()));
                }
                *gain = db_to_lin(n.param(0));
                *mute = n.param(1) >= 0.5;
            }
            Rt::Rec { id } => *id = n.s.parse::<u32>().map(|t| format!("graph:{t}")).unwrap_or_default(),
            Rt::Split | Rt::Merge | Rt::None => {}
        }
    }

    fn rebuild(&mut self, g: &Graph) {
        let rate = self.pick_rate(g);
        if rate != self.rate || !self.claimed {
            // a new rate invalidates every filter, delay and resampler
            self.nodes.clear();
            self.caps.clear();
            self.rate = rate;
            self.e.rate.store(rate, Relaxed);
            self.claim_ring();
        }
        let live = g.live();
        let mut old: HashMap<NodeId, RtNode> = self.nodes.drain(..).map(|n| (n.id, n)).collect();
        let mut idx: HashMap<NodeId, usize> = HashMap::new();
        let mut nodes: Vec<RtNode> = Vec::new();
        for id in g.order().into_iter().filter(|i| live.contains(i)) {
            let Some(n) = g.node(id) else { continue };
            let mut rn = match old.remove(&id) {
                Some(o) if o.kind == n.kind => o,
                _ => self.make(n),
            };
            rn.bypass = n.bypass;
            rn.src = (0..n.inputs()).map(|p| g.edge_into(id, p as u8).and_then(|e| idx.get(&e.from).map(|&i| (i, e.from_port)))).collect();
            self.apply(&mut rn, n);
            idx.insert(id, nodes.len());
            nodes.push(rn);
        }
        drop(old);
        self.nodes = nodes;
        self.master = self.nodes.iter().position(|n| n.kind == Kind::Input);
        self.sync_devices(g);
        // record nodes that are fed from the timeline (directly or through effects): internal loopbacks
        let mut has_tl = vec![false; self.nodes.len()];
        for i in 0..self.nodes.len() {
            has_tl[i] = matches!(self.nodes[i].rt, Rt::Tl { .. }) || self.nodes[i].src.iter().flatten().any(|&(s, _)| has_tl[s]);
        }
        let loops: HashSet<String> = self.nodes.iter().enumerate().filter_map(|(i, n)| if let (Rt::Rec { id }, true) = (&n.rt, has_tl[i]) { Some(id.clone()).filter(|s| !s.is_empty()) } else { None }).collect();
        *bus().virt_loop.lock() = loops;
        let want: Vec<u32> = g.nodes.iter().filter(|n| n.kind == Kind::TimelineOut).filter_map(|n| n.s.parse().ok()).collect();
        *bus().want_tracks.lock() = want;
        bus().on.store(true, Relaxed);
        self.carry = 0.0;
        self.last_tick = Instant::now();
        let (a, b) = (self.nodes.iter().filter(|n| n.kind == Kind::Input).count(), self.nodes.len());
        *self.e.status.lock() = format!("{} Hz, {b} nodes, {a} inputs", self.rate);
    }

    /// Take over the analysers (the fixed input path is switched off while the graph is on).
    fn claim_ring(&mut self) {
        self.claimed = true;
        let sh = &self.e.sh;
        sh.ring_owner.store(3, Relaxed);
        {
            let mut r = sh.ring.lock();
            r.total = 0;
            r.l.fill(0.0);
            r.r.fill(0.0);
        }
        sh.sr.store(self.rate, Relaxed);
        sh.chans.store(2, Relaxed);
        sh.feeding_silence.store(false, Relaxed);
        *sh.status.lock() = format!("Routing graph ({} Hz)", self.rate);
    }

    /// Open the devices the input nodes need (and only those), tap the lanes they read; register the
    /// captures of the timeline record nodes.
    fn sync_devices(&mut self, g: &Graph) {
        let mut want: HashMap<String, Vec<usize>> = HashMap::new();
        for n in &self.nodes {
            if let Rt::Input { dev, li, ri, .. } = &n.rt {
                if dev.is_empty() {
                    continue;
                }
                let lanes = want.entry(dev.clone()).or_default();
                for l in [*li, *ri] {
                    if !lanes.contains(&l) {
                        lanes.push(l);
                    }
                }
            }
        }
        self.caps.retain(|d, _| want.contains_key(d));
        for (dev, lanes) in want {
            let entry = self.caps.entry(dev.clone()).or_insert_with(|| (Arc::new(Capture::start(dev.clone())), Vec::new()));
            let ch = entry.0.format().map(|f| f.1);
            // lanes past the device's channel count read the last channel (the lane list is a request, not a promise)
            let lanes: Vec<usize> = lanes.into_iter().map(|l| ch.map_or(l, |c| l.min(c.max(1) - 1))).collect();
            if entry.1 != lanes {
                entry.0.set_tap(&lanes);
                entry.1 = lanes;
            }
        }
        // timeline record nodes
        let mut virt = bus().virt.lock();
        let ids: HashSet<String> = self.nodes.iter().filter_map(|n| if let Rt::Rec { id } = &n.rt { Some(id.clone()).filter(|s| !s.is_empty()) } else { None }).collect();
        virt.retain(|k, c| ids.contains(k) && c.format().map(|f| f.0) == Some(self.rate));
        for id in ids {
            virt.entry(id.clone()).or_insert_with(|| Arc::new(Capture::virtual_input(id, self.rate)));
        }
        let _ = g;
    }

    /// One pass: take in what the sources delivered, work out how many frames to process, run the graph.
    /// `force` processes exactly that many frames (tests).
    fn tick(&mut self, force: Option<usize>) {
        let sh = self.e.sh.clone();
        if sh.ring_owner.load(Relaxed) == 0 {
            self.claim_ring();
        }
        // the old capture thread sets the rate to 0 when it exits, which may happen after we claimed the ring
        if sh.ring_owner.load(Relaxed) == 3 && sh.sr.load(Relaxed) != self.rate {
            sh.sr.store(self.rate, Relaxed);
            sh.chans.store(2, Relaxed);
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        self.gens = self.e.gens.lock().clone();

        // 1. what the devices delivered
        let mut drained: HashMap<&str, Vec<Vec<f32>>> = HashMap::new();
        let mut total_now = 0u64;
        for (dev, (cap, _)) in &self.caps {
            drained.insert(dev.as_str(), cap.tap_drain());
            if self.master.and_then(|m| if let Rt::Input { dev: d, .. } = &self.nodes[m].rt { Some(d == dev) } else { None }).unwrap_or(false) {
                total_now = cap.total_frames();
            }
        }
        for n in self.nodes.iter_mut() {
            if let Rt::Input { dev, rs, li, ri, .. } = &mut n.rt {
                let Some((cap, _)) = self.caps.get(dev) else { continue };
                if rs.is_none() {
                    if let Some((sr, _)) = cap.format() {
                        *rs = Some(Rs::new(sr, self.rate));
                    }
                }
                let (Some(rs), Some(lanes)) = (rs.as_mut(), drained.get(dev.as_str())) else { continue };
                let ch = cap.format().map_or(2, |f| f.1);
                let (a, b) = ((*li).min(ch.max(1) - 1), (*ri).min(ch.max(1) - 1));
                let (l, r) = (lanes.get(a).map(|v| v.as_slice()).unwrap_or(&[]), lanes.get(b).map(|v| v.as_slice()).unwrap_or(&[]));
                let m = l.len().min(r.len());
                rs.push(&l[..m], &r[..m]);
            }
        }
        {
            let mut tl = bus().tl.lock();
            let sr = tl.sr;
            let TlBus { master, tracks, .. } = &mut *tl;
            for n in self.nodes.iter_mut() {
                if let Rt::Tl { track, rs, sr: rsr, .. } = &mut n.rt {
                    if sr == 0 {
                        continue;
                    }
                    if rs.is_none() || *rsr != sr {
                        *rs = Some(Rs::new(sr, self.rate));
                        *rsr = sr;
                    }
                    let q = match track {
                        None => Some(&mut *master),
                        Some(id) => tracks.get_mut(id),
                    };
                    if let (Some(q), Some(rs)) = (q, rs.as_mut()) {
                        while let Some(f) = q.pop_front() {
                            rs.push_frame(f);
                        }
                    }
                }
            }
        }

        // 2. how many frames
        if total_now != self.last_total {
            self.last_total = total_now;
            self.last_progress = now;
        }
        let master_ok = self.master.is_some() && self.last_progress.elapsed() < Duration::from_millis(300);
        let n_total = if let Some(f) = force {
            f
        } else if master_ok {
            match &mut self.nodes[self.master.unwrap()].rt {
                Rt::Input { rs: Some(rs), .. } => {
                    // after a stall do not replay old audio: keep at most 0.3 s
                    rs.trim(self.rate as usize * 3 / 10);
                    rs.available()
                }
                _ => 0,
            }
        } else {
            self.carry += dt * self.rate as f64;
            let n = self.carry.floor() as usize;
            self.carry -= n as f64;
            n
        };
        prof::C.eng_block.store(n_total as u32, Relaxed);
        prof::C.eng_block_sum.fetch_add(n_total as u64, Relaxed);
        prof::C.eng_sr.store(self.rate, Relaxed);
        let clocked = master_ok && force.is_none();

        // 3. run
        let mut done = 0;
        while done < n_total {
            let n = (n_total - done).min(CHUNK);
            self.chunk(n, clocked);
            done += n;
        }

        // 4. sources that run on another clock must not pile up or run dry for long
        let safety = crate::prof::safety_ms() as usize;
        let (keep_hi, keep_lo) = (self.rate as usize * (150 + 2 * safety) / 1000, self.rate as usize * (60 + safety) / 1000);
        for (i, n) in self.nodes.iter_mut().enumerate() {
            let is_master = Some(i) == self.master && clocked;
            match &mut n.rt {
                Rt::Input { rs: Some(rs), .. } if !is_master => {
                    if rs.queued() > keep_hi * 2 {
                        rs.trim(keep_lo * 2);
                    }
                }
                Rt::Tl { rs: Some(rs), .. } => {
                    if rs.queued() > keep_hi * 2 {
                        rs.trim(keep_lo * 2);
                    }
                }
                _ => {}
            }
        }

        // 5. let the UI see levels and device messages
        if self.last_publish.elapsed() >= Duration::from_millis(40) {
            self.last_publish = Instant::now();
            let mut m = self.e.meters.lock();
            let mut st = self.e.node_status.lock();
            for n in self.nodes.iter_mut() {
                m.insert(n.id, Meter { peak: n.peak, extra: n.extra });
                n.peak = [0.0; 2];
                match &n.rt {
                    Rt::Out { status, .. } => {
                        let s = status.lock().clone();
                        if !s.is_empty() {
                            st.insert(n.id, s);
                        }
                    }
                    Rt::Input { dev, .. } => {
                        let s = self.caps.get(dev).map(|c| c.0.status.lock().clone()).unwrap_or_else(|| if dev.is_empty() { "no device chosen".into() } else { "opening...".into() });
                        st.insert(n.id, s);
                    }
                    _ => {}
                }
            }
            let ids: HashSet<NodeId> = self.nodes.iter().map(|n| n.id).collect();
            m.retain(|k, _| ids.contains(k));
        }
    }

    /// Process `n` frames through every node in order.
    fn chunk(&mut self, n: usize, clocked: bool) {
        let sh = self.e.sh.clone();
        let rate = self.rate;
        let safety = crate::prof::safety_ms() as usize;
        let prime = rate as usize * (25 + safety) / 1000;
        for i in 0..self.nodes.len() {
            let (before, rest) = self.nodes.split_at_mut(i);
            let node = &mut rest[0];
            let is_master = clocked && Some(i) == self.master;
            let inp = node.src.first().copied().flatten().map(|(s, p)| outbuf(&before[s], p));
            let [o0, o1] = &mut node.out;
            match &mut node.rt {
                Rt::Input { rs, gain, delay, primed, .. } => {
                    let got = match rs {
                        Some(rs) => pull(rs, primed, prime, is_master, &mut o0[..n], &mut o1[..n]),
                        None => 0,
                    };
                    o0[got..n].fill(0.0);
                    o1[got..n].fill(0.0);
                    delay.process(&mut o0[..n], &mut o1[..n]);
                    if (*gain - 1.0).abs() > 1e-6 {
                        o0[..n].iter_mut().for_each(|v| *v *= *gain);
                        o1[..n].iter_mut().for_each(|v| *v *= *gain);
                    }
                }
                Rt::Gen { st, idx } => {
                    match self.gens.get(*idx) {
                        Some(p) => {
                            st.render(p, &mut self.gen_buf[..n * 2], 2);
                            for k in 0..n {
                                o0[k] = self.gen_buf[2 * k];
                                o1[k] = self.gen_buf[2 * k + 1];
                            }
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                        }
                    }
                }
                Rt::Tl { rs, primed, .. } => {
                    let got = match rs {
                        Some(rs) => pull(rs, primed, prime, false, &mut o0[..n], &mut o1[..n]),
                        None => 0,
                    };
                    o0[got..n].fill(0.0);
                    o1[got..n].fill(0.0);
                }
                Rt::Mix { gain, mute, master } => {
                    o0[..n].fill(0.0);
                    o1[..n].fill(0.0);
                    for (p, s) in node.src.iter().enumerate() {
                        let Some((s, sp)) = s else { continue };
                        if mute[p.min(MIX_MAX - 1)] {
                            continue;
                        }
                        let g = gain[p.min(MIX_MAX - 1)] * *master;
                        let src = outbuf(&before[*s], *sp);
                        for k in 0..n {
                            o0[k] += src[0][k] * g;
                            o1[k] += src[1][k] * g;
                        }
                    }
                }
                Rt::Split => {
                    // out 1: the left channel on both sides, out 2: the right channel on both sides
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[0][..n]);
                            node.out2[0][..n].copy_from_slice(&b[1][..n]);
                            node.out2[1][..n].copy_from_slice(&b[1][..n]);
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                            node.out2[0][..n].fill(0.0);
                            node.out2[1][..n].fill(0.0);
                        }
                    }
                }
                Rt::Merge => {
                    // each input is reduced to mono: the first becomes the left channel, the second the right
                    for (k, dst) in [&mut *o0, &mut *o1].into_iter().enumerate() {
                        match node.src.get(k).copied().flatten().map(|(s, p)| outbuf(&before[s], p)) {
                            Some(b) => (0..n).for_each(|j| dst[j] = 0.5 * (b[0][j] + b[1][j])),
                            None => dst[..n].fill(0.0),
                        }
                    }
                }
                Rt::Out { q, gain, mute, .. } => {
                    let (a, b) = inp.map(|b| (&b[0][..n], &b[1][..n])).unwrap_or((&ZERO[..n], &ZERO[..n]));
                    if inp.is_some() && !*mute {
                        let (mut l, mut r) = (a.to_vec(), b.to_vec());
                        l.iter_mut().for_each(|v| *v *= *gain);
                        r.iter_mut().for_each(|v| *v *= *gain);
                        let mut q = q.lock();
                        q.src_sr = rate;
                        q.push(&l, &r);
                    }
                    node.peak = peak2(a, b, node.peak);
                    continue;
                }
                Rt::Rec { id } => {
                    let (a, b) = inp.map(|b| (&b[0][..n], &b[1][..n])).unwrap_or((&ZERO[..n], &ZERO[..n]));
                    if let Some(c) = bus().virt.lock().get(id.as_str()) {
                        c.feed(a, b);
                    }
                    node.peak = peak2(a, b, node.peak);
                    continue;
                }
                Rt::None if node.kind == Kind::Analyzer => {
                    let (a, b) = inp.map(|b| (&b[0][..n], &b[1][..n])).unwrap_or((&ZERO[..n], &ZERO[..n]));
                    if sh.ring_owner.load(Relaxed) == 3 {
                        let mut ring = sh.ring.lock();
                        for k in 0..n {
                            ring.push(a[k], b[k]);
                        }
                        drop(ring);
                        sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
                        sh.feeding_silence.store(false, Relaxed);
                        sh.cv.notify_one();
                    }
                    node.peak = peak2(a, b, node.peak);
                    continue;
                }
                // effects: copy the input (silence when unconnected), then process in place
                rt => {
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[1][..n]);
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                        }
                    }
                    if !node.bypass {
                        let (l, r) = (&mut o0[..n], &mut o1[..n]);
                        match rt {
                            Rt::Gain(g) => {
                                l.iter_mut().for_each(|v| *v *= *g);
                                r.iter_mut().for_each(|v| *v *= *g);
                            }
                            Rt::Width { w, b } => stereo_width(l, r, *w, *b),
                            Rt::Util(m) => utility(l, r, *m),
                            Rt::Eq(u) => u.process(l, r),
                            Rt::Filter(u) => u.process(l, r),
                            Rt::Comp(u) => {
                                u.process(l, r);
                                node.extra = u.gr_db;
                            }
                            Rt::Lim(u) => {
                                u.process(l, r);
                                node.extra = u.gr_db;
                            }
                            Rt::Gate(u) => {
                                u.process(l, r);
                                node.extra = if u.open { 1.0 } else { 0.0 };
                            }
                            Rt::Dly(u) => u.process(l, r),
                            Rt::Harm(u) => u.process(l, r),
                            Rt::Dist(u) => u.process(l, r),
                            Rt::Fuzz(u) => u.process(l, r),
                            Rt::Crush(u) => u.process(l, r),
                            Rt::Jit(u) => u.process(l, r),
                            Rt::Trem(u) => u.process(l, r),
                            _ => {}
                        }
                    }
                }
            }
            node.peak = peak2(&o0[..n], &o1[..n], node.peak);
        }
    }
}

/// Pull `l.len()` frames from a source that runs on its own clock. Such a source is first allowed to fill
/// a small cushion (`prime` frames); when it runs dry it refills before it plays again, so that short
/// hiccups become one short gap instead of a stutter. The clock source itself is taken as it comes.
fn pull(rs: &mut Rs, primed: &mut bool, prime: usize, is_clock: bool, l: &mut [f32], r: &mut [f32]) -> usize {
    if !is_clock && !*primed {
        if rs.available() < prime + l.len() {
            return 0;
        }
        *primed = true;
    }
    let got = rs.pull(l, r);
    if got < l.len() && !is_clock {
        *primed = false;
    }
    got
}

/// The buffers of output `port` of a node.
fn outbuf(n: &RtNode, port: u8) -> &[Vec<f32>; 2] {
    if port == 1 { &n.out2 } else { &n.out }
}

fn peak2(a: &[f32], b: &[f32], prev: [f32; 2]) -> [f32; 2] {
    let pa = a.iter().fold(0f32, |m, v| m.max(v.abs()));
    let pb = b.iter().fold(0f32, |m, v| m.max(v.abs()));
    [prev[0].max(pa), prev[1].max(pb)]
}

#[cfg(test)]
mod tests {
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
}
