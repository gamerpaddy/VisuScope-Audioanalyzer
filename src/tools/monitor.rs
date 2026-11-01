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
            let step = q.src_sr.max(1) as f64 / out_sr as f64;
            // pre-buffer ~60 ms before (re)starting so short hiccups do not stutter
            if !primed {
                // 60 ms of cushion, plus the safety buffer chosen in Options
                if q.buf.len() < (q.src_sr as usize * (60 + safety_ms) / 1000).max(64) {
                    return;
                }
                primed = true;
            }
            for frame in data.chunks_mut(ch) {
                while frac >= 1.0 {
                    prev = next;
                    match q.buf.pop_front() {
                        Some(s) => next = s,
                        None => {
                            // the source did not deliver in time: a gap in the sound
                            primed = false;
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
    fn queue_drops_oldest_when_behind() {
        let mut q = Queue { buf: VecDeque::new(), src_sr: 48000 };
        let x: Vec<f32> = (0..40000).map(|i| i as f32).collect();
        q.push(&x, &x);
        assert!(q.buf.len() <= 19200);
        // the newest sample is kept
        assert_eq!(q.buf.back().unwrap().0, 39999.0);
    }
}
