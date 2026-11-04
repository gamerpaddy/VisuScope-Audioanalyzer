//! Runs the routing graph on its own thread. Sources (input devices, generators, the timeline, the file
//! player) are pulled block by block, effects run in dependency order and the sinks (analysers, output
//! devices, timeline tracks) are fed. The clock is the first input device (its callbacks deliver the
//! frames); without one the engine follows the wall clock.

use super::dsp::*;
use super::fx::{ConvUnit, NoiseRedUnit, PhaseUnit, ShiftUnit};
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
    /// record nodes that want a new track of their own (node id, name) and the tracks that were made for them (node id, track id)
    pub track_requests: Mutex<Vec<(u32, String)>>,
    pub node_tracks: Mutex<Vec<(u32, u32)>>,
    /// record nodes (capture ids) that receive audio from a timeline output: a loopback inside the program
    pub virt_loop: Mutex<std::collections::HashSet<String>>,
    /// device of the first output node (tools that play sound use it while the graph is on; empty = default output)
    pub first_out: Mutex<String>,
    /// where the listening probe plays: empty = the device of the first output node, PROBE_DEFAULT = the default output, else a device id
    pub probe_dev: Mutex<String>,
    /// a generator node that leads to a sink is running (tools that play a sweep need a generator to play it)
    pub gen_live: AtomicBool,
    /// bands (frequency, gain, Q) the EQ design window wants in an equaliser node, and the preamp it asks for
    pub eq_send: Mutex<Option<(Vec<[f32; 3]>, f32)>>,
    /// the graph as text (for snapshots) and a graph text a snapshot wants put back
    pub graph_text: Mutex<String>,
    pub graph_restore: Mutex<Option<String>>,
}

/// `Bus::probe_dev`: the default output device.
pub const PROBE_DEFAULT: &str = "\u{2}";

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

    /// Ask the node editor for a "Timeline track" node of this track.
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

/// What the listening probe of the node editor hears: the signal at an input port of a node (the one that feeds it) or at an output port.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Probe {
    pub node: NodeId,
    pub input: bool,
    pub port: u8,
}

pub struct Engine {
    pub graph: Mutex<Graph>,
    /// the listening probe: where it listens, its volume (linear gain, shared with the output stream) and whether the normal
    /// outputs are silent while it listens
    pub probe: Mutex<Option<Probe>>,
    pub probe_gain: Arc<AtomicU32>,
    pub probe_solo: AtomicBool,
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
            probe: Mutex::new(None),
            probe_gain: Arc::new(AtomicU32::new(0.5f32.to_bits())),
            probe_solo: AtomicBool::new(true),
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

/// An impulse response that is being read on a worker thread (a long file must not stall the audio thread).
type IrLoad = Arc<Mutex<Option<Result<(u32, Vec<Vec<f32>>), String>>>>;

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
    Dist(DistUnit),
    Phase(PhaseUnit),
    Shift(Box<ShiftUnit>),
    Conv { u: Box<ConvUnit>, path: String, loading: Option<IrLoad> },
    Nr(Box<NoiseRedUnit>),
    Fuzz(FuzzUnit),
    Crush(CrushUnit),
    Jit(JitterUnit),
    Trem(TremUnit),
    Smooth(SmoothUnit),
    Mix { gain: [f32; MIX_MAX], mute: [bool; MIX_MAX], master: f32 },
    Split,
    Merge,
    Dual,
    Out { dev: String, q: Arc<Mutex<Queue>>, status: Arc<Mutex<String>>, _h: Option<monitor::Handle>, gain: f32, mute: bool, band: BandFilter, buf: [Vec<f32>; 2], aa: FirLowpass, aa_for: u32 },
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
    /// the gain that was applied last (slot 0: input trim, gain node, output; the mixer uses one per input); NaN = not set yet
    cur: [f32; MIX_MAX],
    rt: Rt,
}

/// A gain that changes is eased over the block that follows (a step makes a click): returns the gain at the first sample and the
/// change per sample.
fn span(cur: &mut f32, tgt: f32, n: usize) -> (f32, f32) {
    if !cur.is_finite() {
        *cur = tgt;
    }
    let g0 = *cur;
    *cur = tgt;
    (g0, (tgt - g0) / n.max(1) as f32)
}

/// What the followed parameters have now (node, parameter, value), for the display.
pub static MOD_LIVE: Mutex<Vec<(NodeId, usize, f32)>> = Mutex::new(Vec::new());

/// A parameter that follows a signal, in the running graph: the destination and source (indices of the nodes), a copy of the
/// node whose parameter is set, the value it has now.
struct RtMod {
    dst: usize,
    src: usize,
    node: Node,
    m: Modulation,
    /// the value after the low-pass (what is applied)
    last: f32,
    /// a two-entry setting (on / off): the state it is in, and whether this is one
    on: Option<bool>,
    sw: bool,
}

struct Run {
    e: Arc<Engine>,
    mods: Vec<RtMod>,
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
    /// what the devices delivered since the last pass (kept between passes, see `Capture::tap_drain_into`)
    drain: HashMap<String, Vec<Vec<f32>>>,
    /// the listening probe: its own output stream (kept open for a few seconds after the last hover), the queue that feeds it,
    /// where it listened last and when it was last wanted
    probe_q: Arc<Mutex<Queue>>,
    probe_out: Option<(monitor::Handle, String)>,
    probe_last: Option<Probe>,
    probe_idle: Instant,
    probe_status: Arc<Mutex<String>>,
    probe_buf: [Vec<f32>; 2],
}

fn blank() -> [Vec<f32>; 2] {
    [vec![0.0; CHUNK], vec![0.0; CHUNK]]
}

impl Run {
    fn new(e: Arc<Engine>) -> Run {
        Run {
            e,
            mods: Vec::new(),
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
            drain: HashMap::new(),
            probe_q: Arc::new(Mutex::new(Queue::default())),
            probe_out: None,
            probe_last: None,
            probe_idle: Instant::now(),
            probe_status: Arc::new(Mutex::new(String::new())),
            probe_buf: blank(),
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
            std::thread::sleep(Duration::from_millis(3));
            let gap = last_loop.elapsed();
            last_loop = Instant::now();
            // the engine thread itself was not scheduled for a long time: the audio will have dropped out
            if gap > Duration::from_millis(25) {
                prof::C.eng_late.fetch_add(1, Relaxed);
            }
            self.was_active = true;
            let rev = self.e.rev.load(Relaxed);
            let t0 = Instant::now();
            // a panic inside the graph must not end the audio thread: drop the running state and start over
            let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if rev != self.rev {
                    self.rev = rev;
                    let g = self.e.graph_clone();
                    self.rebuild(&g);
                }
                self.tick(None);
            }));
            if guarded.is_err() {
                crate::log::log("routing graph: panic in the engine, restarting it");
                self.nodes.clear();
                self.caps.clear();
                self.claimed = false;
                self.rev = 0;
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let us = t0.elapsed().as_micros() as u64;
            prof::C.eng_tick_max_us.fetch_max(us, Relaxed);
            prof::C.eng_tick_sum_us.fetch_add(us, Relaxed);
            prof::C.eng_ticks.fetch_add(1, Relaxed);
        }
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.probe_out = None;
        self.nodes.clear();
        self.caps.clear();
        self.master = None;
        bus().virt.lock().clear();
        bus().want_tracks.lock().clear();
        bus().on.store(false, Relaxed);
        bus().gen_live.store(false, Relaxed);
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
            Kind::Distortion => Rt::Dist(DistUnit::default()),
            Kind::Phase => Rt::Phase(PhaseUnit::default()),
            Kind::Shifter => Rt::Shift(Box::new(ShiftUnit::new())),
            Kind::Convolver => Rt::Conv { u: Box::new(ConvUnit::new()), path: String::new(), loading: None },
            Kind::NoiseRed => Rt::Nr(Box::new(NoiseRedUnit::new())),
            Kind::Fuzz => Rt::Fuzz(FuzzUnit::default()),
            Kind::Crusher => Rt::Crush(CrushUnit::default()),
            Kind::Jitter => Rt::Jit(JitterUnit::new(fs)),
            Kind::Tremolo => Rt::Trem(TremUnit::default()),
            Kind::ControlLp => Rt::Smooth(SmoothUnit::default()),
            Kind::Mixer => Rt::Mix { gain: [1.0; MIX_MAX], mute: [false; MIX_MAX], master: 1.0 },
            Kind::Split => Rt::Split,
            Kind::Dual => Rt::Dual,
            Kind::Merge => Rt::Merge,
            Kind::Output => {
                let q = Arc::new(Mutex::new(Queue { buf: VecDeque::new(), src_sr: self.rate, ..Default::default() }));
                // the sentinel device name makes `apply` open the stream
                Rt::Out { dev: "\u{1}".into(), q, status: Arc::new(Mutex::new(String::new())), _h: None, gain: 1.0, mute: false, band: BandFilter::default(), buf: blank(), aa: Default::default(), aa_for: 0 }
            }
            Kind::TimelineRec => Rt::Rec { id: String::new() },
            Kind::Gain => Rt::Gain(1.0),
            Kind::Stereo => Rt::Width { w: 1.0, b: 0.0 },
            Kind::Utility => Rt::Util(0),
            Kind::Analyzer => Rt::None,
        };
        let out2 = if matches!(n.kind, Kind::Split | Kind::Dual) { blank() } else { [Vec::new(), Vec::new()] };
        RtNode { id: n.id, kind: n.kind, bypass: false, src: Vec::new(), out: blank(), out2, peak: [0.0; 2], extra: 0.0, cur: [f32::NAN; MIX_MAX], rt }
    }

    /// Bring a runtime node in line with the parameters of its graph node.
    fn apply(&mut self, rn: &mut RtNode, n: &Node) {
        let (rate, e) = (self.rate, self.e.clone());
        Self::apply_with(rate, &e, rn, n);
    }

    fn apply_with(rate: u32, e: &Arc<Engine>, rn: &mut RtNode, n: &Node) {
        let fs = rate as f32;
        let default_in = e.default_in.lock().clone();
        // parameters are limited to their ranges before they reach a filter
        let pv = n.clamped();
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
            Rt::Eq(u) => u.set(&pv, fs),
            Rt::Filter(u) => u.set(&pv, fs),
            Rt::Comp(u) => u.set(&pv, fs),
            Rt::Lim(u) => u.set(&pv, fs),
            Rt::Gate(u) => u.set(&pv, fs),
            Rt::Dly(u) => u.set(&pv, fs),
            Rt::Dist(u) => u.set(&pv, fs),
            Rt::Shift(u) => {
                u.set(&pv, fs);
                e.node_status.lock().insert(n.id, format!("delays the signal by {:.1} ms", u.latency() as f32 * 1000.0 / fs));
            }
            Rt::Phase(u) => {
                u.set(&pv, fs);
                u.set_curve(&n.s, fs);
                let ms = u.latency() as f32 * 1000.0 / fs;
                let st = if ms > 0.0 { format!("delays the signal by {ms:.1} ms") } else { String::new() };
                e.node_status.lock().insert(n.id, st);
            }
            Rt::Conv { u, path, loading } => {
                u.set(&pv);
                if *path != n.s {
                    *path = n.s.clone();
                    let st = if path.is_empty() {
                        u.loaded = false;
                        *loading = None;
                        "no impulse response chosen".to_string()
                    } else {
                        let slot: IrLoad = Arc::new(Mutex::new(None));
                        let (s2, p2) = (slot.clone(), path.clone());
                        std::thread::spawn(move || {
                            let r = std::panic::catch_unwind(|| crate::tools::decode::read_all(std::path::Path::new(p2.as_str()))).unwrap_or_else(|_| Err("the decoder failed on this file".into()));
                            *s2.lock() = Some(r);
                        });
                        *loading = Some(slot);
                        "loading the impulse response...".to_string()
                    };
                    e.node_status.lock().insert(n.id, st);
                }
            }
            Rt::Nr(u) => {
                u.set(&pv);
                let st = if u.learning { "learning the noise: keep only the noise playing".to_string() } else { format!("delays by {:.1} ms{}", u.latency() as f32 * 1000.0 / fs, if u.has_profile() { ", profile stored" } else { "" }) };
                e.node_status.lock().insert(n.id, st);
            }
            Rt::Fuzz(u) => u.set(&pv, fs),
            Rt::Crush(u) => u.set(&pv),
            Rt::Jit(u) => u.set(&pv),
            Rt::Trem(u) => u.set(&pv, fs),
            Rt::Smooth(u) => u.set(&pv, fs),
            Rt::Mix { gain, mute, master } => {
                *master = db_to_lin(n.param(1));
                for k in 0..MIX_MAX {
                    gain[k] = db_to_lin(n.param(2 + k));
                    mute[k] = n.param(2 + MIX_MAX + k) >= 0.5;
                }
            }
            Rt::Out { dev, q, status, _h, gain, mute, .. } => {
                let want = n.s.clone();
                if *dev != want {
                    *dev = want.clone();
                    q.lock().flush();
                    *status.lock() = String::new();
                    *_h = Some(monitor::start(q.clone(), Arc::new(AtomicU32::new(1.0f32.to_bits())), Some(want).filter(|d| !d.is_empty()), status.clone()));
                }
                *gain = db_to_lin(n.param(0));
                *mute = n.param(1) >= 0.5;
            }
            Rt::Rec { id } => *id = n.s.parse::<u32>().map(|t| format!("graph:{t}")).unwrap_or_default(),
            Rt::Split | Rt::Merge | Rt::Dual | Rt::None => {}
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
        let mut live = g.live();
        if let Some(p) = *self.e.probe.lock() {
            let (mut stack, mut seen) = (vec![p.node], HashSet::new());
            while let Some(n) = stack.pop() {
                if seen.insert(n) {
                    live.insert(n);
                    stack.extend(g.edges.iter().filter(|e| e.to == n).map(|e| e.from));
                }
            }
        }
        // the signals that parameters follow run as well, with everything that feeds them
        let mut stack: Vec<NodeId> = g.mods.iter().filter(|m| live.contains(&m.node)).map(|m| m.src).collect();
        while let Some(n) = stack.pop() {
            if live.insert(n) {
                stack.extend(g.edges.iter().filter(|e| e.to == n).map(|e| e.from));
            }
        }
        // a change of a setting rebuilds the running graph while the sound goes on: only a graph that starts from nothing restarts
        // the clock (resetting it on every change lost the time since the last block, and a slider that is dragged changes 60 times a second)
        let fresh = self.nodes.is_empty();
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
        self.mods = g.mods.iter().filter_map(|m| Some(RtMod { dst: *idx.get(&m.node)?, src: *idx.get(&m.src)?, node: g.node(m.node)?.clone(), m: *m, last: f32::NAN, on: None, sw: g.node(m.node)?.kind.info().params.get(m.param).is_some_and(|d| d.choices.len() == 2) })).collect();
        // the longest delay the nodes add between an input and an output (for the display of the total delay)
        {
            let fs = self.rate as f32;
            let mut dp = vec![0f32; self.nodes.len()];
            let mut total = 0f32;
            for i in 0..self.nodes.len() {
                let rn = &self.nodes[i];
                let own = if rn.bypass {
                    0.0
                } else {
                    match &rn.rt {
                        Rt::Input { .. } => g.node(rn.id).map_or(0.0, |n| n.param(3).max(0.0)),
                        Rt::Shift(u) => u.latency() as f32 * 1000.0 / fs,
                        Rt::Phase(u) => u.latency() as f32 * 1000.0 / fs,
                        Rt::Nr(u) => u.latency() as f32 * 1000.0 / fs,
                        _ => 0.0,
                    }
                };
                let up = rn.src.iter().flatten().map(|&(s, _)| dp[s]).fold(0.0, f32::max);
                dp[i] = up + own;
                if matches!(rn.rt, Rt::Out { .. }) {
                    total = total.max(dp[i]);
                }
            }
            prof::C.path_ms.store(total.to_bits(), Relaxed);
        }
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
        bus().gen_live.store(self.nodes.iter().any(|n| matches!(n.rt, Rt::Gen { .. })), Relaxed);
        bus().on.store(true, Relaxed);
        if fresh {
            self.carry = 0.0;
            self.last_tick = Instant::now();
        }
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
        // the listening probe: its stream follows the first output node's device and stays open a few seconds after the last hover
        {
            let probe = *self.e.probe.lock();
            if probe != self.probe_last {
                self.probe_q.lock().flush();
                self.probe_last = probe;
            }
            if probe.is_some() {
                self.probe_idle = Instant::now();
                let dev = match bus().probe_dev.lock().as_str() {
                    "" => bus().first_out.lock().clone(),
                    PROBE_DEFAULT => String::new(),
                    d => d.to_string(),
                };
                if self.probe_out.as_ref().is_none_or(|(_, d)| *d != dev) {
                    self.probe_q.lock().flush();
                    let h = monitor::start(self.probe_q.clone(), self.e.probe_gain.clone(), Some(dev.clone()).filter(|d| !d.is_empty()), self.probe_status.clone());
                    self.probe_out = Some((h, dev));
                }
            } else if self.probe_out.is_some() && self.probe_idle.elapsed() > Duration::from_secs(3) {
                self.probe_out = None;
            }
        }
        // impulse responses that finished loading
        for n in self.nodes.iter_mut() {
            if let Rt::Conv { u, loading, .. } = &mut n.rt {
                let done = loading.as_ref().and_then(|l| l.lock().take());
                if let Some(res) = done {
                    *loading = None;
                    let st = match res {
                        Ok((sr, ch)) => {
                            u.set_ir(&ch, sr, self.rate);
                            if u.loaded { format!("{:.2} s impulse response, delays by {:.1} ms", u.ir_secs, u.latency() as f32 * 1000.0 / self.rate as f32) } else { "the file holds no audio".to_string() }
                        }
                        Err(e) => {
                            u.loaded = false;
                            e
                        }
                    };
                    self.e.node_status.lock().insert(n.id, st);
                }
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        self.gens.clone_from(&*self.e.gens.lock());

        // 1. what the devices delivered
        let mut total_now = 0u64;
        self.drain.retain(|k, _| self.caps.contains_key(k));
        for (dev, (cap, _)) in &self.caps {
            if !self.drain.contains_key(dev) {
                self.drain.insert(dev.clone(), Vec::new());
            }
            if let Some(buf) = self.drain.get_mut(dev) {
                cap.tap_drain_into(buf);
            }
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
                let (Some(rs), Some(lanes)) = (rs.as_mut(), self.drain.get(dev.as_str())) else { continue };
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
                    rs.trim_out(self.rate as usize * 12 / 100);
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
        // (in frames of the graph rate: a source of a lower rate has fewer frames for the same time)
        let (keep_hi, keep_lo) = (self.rate as usize * (100 + 2 * safety) / 1000, self.rate as usize * (40 + safety) / 1000);
        for (i, n) in self.nodes.iter_mut().enumerate() {
            let is_master = Some(i) == self.master && clocked;
            match &mut n.rt {
                Rt::Input { rs: Some(rs), .. } if !is_master => {
                    if rs.queued_out() > keep_hi {
                        rs.trim_out(keep_lo);
                    }
                }
                Rt::Tl { rs: Some(rs), .. } => {
                    if rs.queued_out() > keep_hi {
                        rs.trim_out(keep_lo);
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
        let probe = *self.e.probe.lock();
        let probe_at = probe.and_then(|p| self.nodes.iter().position(|n| n.id == p.node).map(|i| (i, p)));
        let mute_outputs = probe_at.is_some() && self.e.probe_solo.load(Relaxed);
        if !self.mods.is_empty() {
            // the level of each followed signal over the block before (the signal is a block ahead of its parameter at most)
            let e = self.e.clone();
            for rm in self.mods.iter_mut() {
                let b = &self.nodes[rm.src].out;
                let len = n.min(b[0].len());
                let mean = if len == 0 { 0.0 } else { (0..len).map(|k| 0.5 * (b[0][k] + b[1][k])).sum::<f32>() / len as f32 };
                let v = if rm.sw {
                    // a switch: on above the upper threshold, off below the lower one, in between it stays as it was
                    let (lo, hi) = (rm.m.in_lo.min(rm.m.in_hi), rm.m.in_lo.max(rm.m.in_hi));
                    let on = if mean >= hi && mean.is_finite() { true } else if mean <= lo { false } else { rm.on.unwrap_or(mean >= 0.5 * (lo + hi)) };
                    rm.on = Some(on);
                    if on { rm.m.out_hi } else { rm.m.out_lo }
                } else {
                    rm.m.map(mean)
                };
                if rm.last.is_nan() || (v - rm.last).abs() > 1e-6 * (rm.m.out_hi - rm.m.out_lo).abs().max(1e-6) {
                    rm.last = v;
                    if let Some(slot) = rm.node.p.get_mut(rm.m.param) {
                        *slot = v;
                    }
                    Self::apply_with(rate, &e, &mut self.nodes[rm.dst], &rm.node);
                }
            }
            let mut live = MOD_LIVE.lock();
            live.clear();
            live.extend(self.mods.iter().map(|r| (r.m.node, r.m.param, r.last)));
        }
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
                    let (g0, dg) = span(&mut node.cur[0], *gain, n);
                    if (g0 - 1.0).abs() > 1e-6 || dg != 0.0 {
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
                            o0[k] *= g;
                            o1[k] *= g;
                        }
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
                        let pi = p.min(MIX_MAX - 1);
                        // (a mute or a change of the level fades over the block)
                        let tgt = if mute[pi] { 0.0 } else { gain[pi] * *master };
                        let (g0, dg) = span(&mut node.cur[pi], tgt, n);
                        if g0 == 0.0 && dg == 0.0 {
                            continue;
                        }
                        let src = outbuf(&before[*s], *sp);
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
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
                Rt::Dual => {
                    // two stereo lines, both a copy of the input
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[1][..n]);
                            node.out2[0][..n].copy_from_slice(&b[0][..n]);
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
                Rt::Out { q, gain, mute, band, buf, aa, aa_for, .. } => {
                    // scratch buffers of the node: nothing is allocated per block
                    let [bl, br] = buf;
                    let separate = node.src.len() == 2;
                    let port = |k: usize| node.src.get(k).copied().flatten().map(|(s, sp)| outbuf(&before[s], sp));
                    let have = gather_out(inp, port(0), port(1), separate, n, bl, br);
                    node.peak = peak2(&bl[..n], &br[..n], node.peak);
                    if have {
                        // (muted: the stream goes on with silence, so that the sound output does not run dry and mistake it for a gap)
                        let tgt = if *mute || mute_outputs { 0.0 } else { *gain };
                        let (g0, dg) = span(&mut node.cur[0], tgt, n);
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
                            bl[k] *= g;
                            br[k] *= g;
                        }
                        // a device of a lower rate than the graph: what is above its Nyquist frequency would fold back into the sound
                        // (49 kHz played on a 48 kHz device is heard as 1 kHz), so it is filtered first
                        let osr = q.lock().out_sr;
                        if osr > 0 && osr < rate {
                            if *aa_for != osr {
                                *aa_for = osr;
                                aa.design(rate as f32, osr as f32);
                            }
                            aa.process(&mut bl[..n], &mut br[..n]);
                        }
                        band.process(&mut bl[..n], &mut br[..n], rate as f32);
                        let mut q = q.lock();
                        q.src_sr = rate;
                        q.push(&bl[..n], &br[..n]);
                    }
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
                                let (g0, dg) = span(&mut node.cur[0], *g, n);
                                for k in 0..n {
                                    let g = g0 + dg * k as f32;
                                    l[k] *= g;
                                    r[k] *= g;
                                }
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
                            Rt::Dist(u) => u.process(l, r),
                            Rt::Phase(u) => u.process(l, r),
                            Rt::Shift(u) => u.process(l, r),
                            Rt::Conv { u, .. } => u.process(l, r),
                            Rt::Nr(u) => u.process(l, r),
                            Rt::Fuzz(u) => u.process(l, r),
                            Rt::Crush(u) => u.process(l, r),
                            Rt::Jit(u) => u.process(l, r),
                            Rt::Trem(u) => u.process(l, r),
                            Rt::Smooth(u) => u.process(l, r),
                            _ => {}
                        }
                    }
                }
            }
            node.peak = peak2(&o0[..n], &o1[..n], node.peak);
        }
        if let Some((i, p)) = probe_at {
            let node = &self.nodes[i];
            let src: Option<&[Vec<f32>; 2]> = if p.input { node.src.get(p.port as usize).copied().flatten().map(|(s, sp)| outbuf(&self.nodes[s], sp)) } else if node.kind.info().outputs > 0 { Some(outbuf(node, p.port)) } else { None };
            let [pl, pr] = &mut self.probe_buf;
            match src {
                Some(b) => {
                    pl[..n].copy_from_slice(&b[0][..n]);
                    pr[..n].copy_from_slice(&b[1][..n]);
                }
                None => {
                    pl[..n].fill(0.0);
                    pr[..n].fill(0.0);
                }
            }
            let mut q = self.probe_q.lock();
            q.src_sr = rate;
            q.push(&pl[..n], &pr[..n]);
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

/// What an output node plays: the stereo pair of its one input, or, with two inputs, each made mono (the first becomes the
/// left channel, the second the right one). Returns whether anything is connected.
fn gather_out(stereo: Option<&[Vec<f32>; 2]>, left: Option<&[Vec<f32>; 2]>, right: Option<&[Vec<f32>; 2]>, separate: bool, n: usize, bl: &mut [f32], br: &mut [f32]) -> bool {
    if separate {
        for (dst, src) in [(&mut *bl, left), (&mut *br, right)] {
            match src {
                Some(b) => (0..n).for_each(|j| dst[j] = 0.5 * (b[0][j] + b[1][j])),
                None => dst[..n].fill(0.0),
            }
        }
        return left.is_some() || right.is_some();
    }
    match stereo {
        Some(b) => {
            bl[..n].copy_from_slice(&b[0][..n]);
            br[..n].copy_from_slice(&b[1][..n]);
            true
        }
        None => {
            bl[..n].fill(0.0);
            br[..n].fill(0.0);
            false
        }
    }
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
}
