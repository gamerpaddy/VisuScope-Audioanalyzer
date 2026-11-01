//! Diagnostics for audio hiccups: counters that the audio paths update (plain atomics, nothing blocks),
//! a probe for how late the system wakes a sleeping thread, the Windows performance counters for DPC and
//! interrupt time, and a per-second history in which hiccups are marked with a likely cause.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct Counters {
    /// capture callback: longest gap between two calls and longest run time since the last read (microseconds)
    pub cb_gap_max_us: AtomicU64,
    pub cb_dur_max_us: AtomicU64,
    pub cb_count: AtomicU64,
    /// buffer the capture callback was last called with (frames) and the device rate
    pub cb_frames: AtomicU32,
    pub cb_sr: AtomicU32,
    /// driver reported overruns / underruns of the capture streams
    pub xruns: AtomicU32,
    /// output streams that ran dry (counted by the monitor / graph output callbacks)
    pub out_underruns: AtomicU32,
    /// routing graph: processing time of one tick, ticks, ticks that came late
    pub eng_tick_max_us: AtomicU64,
    pub eng_tick_sum_us: AtomicU64,
    pub eng_ticks: AtomicU64,
    pub eng_late: AtomicU32,
    /// frames per tick of the routing graph (last), summed over the ticks of a second, and the graph rate
    pub eng_block: AtomicU32,
    pub eng_block_sum: AtomicU64,
    pub eng_sr: AtomicU32,
}

pub static C: Counters = Counters {
    cb_gap_max_us: AtomicU64::new(0),
    cb_dur_max_us: AtomicU64::new(0),
    cb_count: AtomicU64::new(0),
    cb_frames: AtomicU32::new(0),
    cb_sr: AtomicU32::new(0),
    xruns: AtomicU32::new(0),
    out_underruns: AtomicU32::new(0),
    eng_tick_max_us: AtomicU64::new(0),
    eng_tick_sum_us: AtomicU64::new(0),
    eng_ticks: AtomicU64::new(0),
    eng_late: AtomicU32::new(0),
    eng_block: AtomicU32::new(0),
    eng_block_sum: AtomicU64::new(0),
    eng_sr: AtomicU32::new(0),
};

/// Bookkeeping a capture callback keeps for itself; `done` publishes it.
pub struct CbTimer {
    last: Option<Instant>,
    start: Instant,
}

impl CbTimer {
    pub fn new() -> Self {
        Self { last: None, start: Instant::now() }
    }

    /// Call first thing in the callback.
    pub fn begin(&mut self) {
        let now = Instant::now();
        if let Some(l) = self.last {
            C.cb_gap_max_us.fetch_max(now.duration_since(l).as_micros() as u64, Relaxed);
        }
        self.last = Some(now);
        self.start = now;
    }

    /// Call last in the callback with the frames it received.
    pub fn done(&self, frames: usize, sr: u32) {
        C.cb_dur_max_us.fetch_max(self.start.elapsed().as_micros() as u64, Relaxed);
        C.cb_count.fetch_add(1, Relaxed);
        C.cb_frames.store(frames as u32, Relaxed);
        C.cb_sr.store(sr, Relaxed);
    }
}

// ---------------------------------------------------------------------- history

/// One second of measurements.
#[derive(Clone, Copy, Default)]
pub struct Sec {
    /// seconds since the profiler started
    pub t: f32,
    /// worst delay of a 1 ms sleep (milliseconds)
    pub wake_max_ms: f32,
    /// sleeps that returned more than 2 / 5 / 10 ms late
    pub late2: u32,
    pub late5: u32,
    pub late10: u32,
    pub cb_gap_max_ms: f32,
    pub cb_dur_max_ms: f32,
    pub callbacks: u32,
    pub eng_tick_max_ms: f32,
    pub eng_tick_avg_ms: f32,
    /// average frames per tick of the routing engine and what that is in milliseconds
    pub eng_block_frames: f32,
    pub eng_block_ms: f32,
    /// system wide, percent (None when the counters are not available)
    pub dpc: f32,
    pub dpc_worst: f32,
    pub intr: f32,
    pub cpu: f32,
    pub dpc_rate: f32,
    pub int_rate: f32,
    pub ctx_rate: f32,
    pub queue: f32,
    pub hiccup: bool,
}

#[derive(Clone)]
pub struct Event {
    pub t: f32,
    pub what: String,
    pub cause: String,
    pub severe: bool,
}

#[derive(Default)]
pub struct State {
    pub secs: VecDeque<Sec>,
    pub events: VecDeque<Event>,
    pub counters_ok: bool,
    pub total_hiccups: u32,
    pub cpu_name: String,
}

/// Safety buffer in milliseconds (Options): extra audio the outputs and the routing graph hold back so that short
/// stalls of the system do not become gaps in the sound. Costs the same amount of delay.
static SAFETY_MS: AtomicU32 = AtomicU32::new(0);

/// A test or screenshot run (set by the environment hooks): nothing may play sound on the machine.
pub fn test_mode() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| ["VISUSCOPE_SHOT", "VISUSCOPE_GRAPH", "VISUSCOPE_LAYOUT", "VISUSCOPE_NO_SOUND", "VISUSCOPE_DAW_DEMO"].iter().any(|k| std::env::var_os(k).is_some()))
}

pub fn safety_ms() -> u32 {
    SAFETY_MS.load(Relaxed)
}

pub fn set_safety_ms(ms: u32) {
    SAFETY_MS.store(ms.min(1000), Relaxed);
}

pub fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

static STARTED: AtomicBool = AtomicBool::new(false);
static WAKE_MAX_US: AtomicU64 = AtomicU64::new(0);
static LATE2: AtomicU32 = AtomicU32::new(0);
static LATE5: AtomicU32 = AtomicU32::new(0);
static LATE10: AtomicU32 = AtomicU32::new(0);

/// Start the probes (once).
pub fn start() {
    if STARTED.swap(true, Relaxed) {
        return;
    }
    std::thread::Builder::new()
        .name("prof-wake".into())
        .spawn(|| loop {
            // how late does the system bring back a thread that sleeps for 1 ms?
            let t = Instant::now();
            std::thread::sleep(Duration::from_millis(1));
            let over = t.elapsed().saturating_sub(Duration::from_millis(1)).as_micros() as u64;
            WAKE_MAX_US.fetch_max(over, Relaxed);
            if over > 2000 {
                LATE2.fetch_add(1, Relaxed);
            }
            if over > 5000 {
                LATE5.fetch_add(1, Relaxed);
            }
            if over > 10000 {
                LATE10.fetch_add(1, Relaxed);
            }
        })
        .ok();
    std::thread::Builder::new().name("prof-sample".into()).spawn(sample_loop).ok();
}

fn sample_loop() {
    let t0 = Instant::now();
    let mut sys = sys::Counters::open();
    state().lock().counters_ok = sys.is_some();
    let (mut last_xr, mut last_uf, mut last_late) = (0u32, 0u32, 0u32);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut s = Sec { t: t0.elapsed().as_secs_f32(), ..Default::default() };
        s.wake_max_ms = WAKE_MAX_US.swap(0, Relaxed) as f32 / 1000.0;
        s.late2 = LATE2.swap(0, Relaxed);
        s.late5 = LATE5.swap(0, Relaxed);
        s.late10 = LATE10.swap(0, Relaxed);
        s.cb_gap_max_ms = C.cb_gap_max_us.swap(0, Relaxed) as f32 / 1000.0;
        s.cb_dur_max_ms = C.cb_dur_max_us.swap(0, Relaxed) as f32 / 1000.0;
        s.callbacks = C.cb_count.swap(0, Relaxed) as u32;
        let ticks = C.eng_ticks.swap(0, Relaxed).max(1);
        s.eng_tick_max_ms = C.eng_tick_max_us.swap(0, Relaxed) as f32 / 1000.0;
        s.eng_tick_avg_ms = C.eng_tick_sum_us.swap(0, Relaxed) as f32 / ticks as f32 / 1000.0;
        s.eng_block_frames = C.eng_block_sum.swap(0, Relaxed) as f32 / ticks as f32;
        let esr = C.eng_sr.load(Relaxed);
        s.eng_block_ms = if esr > 0 { s.eng_block_frames * 1000.0 / esr as f32 } else { 0.0 };
        if let Some(c) = sys.as_mut() {
            if let Some(v) = c.read() {
                s.dpc = v.dpc;
                s.dpc_worst = v.dpc_worst;
                s.intr = v.intr;
                s.cpu = v.cpu;
                s.dpc_rate = v.dpc_rate;
                s.int_rate = v.int_rate;
                s.ctx_rate = v.ctx_rate;
                s.queue = v.queue;
            }
        }
        // hiccups: something that would be heard
        let (xr, uf, late) = (C.xruns.load(Relaxed), C.out_underruns.load(Relaxed), C.eng_late.load(Relaxed));
        let (d_xr, d_uf, d_late) = (xr.wrapping_sub(last_xr), uf.wrapping_sub(last_uf), late.wrapping_sub(last_late));
        (last_xr, last_uf, last_late) = (xr, uf, late);
        let period_ms = {
            let (f, sr) = (C.cb_frames.load(Relaxed), C.cb_sr.load(Relaxed));
            if sr > 0 { f as f32 * 1000.0 / sr as f32 } else { 0.0 }
        };
        let gap_bad = period_ms > 0.0 && s.cb_gap_max_ms > period_ms * 2.5 + 4.0;
        let mut what: Vec<String> = Vec::new();
        if d_xr > 0 {
            what.push(format!("driver reported {d_xr} overrun(s) of the input"));
        }
        if d_uf > 0 {
            what.push(format!("an output ran out of audio {d_uf}x"));
        }
        if gap_bad {
            what.push(format!("input callback came {:.0} ms late (buffer {:.0} ms)", s.cb_gap_max_ms, period_ms));
        }
        if d_late > 0 {
            what.push(format!("the routing engine was not scheduled for >25 ms ({d_late}x)"));
        }
        if s.wake_max_ms > 8.0 {
            what.push(format!("the system woke a thread {:.0} ms late", s.wake_max_ms));
        }
        // the first seconds (devices opening, windows being created) are not what the user wants to hear about
        if !what.is_empty() && s.t > 6.0 {
            s.hiccup = true;
            let mut cause = diagnose(&s, d_late > 0 || gap_bad);
            if d_uf > 0 || gap_bad {
                cause.0.push_str(" Raising the safety buffer in Options adds delay but hides short stalls.");
            }
            let mut st = state().lock();
            st.total_hiccups += 1;
            st.events.push_front(Event { t: s.t, what: what.join("; "), cause: cause.0, severe: cause.1 });
            st.events.truncate(200);
        }
        let mut st = state().lock();
        st.secs.push_back(s);
        while st.secs.len() > 600 {
            st.secs.pop_front();
        }
    }
}

/// What most likely caused a hiccup, from the counters of that second. The second value marks a clear culprit.
fn diagnose(s: &Sec, audio_late: bool) -> (String, bool) {
    if s.dpc_worst >= 8.0 || s.dpc >= 4.0 {
        return (format!("Kernel DPC time was {:.1} % ({:.1} % on the worst core): a driver (network, graphics, USB / audio interface, antivirus filter) kept the CPU. Update or disable the culprit; LatencyMon names it.", s.dpc, s.dpc_worst), true);
    }
    if s.intr >= 5.0 {
        return (format!("Hardware interrupts took {:.1} % of the CPU: a device is flooding the system (USB hub, Wi-Fi / Bluetooth, a failing cable).", s.intr), true);
    }
    if s.cpu >= 92.0 {
        return (format!("The CPU was at {:.0} %: too much to do. Lower the FFT size or rate, close windows, or raise the audio buffer.", s.cpu), true);
    }
    if s.queue > 6.0 {
        return (format!("{:.0} threads were waiting for a core: another program is hogging the CPU.", s.queue), true);
    }
    if s.wake_max_ms > 8.0 && s.cpu < 60.0 {
        return ("The scheduler woke threads late although the CPU was idle: power saving (use the High performance power plan, disable USB selective suspend and core parking), or timer resolution.".into(), false);
    }
    if audio_late {
        return ("No counter stands out. Likely a driver that blocks briefly (check the audio interface driver and buffer size) or a stalled disk / network access.".into(), false);
    }
    ("A short delay with no obvious cause in the counters.".into(), false)
}

pub fn clear() {
    let mut st = state().lock();
    st.events.clear();
    st.total_hiccups = 0;
    st.secs.iter_mut().for_each(|s| s.hiccup = false);
}

/// Raise the priority of the calling thread a little (audio work should win against the UI).
pub fn raise_thread_priority() {
    sys::raise();
}

// ---------------------------------------------------------------------- Windows performance counters

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::System::Performance::*;
    type H = *mut std::ffi::c_void;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL};

    pub fn raise() {
        // SAFETY: plain calls on the current thread
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }
    }

    #[derive(Default, Clone, Copy)]
    pub struct Values {
        pub dpc: f32,
        pub dpc_worst: f32,
        pub intr: f32,
        pub cpu: f32,
        pub dpc_rate: f32,
        pub int_rate: f32,
        pub ctx_rate: f32,
        pub queue: f32,
    }

    pub struct Counters {
        q: H,
        dpc: H,
        dpc_all: H,
        intr: H,
        cpu: H,
        dpc_rate: H,
        int_rate: H,
        ctx: H,
        queue: H,
        primed: bool,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl Counters {
        pub fn open() -> Option<Counters> {
            // SAFETY: documented PDH calls with valid pointers; handles are only used by this thread
            unsafe {
                let mut q: H = std::ptr::null_mut();
                if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                    return None;
                }
                let add = |path: &str| -> Option<H> {
                    let mut h: H = std::ptr::null_mut();
                    let p = wide(path);
                    if PdhAddEnglishCounterW(q, p.as_ptr(), 0, &mut h) == 0 { Some(h) } else { None }
                };
                let c = Counters {
                    q,
                    dpc: add("\\Processor Information(_Total)\\% DPC Time")?,
                    dpc_all: add("\\Processor Information(*)\\% DPC Time").unwrap_or(std::ptr::null_mut()),
                    intr: add("\\Processor Information(_Total)\\% Interrupt Time")?,
                    cpu: add("\\Processor Information(_Total)\\% Processor Time")?,
                    dpc_rate: add("\\Processor Information(_Total)\\DPCs Queued/sec").unwrap_or(std::ptr::null_mut()),
                    int_rate: add("\\Processor Information(_Total)\\Interrupts/sec").unwrap_or(std::ptr::null_mut()),
                    ctx: add("\\System\\Context Switches/sec").unwrap_or(std::ptr::null_mut()),
                    queue: add("\\System\\Processor Queue Length").unwrap_or(std::ptr::null_mut()),
                    primed: false,
                };
                PdhCollectQueryData(q);
                Some(c)
            }
        }

        unsafe fn value(&self, h: H) -> Option<f32> {
            if h.is_null() {
                return None;
            }
            let mut v: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
            let r = unsafe { PdhGetFormattedCounterValue(h, PDH_FMT_DOUBLE, std::ptr::null_mut(), &mut v) };
            if r != 0 || v.CStatus != 0 {
                return None;
            }
            Some(unsafe { v.Anonymous.doubleValue } as f32)
        }

        /// The highest value among the instances of a wildcard counter (the busiest core).
        unsafe fn worst(&self, h: H) -> Option<f32> {
            if h.is_null() {
                return None;
            }
            let (mut bytes, mut count) = (0u32, 0u32);
            let r = unsafe { PdhGetFormattedCounterArrayW(h, PDH_FMT_DOUBLE, &mut bytes, &mut count, std::ptr::null_mut()) };
            if r != PDH_MORE_DATA || bytes == 0 {
                return None;
            }
            let mut buf = vec![0u8; bytes as usize + 64];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if unsafe { PdhGetFormattedCounterArrayW(h, PDH_FMT_DOUBLE, &mut bytes, &mut count, items) } != 0 {
                return None;
            }
            let mut m = 0f32;
            for i in 0..count as usize {
                let it = unsafe { &*items.add(i) };
                if it.FmtValue.CStatus == 0 {
                    m = m.max(unsafe { it.FmtValue.Anonymous.doubleValue } as f32);
                }
            }
            Some(m)
        }

        pub fn read(&mut self) -> Option<Values> {
            // SAFETY: handles from `open`, called from the sampling thread only
            unsafe {
                if PdhCollectQueryData(self.q) != 0 {
                    return None;
                }
                if !self.primed {
                    self.primed = true;
                    return None;
                }
                let g = |h| self.value(h).unwrap_or(0.0);
                Some(Values {
                    dpc: g(self.dpc),
                    dpc_worst: self.worst(self.dpc_all).unwrap_or_else(|| g(self.dpc)),
                    intr: g(self.intr),
                    cpu: g(self.cpu),
                    dpc_rate: g(self.dpc_rate),
                    int_rate: g(self.int_rate),
                    ctx_rate: g(self.ctx),
                    queue: g(self.queue),
                })
            }
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            // SAFETY: closes the query opened in `open`
            unsafe {
                PdhCloseQuery(self.q);
            }
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn raise() {}
    pub struct Values {
        pub dpc: f32,
        pub dpc_worst: f32,
        pub intr: f32,
        pub cpu: f32,
        pub dpc_rate: f32,
        pub int_rate: f32,
        pub ctx_rate: f32,
        pub queue: f32,
    }
    pub struct Counters;
    impl Counters {
        pub fn open() -> Option<Counters> {
            None
        }
        pub fn read(&mut self) -> Option<Values> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnosis_names_the_culprit() {
        let base = Sec { cpu: 20.0, ..Default::default() };
        assert!(diagnose(&Sec { dpc_worst: 15.0, ..base }, true).0.contains("DPC"));
        assert!(diagnose(&Sec { intr: 9.0, ..base }, true).0.contains("interrupts"));
        assert!(diagnose(&Sec { cpu: 97.0, ..base }, true).0.contains("CPU"));
        let idle = diagnose(&Sec { wake_max_ms: 30.0, ..base }, false);
        assert!(idle.0.contains("power") && !idle.1);
    }

    #[test]
    fn counters_can_be_read_on_this_machine() {
        // not every machine has the counters: only require that opening and reading never panics
        #[cfg(windows)]
        if let Some(mut c) = sys::Counters::open() {
            let _ = c.read();
            std::thread::sleep(Duration::from_millis(1100));
            if let Some(v) = c.read() {
                assert!(v.cpu >= 0.0 && v.cpu <= 100.5 && v.dpc >= 0.0 && v.intr >= 0.0, "cpu {} dpc {} int {}", v.cpu, v.dpc, v.intr);
            }
        }
    }
}
