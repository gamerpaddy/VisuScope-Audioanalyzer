//! Self-test of the delay from the generator to the display, for a cable from an output to an input:
//! `VISUSCOPE_LATENCY_TEST=<channel 0|1>` switches a 1 kHz tone on, then off, and prints when the raw input, the Live trace
//! and the Avg trace followed. The settings are not saved in such a run.

use crate::app::App;
use parking_lot::Mutex;
use std::time::Instant;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// Stations along the way of the tone: 0 the generator node, 1 the callback of the output device, 2 the callback of the input
/// device, 3 the input node of the engine. For each, when the tone was first there and when it was gone (ms, see `now_ms`).
pub static ENABLED: AtomicBool = AtomicBool::new(false);
static HERE: [AtomicBool; 5] = [const { AtomicBool::new(false) }; 5];
static RISE: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static FALL: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

pub fn now_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The tone is (or is not) at station `i` now.
pub fn mark(i: usize, on: bool) {
    if ENABLED.load(Relaxed) && HERE[i].swap(on, Relaxed) != on {
        (if on { &RISE } else { &FALL })[i].store(now_ms(), Relaxed);
    }
}

#[derive(Default)]
struct State {
    on_ms: u64,
    off_ms: u64,
    t0: Option<Instant>,
    on: Option<Instant>,
    off: Option<Instant>,
    /// (ms since the start, raw input peak dB over the last 5 ms, Live dB at 1 kHz, Avg dB at 1 kHz)
    log: Vec<(f32, f32, f32, f32)>,
}

static ST: Mutex<Option<State>> = Mutex::new(None);
const WAIT: f32 = 5.0;
const HOLD: f32 = 4.0;
const TAIL: f32 = 4.0;

impl App {
    pub fn latency_test(&mut self) {
        let Some(ch) = std::env::var("VISUSCOPE_LATENCY_TEST").ok().and_then(|v| v.parse::<usize>().ok()) else { return };
        let ch = ch.min(1);
        ENABLED.store(true, Relaxed);
        now_ms();
        let mut g = ST.lock();
        let st = g.get_or_insert_with(State::default);
        let t0 = *st.t0.get_or_insert_with(Instant::now);
        let t = t0.elapsed().as_secs_f32();
        let sr = self.sr().max(1.0);
        // the raw input: the last 5 ms of the ring
        let n = (sr * 0.005) as usize;
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        self.sh.ring.lock().copy_last(&mut l, &mut r);
        let raw = 20.0 * (if ch == 0 { &l } else { &r }).iter().fold(1e-9f32, |a, b| a.max(b.abs())).log10();
        let bin = (1000.0 / (sr / self.an.n.max(1) as f32)) as usize;
        let at = |v: &Vec<f32>| v.get(bin.saturating_sub(3)..(bin + 4).min(v.len())).map_or(-200.0, |s| s.iter().copied().fold(-200.0, f32::max));
        st.log.push((t * 1000.0, raw, at(&self.an.live[ch]), at(&self.an.avg[ch])));
        if st.on.is_none() && t > WAIT {
            (self.tg.wave, self.tg.freq, self.tg.level_db, self.tg.on) = (crate::tonegen::Wave::Sine, 1000.0, [-20.0, -20.0], true);
            if self.gen_handle.is_none() {
                self.start_gen();
            }
            st.on = Some(Instant::now());
            st.on_ms = now_ms();
        }
        if st.off.is_none() && t > WAIT + HOLD {
            self.tg.on = false;
            st.off = Some(Instant::now());
            st.off_ms = now_ms();
        }
        if t > WAIT + HOLD + TAIL {
            let (on, off) = (st.on.unwrap().duration_since(t0).as_secs_f32() * 1000.0, st.off.unwrap().duration_since(t0).as_secs_f32() * 1000.0);
            // the steady values: the last second before the tone went off; the rest: the second before it came on
            let mean = |a: f32, b: f32, f: &dyn Fn(&(f32, f32, f32, f32)) -> f32| {
                let v: Vec<f32> = st.log.iter().filter(|x| x.0 >= a && x.0 < b).map(f).collect();
                v.iter().sum::<f32>() / v.len().max(1) as f32
            };
            eprintln!("latency test: sample rate {sr:.0} Hz, FFT {} ({:.0} ms window), FFT rate {:.0} Hz, average {}, UI {:.0} fps", self.an.n, self.an.n as f32 / sr * 1000.0, self.p.update_hz, self.p.avg_count, self.ui_fps);
            for (name, f) in [("raw input", (&|x: &(f32, f32, f32, f32)| x.1) as &dyn Fn(&(f32, f32, f32, f32)) -> f32), ("Live trace", &|x| x.2), ("Avg trace", &|x| x.3)] {
                let (hi, lo) = (mean(off - 1000.0, off, f), mean(on - 1000.0, on, f));
                let first = |from: f32, ok: &dyn Fn(f32) -> bool| st.log.iter().find(|x| x.0 >= from && ok(f(x))).map_or(f32::NAN, |x| x.0 - from);
                eprintln!(
                    "latency test: {name:10} rest {lo:7.1} dB, tone {hi:7.1} dB | on: seen (rest +10 dB) after {:5.0} ms, within 3 dB after {:5.0} ms, within 1 dB after {:5.0} ms | off: 3 dB down after {:5.0} ms, 20 dB down after {:5.0} ms, 40 dB down after {:5.0} ms",
                    first(on, &|v| v > lo + 10.0),
                    first(on, &|v| v > hi - 3.0),
                    first(on, &|v| v > hi - 1.0),
                    first(off, &|v| v < hi - 3.0),
                    first(off, &|v| v < hi - 20.0),
                    first(off, &|v| v < (hi - 40.0).max(lo + 3.0)),
                );
            }
            for (i, name) in ["generator node", "output device callback", "input device callback", "input node of the engine", "analyzer node (ring write)"].iter().enumerate() {
                eprintln!("latency test: {name:26} tone there {:5} ms after on, gone {:5} ms after off", RISE[i].load(Relaxed) as i64 - st.on_ms as i64, FALL[i].load(Relaxed) as i64 - st.off_ms as i64);
            }
            let first = |from: f32, ok: &dyn Fn(f32) -> bool| st.log.iter().find(|x| x.0 >= from && ok(x.1)).map_or(f32::NAN, |x| x.0 - from);
            eprintln!("latency test: ring read by the UI: above -40 dBFS {:.0} ms after on, below {:.0} ms after off; ring owner {}, graph on {}", first(on, &|v| v > -40.0), first(off, &|v| v < -40.0), self.sh.ring_owner.load(Relaxed), self.graph_on);
            std::process::exit(0);
        }
    }
}
