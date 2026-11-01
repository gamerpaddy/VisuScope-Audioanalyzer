//! Multi-device capture for the timeline. Every device that an armed track reads from gets its own
//! input stream (on its own thread, like the main capture); while armed the streams only meter, and
//! "record" switches them to storing the channels the armed tracks need. Output devices are opened as
//! loopback inputs, exactly like in `audio.rs`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// Device channels that get a meter / can be recorded.
pub const MAX_LANES: usize = 64;

/// What the UI knows about an input device before it is opened.
#[derive(Clone)]
pub struct DevEx {
    pub id: String,
    pub label: String,
    pub ch: usize,
    pub sr: u32,
}

/// Probe channel count and rate of every capture device. Can take a while with some drivers: call from a worker.
pub fn probe_all() -> Vec<DevEx> {
    let host = cpal::default_host();
    let mut out: Vec<DevEx> = crate::audio::list_devices()
        .into_iter()
        .filter_map(|d| {
            let did: cpal::DeviceId = d.id.parse().ok()?;
            let dev = host.device_by_id(&did)?;
            let cfg = if dev.supports_input() { dev.default_input_config() } else { dev.default_output_config() }.ok()?;
            Some(DevEx { id: d.id, label: d.label, ch: cfg.channels() as usize, sr: cfg.sample_rate() })
        })
        .collect();
    out.sort_by_key(|d| !d.label.starts_with("[Input]"));
    out
}

pub fn short_label(label: &str) -> String {
    let s = label.trim_start_matches("[Input] ").trim_start_matches("[Loopback] ");
    let s = s.trim_end_matches(" (default)");
    if label.starts_with("[Loopback]") { format!("{s} (loopback)") } else { s.to_string() }
}

struct Buf {
    /// store samples while set
    rec: AtomicBool,
    /// lanes (device channels, then the mono mix) that are stored
    need: Vec<AtomicBool>,
    data: Mutex<Vec<Vec<f32>>>,
    peak: Vec<AtomicU32>,
    frames: AtomicU64,
    /// set when the stream was rebuilt while recording
    gap: AtomicBool,
}

/// One open capture device.
pub struct Capture {
    pub id: String,
    buf: Arc<Buf>,
    stop: Arc<AtomicBool>,
    info: Arc<Mutex<Option<(u32, usize)>>>,
    pub status: Arc<Mutex<String>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

pub struct Take {
    pub sr: u32,
    /// lane -> samples (only the lanes that were needed hold data)
    pub lanes: Vec<Vec<f32>>,
    pub gap: bool,
}

impl Capture {
    pub fn start(id: String) -> Self {
        let buf = Arc::new(Buf {
            rec: AtomicBool::new(false),
            need: (0..=MAX_LANES).map(|_| AtomicBool::new(false)).collect(),
            data: Mutex::new(Vec::new()),
            peak: (0..=MAX_LANES).map(|_| AtomicU32::new(0)).collect(),
            frames: AtomicU64::new(0),
            gap: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let info = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(String::from("opening...")));
        let (b2, s2, i2, st2, id2) = (buf.clone(), stop.clone(), info.clone(), status.clone(), id.clone());
        std::thread::Builder::new().name("daw-capture".into()).spawn(move || run(id2, b2, s2, i2, st2)).ok();
        Self { id, buf, stop, info, status }
    }

    /// (sample rate, channels) once the stream runs.
    pub fn format(&self) -> Option<(u32, usize)> {
        *self.info.lock()
    }

    /// Largest level per lane since the last call (linear), `ch` channels plus the mix.
    pub fn take_peaks(&self, ch: usize) -> Vec<f32> {
        (0..=ch.min(MAX_LANES)).map(|i| f32::from_bits(self.buf.peak[i].swap(0, Relaxed))).collect()
    }

    pub fn frames(&self) -> u64 {
        self.buf.frames.load(Relaxed)
    }

    /// Start storing the given lanes.
    pub fn begin(&self, lanes: &[usize]) {
        let (sr, ch) = self.format().unwrap_or((48000, 2));
        let mut d = self.buf.data.lock();
        d.clear();
        d.resize(ch.min(MAX_LANES) + 1, Vec::new());
        for (i, n) in self.buf.need.iter().enumerate() {
            n.store(lanes.contains(&i), Relaxed);
        }
        for &l in lanes {
            if let Some(v) = d.get_mut(l) {
                v.reserve(sr as usize * 30);
            }
        }
        self.buf.frames.store(0, Relaxed);
        self.buf.gap.store(false, Relaxed);
        drop(d);
        self.buf.rec.store(true, Relaxed);
    }

    pub fn finish(&self) -> Take {
        self.buf.rec.store(false, Relaxed);
        let (sr, _) = self.format().unwrap_or((48000, 2));
        // the callback may still be inside its critical section: this lock waits for it
        let lanes = std::mem::take(&mut *self.buf.data.lock());
        Take { sr, lanes, gap: self.buf.gap.swap(false, Relaxed) }
    }

    /// Min / max over `cols` equal slices of the first `limit` recorded frames of a lane (live preview while recording).
    pub fn preview(&self, lane: usize, from: usize, to: usize, cols: usize) -> Vec<(f32, f32)> {
        let d = self.buf.data.lock();
        let Some(v) = d.get(lane) else { return Vec::new() };
        let to = to.min(v.len());
        if to <= from || cols == 0 {
            return Vec::new();
        }
        let span = (to - from) as f64 / cols as f64;
        (0..cols)
            .map(|c| {
                let a = from + (c as f64 * span) as usize;
                let b = (from + ((c + 1) as f64 * span) as usize).clamp(a + 1, to);
                let step = ((b - a) / 48).max(1);
                v[a..b].iter().step_by(step).fold((0f32, 0f32), |(lo, hi), &x| (lo.min(x), hi.max(x)))
            })
            .collect()
    }
}

fn run(id: String, buf: Arc<Buf>, stop: Arc<AtomicBool>, info: Arc<Mutex<Option<(u32, usize)>>>, status: Arc<Mutex<String>>) {
    while !stop.load(Relaxed) {
        let dead = Arc::new(AtomicBool::new(false));
        match build(&id, &buf, dead.clone()) {
            Ok((stream, sr, ch, text)) => {
                *info.lock() = Some((sr, ch));
                *status.lock() = text;
                while !stop.load(Relaxed) && !dead.load(Relaxed) {
                    std::thread::sleep(Duration::from_millis(40));
                }
                if dead.load(Relaxed) && !stop.load(Relaxed) {
                    if buf.rec.load(Relaxed) {
                        buf.gap.store(true, Relaxed);
                    }
                    *info.lock() = None;
                    *status.lock() = "device changed, reconnecting...".into();
                    // an invalidated stream can hang when dropped: leak it (see audio.rs)
                    std::mem::forget(stream);
                    std::thread::sleep(Duration::from_millis(500));
                } else {
                    dead.store(true, Relaxed);
                    drop(stream);
                }
            }
            Err(e) => {
                crate::log::log(&format!("daw capture: {e}"));
                *status.lock() = format!("{e}");
                for _ in 0..25 {
                    if stop.load(Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        }
    }
}

fn build(id: &str, buf: &Arc<Buf>, dead: Arc<AtomicBool>) -> Result<(cpal::Stream, u32, usize, String), String> {
    let host = cpal::default_host();
    let did: cpal::DeviceId = id.parse().map_err(|e| format!("bad device id: {e:?}"))?;
    let dev = host.device_by_id(&did).ok_or_else(|| "device not found".to_string())?;
    let cfg = if dev.supports_input() { dev.default_input_config() } else { dev.default_output_config() }.map_err(|e| e.to_string())?;
    let (fmt, sr, ch) = (cfg.sample_format(), cfg.sample_rate(), cfg.channels() as usize);
    let sc: cpal::StreamConfig = cfg.into();
    let b = buf.clone();
    let stream = match fmt {
        SampleFormat::F32 => make::<f32>(&dev, sc, b, dead),
        SampleFormat::F64 => make::<f64>(&dev, sc, b, dead),
        SampleFormat::I8 => make::<i8>(&dev, sc, b, dead),
        SampleFormat::I16 => make::<i16>(&dev, sc, b, dead),
        SampleFormat::I24 => make::<cpal::I24>(&dev, sc, b, dead),
        SampleFormat::I32 => make::<i32>(&dev, sc, b, dead),
        SampleFormat::U8 => make::<u8>(&dev, sc, b, dead),
        SampleFormat::U16 => make::<u16>(&dev, sc, b, dead),
        SampleFormat::U32 => make::<u32>(&dev, sc, b, dead),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("{e} ({sr} Hz, {fmt:?})"))?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, sr, ch, format!("{sr} Hz, {ch} ch")))
}

fn make<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, buf: Arc<Buf>, dead: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let ch = (cfg.channels as usize).max(1);
    let lanes = ch.min(MAX_LANES);
    let dead_cb = dead.clone();
    let mut peak = vec![0f32; lanes + 1];
    let mut need: Vec<usize> = Vec::new();
    dev.build_input_stream(
        cfg,
        move |data: &[T], _: &_| {
            if dead_cb.load(Relaxed) {
                return;
            }
            peak.iter_mut().for_each(|p| *p = 0.0);
            let rec = buf.rec.load(Relaxed);
            if rec {
                need.clear();
                need.extend((0..=lanes).filter(|&l| buf.need[l].load(Relaxed)));
            }
            let mut guard = if rec { Some(buf.data.lock()) } else { None };
            let mut n = 0u64;
            for frame in data.chunks_exact(ch) {
                let mut mix = 0f32;
                for (c, s) in frame.iter().enumerate().take(lanes) {
                    let v = f32::from_sample(*s);
                    peak[c] = peak[c].max(v.abs());
                    mix += v;
                    if let Some(g) = guard.as_mut() {
                        if buf.need[c].load(Relaxed) {
                            if let Some(lane) = g.get_mut(c) {
                                lane.push(v);
                            }
                        }
                    }
                }
                let mix = mix / lanes as f32;
                peak[lanes] = peak[lanes].max(mix.abs());
                if let Some(g) = guard.as_mut() {
                    if need.contains(&lanes) {
                        if let Some(lane) = g.get_mut(lanes) {
                            lane.push(mix);
                        }
                    }
                }
                n += 1;
            }
            if rec {
                buf.frames.fetch_add(n, Relaxed);
            }
            drop(guard);
            for (i, p) in peak.iter().enumerate() {
                buf.peak[i].fetch_max(p.to_bits(), Relaxed);
            }
        },
        move |e| match e.kind() {
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceBusy => {}
            _ => {
                crate::log::log(&format!("daw capture: stream error {:?}: {e}", e.kind()));
                dead.store(true, Relaxed)
            }
        },
        None,
    )
}
