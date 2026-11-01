//! Signal generator: tones, sweeps, multitone, IMD pairs, noise, harmonics, phase, jitter and
//! word-length quantisation with dither. Rendered in the output stream callback.

use crate::audio::DevInfo;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Wave {
    Sine,
    Square,
    Triangle,
    Saw,
    Pulse,
    Impulse,
    SweepLog,
    SweepLin,
    Multitone,
    Smpte,
    Ccif,
    Noise,
}

impl Wave {
    pub const ALL: [Wave; 12] = [
        Wave::Sine,
        Wave::Square,
        Wave::Triangle,
        Wave::Saw,
        Wave::Pulse,
        Wave::Impulse,
        Wave::SweepLog,
        Wave::SweepLin,
        Wave::Multitone,
        Wave::Smpte,
        Wave::Ccif,
        Wave::Noise,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Wave::Sine => "Sine",
            Wave::Square => "Square",
            Wave::Triangle => "Triangle",
            Wave::Saw => "Sawtooth",
            Wave::Pulse => "Pulse (duty)",
            Wave::Impulse => "Impulse train",
            Wave::SweepLog => "Sweep (log)",
            Wave::SweepLin => "Sweep (linear)",
            Wave::Multitone => "Multitone",
            Wave::Smpte => "IMD SMPTE (60 Hz + 7 kHz, 4:1)",
            Wave::Ccif => "IMD CCIF (19 kHz + 20 kHz)",
            Wave::Noise => "Noise",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoiseKind {
    White,
    WhiteUniform,
    Pink,
    Brown,
    Blue,
    Violet,
}

impl NoiseKind {
    pub const ALL: [NoiseKind; 6] = [NoiseKind::White, NoiseKind::WhiteUniform, NoiseKind::Pink, NoiseKind::Brown, NoiseKind::Blue, NoiseKind::Violet];

    pub fn name(self) -> &'static str {
        match self {
            NoiseKind::White => "White (gaussian)",
            NoiseKind::WhiteUniform => "White (uniform)",
            NoiseKind::Pink => "Pink (-3 dB/oct)",
            NoiseKind::Brown => "Brown (-6 dB/oct)",
            NoiseKind::Blue => "Blue (+3 dB/oct)",
            NoiseKind::Violet => "Violet (+6 dB/oct)",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    Off,
    Rpdf,
    Tpdf,
    ShapedTpdf,
}

impl Dither {
    pub const ALL: [Dither; 4] = [Dither::Off, Dither::Rpdf, Dither::Tpdf, Dither::ShapedTpdf];

    pub fn name(self) -> &'static str {
        match self {
            Dither::Off => "None (truncation noise)",
            Dither::Rpdf => "RPDF",
            Dither::Tpdf => "TPDF",
            Dither::ShapedTpdf => "High-pass TPDF",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct GenParams {
    pub on: bool,
    pub wave: Wave,
    pub freq: f32,
    /// level per channel in dBFS (peak for tones, RMS for noise / multitone)
    pub level_db: [f32; 2],
    pub mute: [bool; 2],
    pub duty: f32,
    pub sweep_f0: f32,
    pub sweep_f1: f32,
    pub sweep_time: f32,
    pub mt_count: usize,
    pub mt_f0: f32,
    pub mt_f1: f32,
    pub mt_log: bool,
    pub mt_schroeder: bool,
    pub noise: NoiseKind,
    pub noise_corr: bool,
    pub harm_on: bool,
    /// H2..H6 in dBc
    pub harm_db: [f32; 5],
    /// R relative to L, degrees
    pub phase_deg: f32,
    pub invert_r: bool,
    pub jitter_on: bool,
    pub jit_rms_ns: f32,
    pub jit_sine_ns: f32,
    pub jit_sine_hz: f32,
    pub quant_on: bool,
    pub bits: u32,
    pub dither: Dither,
    pub dither_lsb: f32,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            on: false,
            wave: Wave::Sine,
            freq: 1000.0,
            level_db: [-20.0, -20.0],
            mute: [false, false],
            duty: 0.25,
            sweep_f0: 20.0,
            sweep_f1: 20000.0,
            sweep_time: 5.0,
            mt_count: 10,
            mt_f0: 100.0,
            mt_f1: 10000.0,
            mt_log: true,
            mt_schroeder: true,
            noise: NoiseKind::Pink,
            noise_corr: false,
            harm_on: false,
            harm_db: [-60.0, -70.0, -80.0, -90.0, -100.0],
            phase_deg: 0.0,
            invert_r: false,
            jitter_on: false,
            jit_rms_ns: 1.0,
            jit_sine_ns: 0.0,
            jit_sine_hz: 1000.0,
            quant_on: false,
            bits: 16,
            dither: Dither::Tpdf,
            dither_lsb: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------- DSP

struct Rng(u64);

impl Rng {
    #[inline]
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// uniform in [0, 1)
    #[inline]
    fn uni(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16_777_216.0
    }

    /// gaussian, unit variance
    #[inline]
    fn gauss(&mut self) -> f32 {
        let u1 = (self.uni() + 1e-12).min(1.0);
        let u2 = self.uni();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

#[derive(Default, Clone, Copy)]
struct NoiseState {
    b: [f32; 7],
    brown: f32,
    prev_pink: f32,
    prev_white: f32,
}

fn raw_noise(kind: NoiseKind, st: &mut NoiseState, rng: &mut Rng) -> f32 {
    match kind {
        NoiseKind::White => rng.gauss(),
        NoiseKind::WhiteUniform => (rng.uni() * 2.0 - 1.0) * 3f32.sqrt(),
        NoiseKind::Pink | NoiseKind::Blue => {
            let w = rng.uni() * 2.0 - 1.0;
            let b = &mut st.b;
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.1538520;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let pink = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
            b[6] = w * 0.115926;
            if kind == NoiseKind::Pink {
                pink
            } else {
                let d = pink - st.prev_pink;
                st.prev_pink = pink;
                d
            }
        }
        NoiseKind::Brown => {
            st.brown = (st.brown + 0.02 * rng.gauss()) / 1.02;
            st.brown
        }
        NoiseKind::Violet => {
            let w = rng.gauss();
            let d = w - st.prev_white;
            st.prev_white = w;
            d
        }
    }
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

pub struct GenState {
    sr: f64,
    n: u64,
    acc: f64,
    mt_acc: Vec<f64>,
    sweep_t: f64,
    gain: f32,
    rng: Rng,
    noise: [NoiseState; 2],
    norm: [f32; 6],
    dprev: [f32; 2],
    prev_wrap: bool,
}

impl GenState {
    pub fn new(sr: f64) -> Self {
        // normalise every noise colour to unit RMS (measured once: it takes tens of milliseconds, and
        // generators are created on audio threads)
        static NORM: std::sync::OnceLock<[f32; 6]> = std::sync::OnceLock::new();
        let norm = *NORM.get_or_init(|| {
            let mut norm = [1.0f32; 6];
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
            for k in NoiseKind::ALL {
                let mut st = NoiseState::default();
                let mut sum = 0.0f64;
                let n = 300_000;
                for i in 0..n {
                    let v = raw_noise(k, &mut st, &mut rng) as f64;
                    if i > 20_000 {
                        sum += v * v;
                    }
                }
                norm[k.index()] = (1.0 / (sum / (n - 20_001) as f64).sqrt()) as f32;
            }
            norm
        });
        Self {
            sr,
            n: 0,
            acc: 0.0,
            mt_acc: vec![0.0; 64],
            sweep_t: 0.0,
            gain: 0.0,
            rng: Rng(0xD1B5_4A32_D192_ED03),
            noise: [NoiseState::default(); 2],
            norm,
            dprev: [0.0; 2],
            prev_wrap: false,
        }
    }

    fn periodic(&mut self, w: Wave, ph: f64, dt: f32, duty: f32) -> f32 {
        let t = ph.rem_euclid(1.0) as f32;
        match w {
            Wave::Square => {
                let s = if t < 0.5 { 1.0 } else { -1.0 };
                s + poly_blep(t, dt) - poly_blep((t + 0.5) % 1.0, dt)
            }
            Wave::Pulse => {
                let s = if t < duty { 1.0 } else { -1.0 };
                s + poly_blep(t, dt) - poly_blep((t + 1.0 - duty) % 1.0, dt)
            }
            Wave::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Wave::Triangle => {
                if t < 0.5 {
                    4.0 * t - 1.0
                } else {
                    3.0 - 4.0 * t
                }
            }
            _ => (std::f64::consts::TAU * ph).sin() as f32,
        }
    }

    pub fn render(&mut self, p: &GenParams, out: &mut [f32], channels: usize) {
        let sr = self.sr;
        let tau = std::f64::consts::TAU;
        let fade = 1.0 - (-1.0 / (0.01 * sr as f32)).exp();
        let amp = [
            if p.mute[0] { 0.0 } else { 10f32.powf(p.level_db[0] / 20.0) },
            if p.mute[1] { 0.0 } else { 10f32.powf(p.level_db[1] / 20.0) },
        ];
        let hamp: Vec<f32> = p.harm_db.iter().map(|d| 10f32.powf(d / 20.0)).collect();
        let dphase = p.phase_deg as f64 / 360.0;
        let mt_n = p.mt_count.clamp(1, 64);
        let mt_f: Vec<f64> = (0..mt_n)
            .map(|k| {
                let x = if mt_n > 1 { k as f64 / (mt_n - 1) as f64 } else { 0.0 };
                if p.mt_log {
                    p.mt_f0 as f64 * (p.mt_f1 as f64 / p.mt_f0 as f64).powf(x)
                } else {
                    p.mt_f0 as f64 + (p.mt_f1 as f64 - p.mt_f0 as f64) * x
                }
            })
            .collect();
        let mt_amp = 0.7 / (mt_n as f32).sqrt();
        let q = if p.quant_on { 2f32.powi(p.bits.clamp(2, 24) as i32 - 1) } else { 0.0 };

        // measurement sweep (tools::sweep) is mixed on top of whatever the generator plays
        let mut sweep = if crate::tools::sweep::PLAY_ACTIVE.load(Relaxed) { crate::tools::sweep::PLAYER.try_lock() } else { None };
        for frame in out.chunks_mut(channels) {
            let sw = match sweep.as_mut() {
                Some(g) => crate::tools::sweep::next_sample(g, self.sr),
                None => 0.0,
            };
            self.gain += ((if p.on { 1.0 } else { 0.0 }) - self.gain) * fade;
            if self.gain < 1e-5 && !p.on {
                frame.fill(0.0);
                frame[0] = sw;
                if channels > 1 {
                    frame[1] = sw;
                }
                continue;
            }
            let t = self.n as f64 / sr;
            let mut j = 0.0f64;
            if p.jitter_on {
                j = self.rng.gauss() as f64 * p.jit_rms_ns as f64 * 1e-9;
                j += p.jit_sine_ns as f64 * 1e-9 * (tau * p.jit_sine_hz as f64 * t).sin();
            }
            let f = p.freq as f64;
            let dt = (f / sr) as f32;
            let mut s = [0.0f32; 2];
            match p.wave {
                Wave::Sine | Wave::Square | Wave::Triangle | Wave::Saw | Wave::Pulse => {
                    for c in 0..2 {
                        let ph = self.acc + f * j + if c == 1 { dphase } else { 0.0 };
                        let mut v = self.periodic(p.wave, ph, dt, p.duty);
                        if p.harm_on {
                            for (h, a) in hamp.iter().enumerate() {
                                v += a * (tau * (h as f64 + 2.0) * ph).sin() as f32;
                            }
                        }
                        s[c] = v * amp[c];
                    }
                    self.acc = (self.acc + f / sr).rem_euclid(1.0);
                }
                Wave::Impulse => {
                    let wrapped = self.acc + f / sr >= 1.0;
                    self.acc = (self.acc + f / sr).rem_euclid(1.0);
                    let v = if wrapped && !self.prev_wrap { 1.0 } else { 0.0 };
                    self.prev_wrap = wrapped;
                    s = [v * amp[0], v * amp[1]];
                }
                Wave::SweepLog | Wave::SweepLin => {
                    let (f0, f1, tt) = (p.sweep_f0 as f64, (p.sweep_f1 as f64).max(p.sweep_f0 as f64 + 1.0), (p.sweep_time as f64).max(0.05));
                    let x = self.sweep_t;
                    let (phase, finst) = if p.wave == Wave::SweepLog {
                        let r = f1 / f0;
                        let lr = r.ln();
                        (f0 * tt / lr * (r.powf(x / tt) - 1.0), f0 * r.powf(x / tt))
                    } else {
                        (f0 * x + (f1 - f0) * x * x / (2.0 * tt), f0 + (f1 - f0) * x / tt)
                    };
                    for c in 0..2 {
                        let ph = phase + finst * j + if c == 1 { dphase } else { 0.0 };
                        let mut v = (tau * ph).sin() as f32;
                        if p.harm_on {
                            for (h, a) in hamp.iter().enumerate() {
                                v += a * (tau * (h as f64 + 2.0) * ph).sin() as f32;
                            }
                        }
                        s[c] = v * amp[c];
                    }
                    self.sweep_t += 1.0 / sr;
                    if self.sweep_t >= tt {
                        self.sweep_t -= tt;
                    }
                }
                Wave::Multitone => {
                    for c in 0..2 {
                        let mut v = 0.0f32;
                        for k in 0..mt_n {
                            let p0 = if p.mt_schroeder { 0.5 * (k * k) as f64 / mt_n as f64 } else { (k as f64 * 0.618_033_988_75).fract() };
                            let ph = self.mt_acc[k] + mt_f[k] * j + p0 + if c == 1 { dphase } else { 0.0 };
                            v += (tau * ph).sin() as f32;
                        }
                        s[c] = v * mt_amp * amp[c];
                    }
                    for k in 0..mt_n {
                        self.mt_acc[k] = (self.mt_acc[k] + mt_f[k] / sr).rem_euclid(1.0);
                    }
                }
                Wave::Smpte | Wave::Ccif => {
                    let (f1, a1, f2, a2) = if p.wave == Wave::Smpte { (60.0, 0.8, 7000.0, 0.2) } else { (19000.0, 0.5, 20000.0, 0.5) };
                    for c in 0..2 {
                        let off = if c == 1 { dphase } else { 0.0 };
                        let v = a1 * (tau * (self.mt_acc[0] + f1 * j + off)).sin() + a2 * (tau * (self.mt_acc[1] + f2 * j + off)).sin();
                        s[c] = v as f32 * amp[c];
                    }
                    self.mt_acc[0] = (self.mt_acc[0] + f1 / sr).rem_euclid(1.0);
                    self.mt_acc[1] = (self.mt_acc[1] + f2 / sr).rem_euclid(1.0);
                }
                Wave::Noise => {
                    let k = p.noise;
                    let nrm = self.norm[k.index()];
                    let a = raw_noise(k, &mut self.noise[0], &mut self.rng) * nrm;
                    let b = if p.noise_corr { a } else { raw_noise(k, &mut self.noise[1], &mut self.rng) * nrm };
                    s = [a * amp[0], b * amp[1]];
                }
            }
            if p.invert_r {
                s[1] = -s[1];
            }
            for c in 0..2 {
                let mut y = s[c];
                if q > 0.0 {
                    let d = match p.dither {
                        Dither::Off => 0.0,
                        Dither::Rpdf => self.rng.uni() - 0.5,
                        Dither::Tpdf => self.rng.uni() - self.rng.uni(),
                        Dither::ShapedTpdf => {
                            let t = self.rng.uni() - self.rng.uni();
                            let d = t - self.dprev[c];
                            self.dprev[c] = t;
                            d * 0.7
                        }
                    } * p.dither_lsb;
                    y = ((y * q + d).round() / q).clamp(-1.0, 1.0);
                }
                s[c] = (y.clamp(-1.0, 1.0) * self.gain + sw).clamp(-1.0, 1.0);
            }
            if channels == 1 {
                frame[0] = 0.5 * (s[0] + s[1]);
            } else {
                frame[0] = s[0];
                frame[1] = s[1];
                for v in frame.iter_mut().skip(2) {
                    *v = 0.0;
                }
            }
            self.n += 1;
        }
    }
}

// ---------------------------------------------------------------------------- output stream

pub fn list_output_devices() -> Vec<DevInfo> {
    let host = cpal::default_host();
    let default_out = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let mut out = Vec::new();
    if let Ok(devs) = host.devices() {
        for d in devs {
            if !d.supports_output() {
                continue;
            }
            let Ok(id) = d.id() else { continue };
            let id = id.to_string();
            let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
            let star = if Some(&id) == default_out.as_ref() { " (default)" } else { "" };
            out.push(DevInfo { id, label: format!("{name}{star}") });
        }
    }
    out.sort_by_key(|d| (!d.label.contains("(default)"), d.label.clone()));
    out
}

/// Dropping the handle asks the generator thread to stop (never blocks the UI).
pub struct GenHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for GenHandle {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, ms: u64) {
    let mut left = ms;
    while left > 0 && !stop.load(Relaxed) {
        let step = left.min(40);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
}

/// Starts the output stream; rebuilds it automatically if the device format changes or it is invalidated.
pub fn start(params: Arc<Mutex<GenParams>>, id: String, status: Arc<Mutex<String>>) -> GenHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("generator".into())
        .spawn(move || {
            while !stop2.load(Relaxed) {
                let dead = Arc::new(AtomicBool::new(false));
                match build(&params, &id, dead.clone()) {
                    Ok((stream, info, key)) => {
                        crate::log::log(&format!("generator: stream running: {info}"));
                        *status.lock() = info;
                        let mut last_poll = std::time::Instant::now();
                        let mut mismatches = 0;
                        while !stop2.load(Relaxed) && !dead.load(Relaxed) {
                            std::thread::sleep(Duration::from_millis(40));
                            // the driver does not always report a format change: poll it
                            if last_poll.elapsed() >= Duration::from_millis(750) {
                                last_poll = std::time::Instant::now();
                                if current_key(&id) == Some(key) {
                                    mismatches = 0;
                                } else {
                                    mismatches += 1;
                                    if mismatches >= 2 {
                                        crate::log::log(&format!("generator: device format now {:?}, built with {key:?} -> rebuilding", current_key(&id)));
                                        dead.store(true, Relaxed);
                                    }
                                }
                            }
                        }
                        if dead.load(Relaxed) && !stop2.load(Relaxed) {
                            std::mem::forget(stream);
                            *status.lock() = "Device format changed - reconnecting...".into();
                            sleep_unless_stopped(&stop2, 400);
                        } else {
                            dead.store(true, Relaxed);
                            drop(stream);
                        }
                    }
                    Err(e) => {
                        crate::log::log(&format!("generator: build failed: {e}"));
                        *status.lock() = format!("{e} - retrying...");
                        sleep_unless_stopped(&stop2, 1000);
                    }
                }
            }
        })
        .expect("spawn generator thread");
    GenHandle { stop }
}

/// (sample rate, channels, format) the output device currently offers; `None` if it cannot be queried.
fn current_key(id: &str) -> Option<(u32, u16, SampleFormat)> {
    let host = cpal::default_host();
    let did: cpal::DeviceId = id.parse().ok()?;
    let dev = host.device_by_id(&did)?;
    let cfg = dev.default_output_config().ok()?;
    Some((cfg.sample_rate(), cfg.channels(), cfg.sample_format()))
}

fn build(params: &Arc<Mutex<GenParams>>, id: &str, dead: Arc<AtomicBool>) -> Result<(cpal::Stream, String, (u32, u16, SampleFormat)), String> {
    let host = cpal::default_host();
    let did: cpal::DeviceId = id.parse().map_err(|e| format!("bad device id: {e:?}"))?;
    let dev = host.device_by_id(&did).ok_or_else(|| "device not found".to_string())?;
    let cfg = dev.default_output_config().map_err(|e| format!("{e}"))?;
    let (fmt, sr, ch) = (cfg.sample_format(), cfg.sample_rate(), cfg.channels());
    let sc: cpal::StreamConfig = cfg.into();
    let p = params.clone();
    let stream = match fmt {
        SampleFormat::F32 => make::<f32>(&dev, sc, p, sr, dead),
        SampleFormat::F64 => make::<f64>(&dev, sc, p, sr, dead),
        SampleFormat::I8 => make::<i8>(&dev, sc, p, sr, dead),
        SampleFormat::I16 => make::<i16>(&dev, sc, p, sr, dead),
        SampleFormat::I24 => make::<cpal::I24>(&dev, sc, p, sr, dead),
        SampleFormat::I32 => make::<i32>(&dev, sc, p, sr, dead),
        SampleFormat::I64 => make::<i64>(&dev, sc, p, sr, dead),
        SampleFormat::U8 => make::<u8>(&dev, sc, p, sr, dead),
        SampleFormat::U16 => make::<u16>(&dev, sc, p, sr, dead),
        SampleFormat::U24 => make::<cpal::U24>(&dev, sc, p, sr, dead),
        SampleFormat::U32 => make::<u32>(&dev, sc, p, sr, dead),
        SampleFormat::U64 => make::<u64>(&dev, sc, p, sr, dead),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("{e}"))?;
    stream.play().map_err(|e| format!("{e}"))?;
    Ok((stream, format!("{sr} Hz, {ch} ch, {fmt:?}"), (sr, ch, fmt)))
}

fn make<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, params: Arc<Mutex<GenParams>>, sr: u32, dead: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ch = cfg.channels as usize;
    let mut st = GenState::new(sr as f64);
    let mut last = params.lock().clone();
    let mut scratch: Vec<f32> = Vec::new();
    let dead_cb = dead.clone();
    dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &_| {
            // an abandoned stream (device changed) must go silent
            if dead_cb.load(Relaxed) {
                data.iter_mut().for_each(|d| *d = T::from_sample(0.0));
                return;
            }
            if let Some(g) = params.try_lock() {
                if *g != last {
                    last = g.clone();
                }
            }
            scratch.resize(data.len(), 0.0);
            st.render(&last, &mut scratch, ch);
            for (d, s) in data.iter_mut().zip(scratch.iter()) {
                *d = T::from_sample(*s);
            }
        },
        move |e| match e.kind() {
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceBusy => {}
            _ => {
                crate::log::log(&format!("generator: stream error {:?}: {e} -> rebuilding", e.kind()));
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
    fn sine_level_and_noise_rms() {
        let mut g = GenState::new(48000.0);
        let mut p = GenParams::default();
        p.on = true;
        p.level_db = [-6.0, -6.0];
        let mut buf = vec![0.0f32; 2 * 48000];
        g.render(&p, &mut buf, 2);
        let peak = buf[2 * 4800..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5012).abs() < 0.01, "peak {peak}");
        for k in NoiseKind::ALL {
            let mut g = GenState::new(48000.0);
            p.wave = Wave::Noise;
            p.noise = k;
            p.level_db = [-20.0, -20.0];
            let mut buf = vec![0.0f32; 2 * 96000];
            g.render(&p, &mut buf, 2);
            let tail = &buf[2 * 24000..];
            let rms = (tail.iter().step_by(2).map(|v| (*v as f64).powi(2)).sum::<f64>() / (tail.len() / 2) as f64).sqrt();
            let db = 20.0 * rms.log10();
            assert!((db + 20.0).abs() < 1.5, "{} rms {db}", k.name());
        }
    }
}
