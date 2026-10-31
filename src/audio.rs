//! Audio capture through cpal. Output devices are opened as inputs, which gives WASAPI loopback on Windows.
//!
//! The stream lives on its own thread. If the device is invalidated (sample rate / bit depth changed in
//! the driver or Windows sound settings, device unplugged) the stream is rebuilt automatically with the
//! device's new default format.

use crate::shared::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct DevInfo {
    pub id: String,
    pub label: String,
}

pub fn list_devices() -> Vec<DevInfo> {
    let host = cpal::default_host();
    let mut out: Vec<DevInfo> = Vec::new();
    let default_in = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let default_out = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    if let Ok(devs) = host.devices() {
        for d in devs {
            let Ok(id) = d.id() else { continue };
            let id = id.to_string();
            let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
            let label = if d.supports_input() {
                let star = if Some(&id) == default_in.as_ref() { " (default)" } else { "" };
                format!("[Input] {name}{star}")
            } else if d.supports_output() {
                let star = if Some(&id) == default_out.as_ref() { " (default)" } else { "" };
                format!("[Loopback] {name}{star}")
            } else {
                continue;
            };
            out.push(DevInfo { id, label });
        }
    }
    out.sort_by_key(|d| {
        let default = d.label.contains("(default)");
        let input = d.label.starts_with("[Input]");
        // default input first, then other inputs, then loopbacks (default output first)
        (!(default && input), !input, !default, d.label.clone())
    });
    out
}

/// Dropping the handle asks the audio thread to stop. It never blocks: closing a stream whose device
/// was just invalidated can hang inside the driver, and the UI thread must not wait for that.
pub struct AudioHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

static SIM_ACTIVE: AtomicBool = AtomicBool::new(false);

fn sleep_unless_stopped(stop: &AtomicBool, ms: u64) {
    let mut left = ms;
    while left > 0 && !stop.load(Relaxed) {
        let step = left.min(40);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
}

pub fn start(sh: Arc<Shared>, id: String) -> AudioHandle {
    let gid = sh.audio_gen.fetch_add(1, Relaxed) + 1;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let mut sim_done = false;
            while !stop2.load(Relaxed) {
                let dead = Arc::new(AtomicBool::new(false));
                match build(&sh, &id, gid, dead.clone()) {
                    Ok((stream, info, key)) => {
                        crate::log::log(&format!("audio: stream running: {info}"));
                        *sh.status.lock() = info;
                        let mut last_poll = std::time::Instant::now();
                        let born = std::time::Instant::now();
                        let mut mismatches = 0;
                        sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
                        let mut last_tick = std::time::Instant::now();
                        while !stop2.load(Relaxed) && !dead.load(Relaxed) {
                            std::thread::sleep(Duration::from_millis(10));
                            let dt = last_tick.elapsed().as_secs_f32().min(0.1);
                            last_tick = std::time::Instant::now();
                            // A silent loopback (or a stalled device) delivers no buffers at all. Feed silence so
                            // the analysis keeps running and the display falls to the noise floor instead of
                            // freezing on the last frame.
                            let idle = (sh.t0.elapsed().as_millis() as u64).saturating_sub(sh.last_data_ms.load(Relaxed));
                            if idle > 300 {
                                let n = (key.0 as f32 * dt) as usize;
                                {
                                    let mut r = sh.ring.lock();
                                    for _ in 0..n {
                                        r.push(0.0, 0.0);
                                    }
                                }
                                sh.cv.notify_one();
                                sh.feeding_silence.store(true, Relaxed);
                            } else {
                                sh.feeding_silence.store(false, Relaxed);
                            }
                            // The driver does not always report a format change through the stream's error
                            // callback (a silent loopback never wakes up), so poll the device's format.
                            if last_poll.elapsed() >= Duration::from_millis(750) {
                                last_poll = std::time::Instant::now();
                                // test hook: VISUSCOPE_SIM_CHANGE=1 pretends the format changed once, 8 s in
                                let force = !sim_done && std::env::var_os("VISUSCOPE_SIM_CHANGE").is_some() && born.elapsed() > Duration::from_secs(8);
                                if force {
                                    sim_done = true;
                                    SIM_ACTIVE.store(true, Relaxed);
                                    mismatches = 2;
                                    crate::log::log("audio: SIMULATED format change");
                                }
                                if !force && current_key(&id) == Some(key) {
                                    mismatches = 0;
                                } else {
                                    mismatches += 1;
                                    if mismatches >= 2 {
                                        crate::log::log(&format!("audio: device format now {:?}, built with {key:?} -> rebuilding", current_key(&id)));
                                        dead.store(true, Relaxed);
                                    }
                                }
                            }
                        }
                        if dead.load(Relaxed) && !stop2.load(Relaxed) {
                            // `dead` also silences the old callback. An invalidated stream can block forever
                            // when dropped, so leak it instead.
                            crate::log::log("audio: abandoning old stream (not dropping it)");
                            std::mem::forget(stream);
                            *sh.status.lock() = "Device format changed - reconnecting...".into();
                            sleep_unless_stopped(&stop2, 400);
                        } else {
                            dead.store(true, Relaxed);
                            drop(stream);
                        }
                    }
                    Err(e) => {
                        crate::log::log(&format!("audio: build failed: {e}"));
                        *sh.status.lock() = format!("{e} - retrying...");
                        sleep_unless_stopped(&stop2, 1000);
                    }
                }
            }
            // only the newest audio thread owns the sample rate
            if sh.audio_gen.load(Relaxed) == gid {
                sh.sr.store(0, Relaxed);
            }
        })
        .expect("spawn audio thread");
    AudioHandle { stop }
}

/// (sample rate, channels, format) the device currently offers; `None` if it cannot be queried.
fn current_key(id: &str) -> Option<(u32, u16, SampleFormat)> {
    let host = cpal::default_host();
    let did: cpal::DeviceId = id.parse().ok()?;
    let dev = host.device_by_id(&did)?;
    let cfg = if dev.supports_input() { dev.default_input_config() } else { dev.default_output_config() }.ok()?;
    Some((cfg.sample_rate(), cfg.channels(), cfg.sample_format()))
}

fn build(sh: &Arc<Shared>, id: &str, gid: u32, dead: Arc<AtomicBool>) -> Result<(cpal::Stream, String, (u32, u16, SampleFormat)), String> {
    let host = cpal::default_host();
    let did: cpal::DeviceId = id.parse().map_err(|e| format!("bad device id: {e:?}"))?;
    let dev = host.device_by_id(&did).ok_or_else(|| "device not found".to_string())?;
    let cfg = if dev.supports_input() { dev.default_input_config() } else { dev.default_output_config() }.map_err(|e| format!("{e}"))?;
    let fmt = cfg.sample_format();
    let mut sr = cfg.sample_rate();
    // test hook: after VISUSCOPE_SIM_CHANGE fires, report a different sample rate to the analysis side
    if SIM_ACTIVE.load(Relaxed) {
        if let Some(v) = std::env::var("VISUSCOPE_SIM_SR").ok().and_then(|s| s.parse::<u32>().ok()) {
            sr = v;
        }
    }
    let ch = cfg.channels();
    let mut sc: cpal::StreamConfig = cfg.clone().into();
    let default_buf = sc.buffer_size.clone();
    if let cpal::SupportedBufferSize::Range { min, max } = cfg.buffer_size() {
        sc.buffer_size = cpal::BufferSize::Fixed(2048u32.clamp(*min, *max));
    }
    let mut sc_default = sc.clone();
    sc_default.buffer_size = default_buf;

    {
        let mut r = sh.ring.lock();
        r.total = 0;
        r.l.fill(0.0);
        r.r.fill(0.0);
    }
    sh.sr.store(sr, Relaxed);
    sh.chans.store(ch as u32, Relaxed);

    let build_with = |sc: cpal::StreamConfig| match fmt {
        SampleFormat::F32 => make::<f32>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::F64 => make::<f64>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::I8 => make::<i8>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::I16 => make::<i16>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::I24 => make::<cpal::I24>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::I32 => make::<i32>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::I64 => make::<i64>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::U8 => make::<u8>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::U16 => make::<u16>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::U24 => make::<cpal::U24>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::U32 => make::<u32>(&dev, sc, sh.clone(), gid, dead.clone()),
        SampleFormat::U64 => make::<u64>(&dev, sc, sh.clone(), gid, dead.clone()),
        _ => Err(cpal::ErrorKind::UnsupportedConfig.into()),
    };
    // Larger buffer first (fewer xruns); fall back to the driver default if refused.
    let stream = build_with(sc).or_else(|_| build_with(sc_default)).map_err(|e| format!("{e} (format {fmt:?}, {sr} Hz)"))?;
    sh.xruns.store(0, Relaxed);
    stream.play().map_err(|e| format!("{e}"))?;
    Ok((stream, format!("{sr} Hz, {ch} ch, {fmt:?}"), (sr, ch, fmt)))
}

fn make<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, sh: Arc<Shared>, gid: u32, dead: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let ch = cfg.channels as usize;
    let in_sr = cfg.sample_rate;
    let (mut ml, mut mr): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let sh_err = sh.clone();
    let dead_cb = dead.clone();
    dev.build_input_stream(
        cfg,
        move |data: &[T], _: &_| {
            // a stream that has been replaced or abandoned must not feed the ring any more
            if sh.audio_gen.load(Relaxed) != gid || dead_cb.load(Relaxed) {
                return;
            }
            let li = sh.ch_a.load(Relaxed).min(ch - 1);
            let ri = sh.ch_b.load(Relaxed).min(ch - 1);
            let nm = ch.min(MAX_CH);
            let mut peak = [0f32; MAX_CH];
            let mon = sh.mon_on.load(Relaxed);
            ml.clear();
            mr.clear();
            {
                let mut ring = sh.ring.lock();
                for frame in data.chunks_exact(ch) {
                    let (l, r) = (f32::from_sample(frame[li]), f32::from_sample(frame[ri]));
                    ring.push(l, r);
                    if mon {
                        ml.push(l);
                        mr.push(r);
                    }
                    if ch > 2 {
                        for (c, p) in peak[..nm].iter_mut().enumerate() {
                            *p = p.max(f32::from_sample(frame[c]).abs());
                        }
                    }
                }
            }
            if ch > 2 {
                for c in 0..nm {
                    sh.ch_peak[c].fetch_max(peak[c].to_bits(), Relaxed);
                }
            }
            if mon {
                if let Some(mut q) = sh.mon_q.try_lock() {
                    q.src_sr = in_sr;
                    q.push(&ml, &mr);
                }
            }
            sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
            sh.cv.notify_one();
        },
        move |e| match e.kind() {
            cpal::ErrorKind::Xrun => {
                sh_err.xruns.fetch_add(1, Relaxed);
            }
            // harmless: stream is still running
            cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceBusy => {}
            // anything else means the stream is gone (format changed, device removed): rebuild it
            _ => {
                crate::log::log(&format!("audio: stream error {:?}: {e} -> rebuilding", e.kind()));
                dead.store(true, Relaxed)
            }
        },
        None,
    )
}
