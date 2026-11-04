//! Sound output for the file player: a small output stream fed from a sample queue, with
//! linear-interpolation resampling so any file rate plays on any device rate.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
pub struct Queue {
    pub buf: VecDeque<(f32, f32)>,
    pub src_sr: u32,
    /// the sample rate of the device that plays it (0 until the stream is up): a source of a higher rate is filtered to it
    pub out_sr: u32,
    /// the queue was emptied on purpose (a seek): the output starts afresh and does not take the pause for a hiccup
    pub flushed: bool,
}

impl Queue {
    /// Empty the queue on purpose (the position jumped).
    pub fn flush(&mut self) {
        self.buf.clear();
        self.flushed = true;
    }

    /// Append samples; if the output falls far behind (clock drift) the oldest ones are dropped.
    pub fn push(&mut self, l: &[f32], r: &[f32]) {
        self.buf.extend(l.iter().copied().zip(r.iter().copied()));
        // the backlog that is tolerated grows with the safety buffer (the reserve of an output is 150 ms at most)
        let cap = (self.src_sr as usize * (250 + 2 * crate::prof::safety_ms() as usize) / 1000).max(4096);
        if self.buf.len() > cap {
            let drop = self.buf.len() - cap * 7 / 10;
            self.buf.drain(..drop);
        }
    }
}

/// How much sound an output keeps in reserve: the length of the queue that is left after every callback has taken what it needs
/// (the margin against a source that is late). It starts at a moderate value and learns: a gap makes it larger at once and
/// remembers that the smaller value was not enough; after a calm stretch in which the queue really never came near it, a smaller
/// one is tried, never below the largest value that failed. The result is the least delay this system and these sources can do
/// without hiccups.
pub struct Cushion {
    cur: f32,
    floor: f32,
    calm: f32,
}

impl Cushion {
    pub const START_MS: f32 = 20.0;
    pub const FLOOR_MS: f32 = 6.0;
    pub const MAX_MS: f32 = 150.0;
    /// seconds without a gap before a smaller cushion is tried
    const CALM_SECS: f32 = 12.0;

    pub fn new() -> Self {
        Self { cur: Self::START_MS, floor: Self::FLOOR_MS, calm: 0.0 }
    }

    pub fn ms(&self) -> f32 {
        self.cur
    }

    /// (reserve, smallest value that may be tried) for keeping what was learned from one stream to the next.
    pub fn state(&self) -> (f32, f32) {
        (self.cur, self.floor)
    }

    pub fn from_state(s: (f32, f32)) -> Self {
        let floor = s.1.clamp(Self::FLOOR_MS, Self::MAX_MS);
        Self { cur: s.0.clamp(floor, Self::MAX_MS), floor, calm: 0.0 }
    }

    pub fn on_gap(&mut self) {
        self.floor = (self.cur + 3.0).min(Self::MAX_MS);
        self.cur = (self.cur * 1.3 + 4.0).clamp(self.floor, Self::MAX_MS);
        self.calm = 0.0;
    }

    /// `secs` of sound were played without a gap; `margin_ms` is the least that was left in the queue meanwhile.
    pub fn on_calm(&mut self, secs: f32, margin_ms: f32) {
        self.calm += secs;
        if self.calm >= Self::CALM_SECS {
            self.calm = 0.0;
            // only when the queue stayed clearly above the smaller value: the sources are steadier than the cushion needs
            if margin_ms >= self.cur + 2.0 {
                self.cur = (self.cur - 2.0).max(self.floor);
            }
        }
    }
}

/// What each output device has taught the reserve: a new stream (the timeline starts playing again) begins with it.
static LEARNED: Mutex<Vec<(String, (f32, f32))>> = Mutex::new(Vec::new());

fn learned_get(key: &str) -> Option<(f32, f32)> {
    LEARNED.lock().iter().find(|(k, _)| k == key).map(|(_, s)| *s)
}

fn learned_put(key: &str, s: (f32, f32)) {
    // from the audio callback: if the list is busy the value is stored the next time
    if let Some(mut l) = LEARNED.try_lock() {
        match l.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = s,
            None => l.push((key.to_string(), s)),
        }
    }
}

/// What an output does with the next callback.
#[derive(Debug, PartialEq)]
pub enum Act {
    /// not enough sound queued yet: play silence
    Wait,
    /// play; `fast` = play 0.5 % faster (inaudible) because more is queued than the cushion needs
    Play { fast: bool },
}

/// The control of one output: when to start, when to catch up, what the cushion should be.
pub struct OutCtl {
    pub cush: Cushion,
    primed: bool,
    /// a source that ran dry and came back within a quarter of a second had a hiccup; one that stays away has stopped (the
    /// timeline was stopped): only the first teaches the cushion
    gap: Option<f32>,
    win_min: f64,
    win_t: f32,
    surplus: bool,
    /// how much of a backlog that stayed all second long is to be skipped (ms), asked for by the next callback
    skip_ms: f32,
}

impl OutCtl {
    pub fn new() -> Self {
        Self { cush: Cushion::new(), primed: false, gap: None, win_min: f64::MAX, win_t: 0.0, surplus: false, skip_ms: 0.0 }
    }

    /// `len` frames are queued, the callback needs `need` of them (both at the source rate `sr`) and lasts `dt` seconds.
    pub fn begin(&mut self, len: usize, need: f64, sr: f64, dt: f32, safety_ms: f32) -> Act {
        let want = sr * (self.cush.ms() + safety_ms) as f64 / 1000.0;
        if !self.primed {
            if let Some(g) = self.gap.as_mut() {
                *g += dt;
                if *g > 0.25 {
                    self.gap = None;
                }
            }
            if (len as f64) < (want + need).max(64.0) {
                return Act::Wait;
            }
            self.primed = true;
            self.win_min = f64::MAX;
            self.win_t = 0.0;
            self.surplus = false;
            if self.gap.take().is_some() {
                self.cush.on_gap();
            }
        }
        // the queue after this callback: the reserve
        self.win_min = self.win_min.min(len as f64 - need);
        self.win_t += dt;
        if self.win_t >= 1.0 {
            let min_ms = (self.win_min / sr.max(1.0) * 1000.0) as f32;
            self.cush.on_calm(self.win_t, min_ms - safety_ms);
            // more in reserve than wanted, all second long: catch up slowly
            self.surplus = min_ms > self.cush.ms() + safety_ms + 4.0;
            // far more than needed, all second long (a burst, a stall that was caught up): playing it away at 0.5 % would take
            // minutes of too much delay. Skip most of it at once.
            if min_ms > self.cush.ms() + safety_ms + 50.0 {
                self.skip_ms = min_ms - self.cush.ms() - safety_ms - 10.0;
            }
            self.win_min = f64::MAX;
            self.win_t = 0.0;
        }
        Act::Play { fast: self.surplus }
    }

    /// A backlog to skip now (ms), once.
    pub fn take_skip(&mut self) -> f32 {
        std::mem::take(&mut self.skip_ms)
    }

    /// The source jumped: start afresh, without learning from the pause.
    pub fn restart(&mut self) {
        self.primed = false;
        self.gap = None;
        self.surplus = false;
    }

    /// The queue ran empty in the middle of a callback.
    pub fn underrun(&mut self) {
        self.primed = false;
        self.gap = Some(0.0);
        self.surplus = false;
    }

    /// The reserve in ms (for the profiler).
    pub fn reserve_ms(&self, safety_ms: f32) -> f32 {
        self.cush.ms() + safety_ms
    }
}

pub struct Handle {
    stop: Arc<AtomicBool>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

pub fn start(queue: Arc<Mutex<Queue>>, gain: Arc<AtomicU32>, dev_id: Option<String>, status: Arc<Mutex<String>>) -> Handle {
    let stop = Arc::new(AtomicBool::new(false));
    // screenshot and test runs never open an output device (nobody wants a test tone in the headphones)
    if crate::prof::test_mode() {
        *status.lock() = "test run: no sound output".into();
        return Handle { stop };
    }
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("monitor-out".into())
        .spawn(move || {
            while !stop2.load(Relaxed) {
                let dead = Arc::new(AtomicBool::new(false));
                match build(&queue, &gain, dev_id.as_deref(), dead.clone()) {
                    Ok((stream, info)) => {
                        *status.lock() = info;
                        while !stop2.load(Relaxed) && !dead.load(Relaxed) {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if dead.load(Relaxed) && !stop2.load(Relaxed) {
                            // an invalidated stream can hang when dropped: leak it, like the other streams
                            std::mem::forget(stream);
                            *status.lock() = "output changed, reconnecting...".into();
                            std::thread::sleep(Duration::from_millis(500));
                        } else {
                            dead.store(true, Relaxed);
                            drop(stream);
                        }
                    }
                    Err(e) => {
                        crate::log::log(&format!("monitor: build failed: {e}"));
                        *status.lock() = format!("{e}");
                        for _ in 0..20 {
                            if stop2.load(Relaxed) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        })
        .ok();
    Handle { stop }
}

fn build(q: &Arc<Mutex<Queue>>, gain: &Arc<AtomicU32>, id: Option<&str>, dead: Arc<AtomicBool>) -> Result<(cpal::Stream, String), String> {
    let host = cpal::default_host();
    let dev = match id {
        Some(id) if !id.is_empty() => {
            let did: cpal::DeviceId = id.parse().map_err(|e| format!("bad device id: {e:?}"))?;
            host.device_by_id(&did).ok_or_else(|| "output device not found".to_string())?
        }
        _ => host.default_output_device().ok_or_else(|| "no output device".to_string())?,
    };
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let (fmt, sr, ch) = (cfg.sample_format(), cfg.sample_rate(), cfg.channels());
    let sc: cpal::StreamConfig = cfg.into();
    let (q, g, d) = (q.clone(), gain.clone(), dead);
    let stream = match fmt {
        SampleFormat::F32 => make::<f32>(&dev, sc, q, g, sr, d),
        SampleFormat::F64 => make::<f64>(&dev, sc, q, g, sr, d),
        SampleFormat::I8 => make::<i8>(&dev, sc, q, g, sr, d),
        SampleFormat::I16 => make::<i16>(&dev, sc, q, g, sr, d),
        SampleFormat::I24 => make::<cpal::I24>(&dev, sc, q, g, sr, d),
        SampleFormat::I32 => make::<i32>(&dev, sc, q, g, sr, d),
        SampleFormat::U8 => make::<u8>(&dev, sc, q, g, sr, d),
        SampleFormat::U16 => make::<u16>(&dev, sc, q, g, sr, d),
        SampleFormat::U32 => make::<u32>(&dev, sc, q, g, sr, d),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, format!("{sr} Hz, {ch} ch")))
}

fn make<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, q: Arc<Mutex<Queue>>, gain: Arc<AtomicU32>, out_sr: u32, dead: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ch = (cfg.channels as usize).max(1);
    // (the source learns the rate of the device, to filter what the device cannot play)
    q.lock().out_sr = out_sr;
    let (mut prev, mut next) = ((0f32, 0f32), (0f32, 0f32));
    let mut frac = 1.0f64;
    let key = dev.id().map(|i| i.to_string()).unwrap_or_default();
    let mut ctl = OutCtl::new();
    // the last sample played, and a crossfade (frames left) after a backlog was skipped
    let (mut hold, mut fade, mut fade_from, mut fade_len) = ((0f32, 0f32), 0usize, (0f32, 0f32), 1usize);
    if let Some(st) = learned_get(&key) {
        ctl.cush = Cushion::from_state(st);
    }
    let mut stored = ctl.cush.state();
    let dead_cb = dead.clone();
    dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &_| {
            let silence = T::from_sample(0.0);
            data.iter_mut().for_each(|d| *d = silence);
            if dead_cb.load(Relaxed) {
                return;
            }
            let g = f32::from_bits(gain.load(Relaxed));
            let safety_ms = crate::prof::safety_ms() as usize;
            // (the engine holds the queue for a few microseconds while it adds a block: wait for it rather than play silence)
            let Some(mut q) = q.try_lock_for(Duration::from_micros(800)) else { return };
            if std::mem::take(&mut q.flushed) {
                ctl.restart();
            }
            let mut step = q.src_sr.max(1) as f64 / out_sr as f64;
            let frames = data.len() / ch;
            let need = frames as f64 * step;
            let dt = frames as f32 / out_sr.max(1) as f32;
            match ctl.begin(q.buf.len(), need, q.src_sr.max(1) as f64, dt, safety_ms as f32) {
                Act::Wait => return,
                // more is queued than the reserve needs: play slightly faster (0.5 %, inaudible) until the surplus is gone
                Act::Play { fast } => {
                    if fast {
                        step *= 1.005;
                    }
                }
            }
            crate::prof::C.out_cushion_ms.store(ctl.reserve_ms(safety_ms as f32) as u32, Relaxed);
            let skip = ctl.take_skip();
            if skip > 0.0 {
                let drop = ((skip as f64 / 1000.0 * q.src_sr as f64) as usize).min(q.buf.len().saturating_sub(need as usize * 2));
                q.buf.drain(..drop);
                // the cut is crossfaded from what was playing (2 ms)
                fade = (out_sr as usize * 2 / 1000).max(8);
                fade_from = hold;
                crate::prof::C.out_skips.fetch_add(1, Relaxed);
            }
            crate::prof::C.out_queue_ms.store((q.buf.len() as f64 / q.src_sr.max(1) as f64 * 1000.0) as u32, Relaxed);
            if ctl.cush.state() != stored {
                stored = ctl.cush.state();
                learned_put(&key, stored);
            }
            for frame in data.chunks_mut(ch) {
                while frac >= 1.0 {
                    prev = next;
                    match q.buf.pop_front() {
                        Some(s) => next = s,
                        None => {
                            // the source did not deliver in time: a gap in the sound
                            ctl.underrun();
                            crate::prof::C.out_underruns.fetch_add(1, Relaxed);
                            return;
                        }
                    }
                    frac -= 1.0;
                }
                let t = frac as f32;
                let mut l = (prev.0 + (next.0 - prev.0) * t) * g;
                let mut r = (prev.1 + (next.1 - prev.1) * t) * g;
                if fade > 0 {
                    if fade_len == 1 {
                        fade_len = fade;
                    }
                    let a = 1.0 - fade as f32 / fade_len as f32;
                    l = fade_from.0 * (1.0 - a) + l * a;
                    r = fade_from.1 * (1.0 - a) + r * a;
                    fade -= 1;
                    if fade == 0 {
                        fade_len = 1;
                    }
                }
                hold = (l, r);
                frac += step;
                if ch == 1 {
                    frame[0] = T::from_sample(0.5 * (l + r));
                } else {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                }
            }
        },
        move |e| match e.kind() {
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceBusy => {}
            _ => {
                crate::log::log(&format!("monitor: stream error {:?}: {e}", e.kind()));
                dead.store(true, Relaxed)
            }
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays `secs` of virtual time: a source that delivers `chunk_ms` of sound every `chunk_ms` (+ random delay up to `jitter_ms`),
    /// an output that asks for `cb_ms` every `cb_ms`, optionally one stall of the source. Returns the gaps after the first
    /// `warm` seconds, the gaps in all, and the delay (reserve + callback) at the end.
    fn simulate(chunk_ms: f64, jitter_ms: f64, cb_ms: f64, secs: f64, stall: Option<(f64, f64)>, warm: f64) -> (u32, u32, f64) {
        let sr = 48000.0;
        let mut ctl = OutCtl::new();
        let mut seed = 0x9E37_79B9u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f64 / u32::MAX as f64
        };
        let (mut len, mut t, mut next_chunk, mut gaps, mut late_gaps) = (0.0f64, 0.0f64, 0.0f64, 0u32, 0u32);
        let mut last_delay = 0.0;
        let mut due = 0.0;
        while t < secs {
            // the source delivers what is due (a stall holds it back)
            loop {
                if let Some((a, d)) = stall {
                    if next_chunk >= a && next_chunk < a + d {
                        next_chunk = a + d;
                    }
                }
                if due == 0.0 {
                    due = next_chunk + rnd() * jitter_ms / 1000.0;
                }
                if due > t {
                    break;
                }
                len += chunk_ms / 1000.0 * sr;
                next_chunk += chunk_ms / 1000.0;
                due = 0.0;
            }
            let need = cb_ms / 1000.0 * sr;
            match ctl.begin(len as usize, need, sr, (cb_ms / 1000.0) as f32, 0.0) {
                Act::Wait => {}
                Act::Play { fast } => {
                    let take = if fast { need * 1.005 } else { need };
                    if len < take {
                        len = 0.0;
                        ctl.underrun();
                        gaps += 1;
                        if t > warm {
                            late_gaps += 1;
                        }
                    } else {
                        len -= take;
                        last_delay = (len + take) / sr * 1000.0;
                    }
                }
            }
            t += cb_ms / 1000.0;
        }
        (late_gaps, gaps, last_delay)
    }

    #[test]
    fn a_steady_source_converges_to_a_small_delay_without_gaps() {
        // 10 ms blocks like a typical input callback
        let (late, all, delay) = simulate(10.0, 1.0, 10.0, 120.0, None, 30.0);
        assert!(late == 0 && all <= 1, "gaps: after warm-up {late}, in all {all}");
        assert!(delay <= 31.0, "delay {delay} ms");
        // 21 ms engine blocks against 10 ms callbacks: needs more reserve, still no gaps once learned
        let (late, all, delay) = simulate(21.3, 2.0, 10.0, 150.0, None, 60.0);
        assert!(late == 0, "gaps after warm-up {late} (all {all})");
        assert!(delay < 60.0, "delay {delay} ms");
    }

    #[test]
    fn a_seek_does_not_teach_the_reserve() {
        let mut ctl = OutCtl::new();
        let before = ctl.cush.ms();
        for _ in 0..30 {
            // playing, then the queue is flushed by a seek and refills at once
            for _ in 0..20 {
                ctl.begin(4800, 480.0, 48000.0, 0.01, 0.0);
            }
            ctl.restart();
            for _ in 0..5 {
                ctl.begin(4800, 480.0, 48000.0, 0.01, 0.0);
            }
        }
        assert_eq!(ctl.cush.ms(), before);
    }

    #[test]
    fn a_stall_costs_one_gap_then_the_reserve_is_larger() {
        let (late, all, _) = simulate(10.0, 1.0, 10.0, 200.0, Some((100.0, 0.060)), 20.0);
        assert!((1..=2).contains(&late), "one gap expected for the stall, got {late} (all {all})");
        // and a source that stays away is not a reason to grow the reserve for ever
        let mut ctl = OutCtl::new();
        for _ in 0..50 {
            ctl.underrun();
            for _ in 0..40 {
                ctl.begin(0, 480.0, 48000.0, 0.01, 0.0);
            }
        }
        assert_eq!(ctl.cush.ms(), Cushion::START_MS);
    }

    #[test]
    fn the_cushion_only_shrinks_with_real_headroom_and_remembers_what_failed() {
        let mut c = Cushion::new();
        // calm, but the queue was close to the cushion: no shrinking
        for _ in 0..40 {
            c.on_calm(6.0, c.ms());
        }
        assert_eq!(c.ms(), Cushion::START_MS);
        // calm with room to spare: smaller, down to the floor and no further
        for _ in 0..80 {
            c.on_calm(6.0, 100.0);
        }
        assert_eq!(c.ms(), Cushion::FLOOR_MS);
        let before = c.ms();
        c.on_gap();
        assert!(c.ms() > before + 3.0, "{}", c.ms());
        for _ in 0..400 {
            c.on_calm(6.0, 100.0);
        }
        assert!(c.ms() >= before + 3.0 - 1e-3, "{}", c.ms());
        for _ in 0..50 {
            c.on_gap();
        }
        assert!(c.ms() <= Cushion::MAX_MS);
    }

    #[test]
    fn queue_drops_oldest_when_behind() {
        let mut q = Queue { buf: VecDeque::new(), src_sr: 48000, ..Default::default() };
        let x: Vec<f32> = (0..40000).map(|i| i as f32).collect();
        q.push(&x, &x);
        assert!(q.buf.len() <= 19200);
        // the newest sample is kept
        assert_eq!(q.buf.back().unwrap().0, 39999.0);
    }
}
