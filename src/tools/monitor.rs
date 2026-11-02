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
}

impl Queue {
    /// Append samples; if the output falls far behind (clock drift) the oldest ones are dropped.
    pub fn push(&mut self, l: &[f32], r: &[f32]) {
        self.buf.extend(l.iter().copied().zip(r.iter().copied()));
        // the backlog that is tolerated grows with the safety buffer
        let cap = (self.src_sr as usize * (400 + 2 * crate::prof::safety_ms() as usize) / 1000).max(4096);
        if self.buf.len() > cap {
            let drop = self.buf.len() - cap / 2;
            self.buf.drain(..drop);
        }
    }
}

/// How much sound an output holds back before it starts playing (and again after a gap). It starts small and learns:
/// a gap makes it larger at once and remembers that the smaller value was too little; after a calm stretch it tries a
/// slightly smaller one again, never below the largest value that failed. The result is the least delay this system
/// and these sources can do without hiccups.
pub struct Cushion {
    cur: f32,
    floor: f32,
    calm: f32,
}

impl Cushion {
    pub const START_MS: f32 = 30.0;
    pub const FLOOR_MS: f32 = 12.0;
    pub const MAX_MS: f32 = 150.0;
    /// seconds without a gap before a smaller cushion is tried
    const CALM_SECS: f32 = 12.0;

    pub fn new() -> Self {
        Self { cur: Self::START_MS, floor: Self::FLOOR_MS, calm: 0.0 }
    }

    pub fn ms(&self) -> f32 {
        self.cur
    }

    pub fn on_gap(&mut self) {
        self.floor = (self.cur + 3.0).min(Self::MAX_MS);
        self.cur = (self.cur * 1.3 + 4.0).clamp(self.floor, Self::MAX_MS);
        self.calm = 0.0;
    }

    /// `secs` of sound were played without a gap.
    pub fn on_played(&mut self, secs: f32) {
        self.calm += secs;
        if self.calm >= Self::CALM_SECS {
            self.calm = 0.0;
            self.cur = (self.cur - 2.0).max(self.floor);
        }
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
    let (mut prev, mut next) = ((0f32, 0f32), (0f32, 0f32));
    let mut frac = 1.0f64;
    let mut primed = false;
    let mut cushion = Cushion::new();
    // a source that ran dry and came back within a quarter of a second had a hiccup; one that stays away has stopped
    // (the timeline was stopped): only the first teaches the cushion
    let mut gap: Option<f32> = None;
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
            let Some(mut q) = q.try_lock() else { return };
            let mut step = q.src_sr.max(1) as f64 / out_sr as f64;
            // the cushion that is held back before playing starts (it learns, see `Cushion`), plus the safety buffer of Options
            let want = (q.src_sr as f32 * (cushion.ms() + safety_ms as f32) / 1000.0) as usize;
            if !primed {
                if let Some(g) = gap.as_mut() {
                    *g += data.len() as f32 / ch as f32 / out_sr.max(1) as f32;
                    if *g > 0.25 {
                        gap = None;
                    }
                }
                if q.buf.len() < want.max(64) {
                    return;
                }
                primed = true;
                if gap.take().is_some() {
                    cushion.on_gap();
                }
            }
            crate::prof::C.out_cushion_ms.store((cushion.ms() + safety_ms as f32) as u32, Relaxed);
            // more is queued than the cushion needs (a source that runs a little fast, or a cushion that just got smaller):
            // play slightly faster (0.5 %, inaudible) until the surplus is gone
            if q.buf.len() > want + want / 2 + q.src_sr as usize / 100 {
                step *= 1.005;
            }
            cushion.on_played(data.len() as f32 / ch as f32 / out_sr.max(1) as f32);
            for frame in data.chunks_mut(ch) {
                while frac >= 1.0 {
                    prev = next;
                    match q.buf.pop_front() {
                        Some(s) => next = s,
                        None => {
                            // the source did not deliver in time: a gap in the sound
                            primed = false;
                            gap = Some(0.0);
                            crate::prof::C.out_underruns.fetch_add(1, Relaxed);
                            return;
                        }
                    }
                    frac -= 1.0;
                }
                let t = frac as f32;
                let l = (prev.0 + (next.0 - prev.0) * t) * g;
                let r = (prev.1 + (next.1 - prev.1) * t) * g;
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

    #[test]
    fn the_cushion_shrinks_when_calm_grows_after_a_gap_and_remembers_what_failed() {
        let mut c = Cushion::new();
        assert_eq!(c.ms(), Cushion::START_MS);
        // calm: smaller, down to the floor and no further
        for _ in 0..40 {
            c.on_played(6.0);
        }
        assert_eq!(c.ms(), Cushion::FLOOR_MS);
        // a gap: larger at once
        let before = c.ms();
        c.on_gap();
        assert!(c.ms() > before + 3.0, "{}", c.ms());
        let failed_at = before;
        // calm again: it comes back down, but not to a value that already failed
        for _ in 0..400 {
            c.on_played(6.0);
        }
        assert!(c.ms() >= failed_at + 3.0 - 1e-3, "{}", c.ms());
        // repeated gaps never leave the allowed range
        for _ in 0..50 {
            c.on_gap();
        }
        assert!(c.ms() <= Cushion::MAX_MS);
    }

    #[test]
    fn queue_drops_oldest_when_behind() {
        let mut q = Queue { buf: VecDeque::new(), src_sr: 48000 };
        let x: Vec<f32> = (0..40000).map(|i| i as f32).collect();
        q.push(&x, &x);
        assert!(q.buf.len() <= 19200);
        // the newest sample is kept
        assert_eq!(q.buf.back().unwrap().0, 39999.0);
    }
}
