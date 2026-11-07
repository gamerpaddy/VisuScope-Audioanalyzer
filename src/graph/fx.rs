//! Heavier effects of the graph: a constant phase shifter, a partitioned convolution (impulse responses,
//! reverbs, cabinets) and a spectral noise reducer. Same contract as `dsp`: stereo blocks in place, state
//! survives parameter changes.

use super::dsp::{db_to_lin, BqType, Biquad, Rs};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::f32::consts::PI;
use std::sync::Arc;

// ---------------------------------------------------------------------- phase shifter

const HILBERT_TAPS: usize = 511;
const HILBERT_DELAY: usize = (HILBERT_TAPS - 1) / 2;

/// Phase shift node: a polarity switch plus, optionally, a constant shift of every frequency (a Hilbert transformer,
/// which delays the signal by `latency()` frames) or an all-pass at a chosen frequency.
struct Layer {
    unit: Option<ConvUnit>,
    w: f32,
}

pub struct PhaseUnit {
    sign: f32,
    /// 0 polarity only, 1 constant shift, 2 all-pass, 3 drawn curve
    mode: usize,
    /// the drawn curve as linear-phase FIRs (mode 3). A change of the curve adds a layer that fades in over the older ones (a hard
    /// switch would cut the sound and make it stutter); the weights always add up to 1. `None` as the filter is the dry signal.
    layers: Vec<Layer>,
    /// the curve and rate the newest layer was made from
    curve_key: (String, u32),
    /// scratch: what each layer made of the block
    sc: Vec<[Vec<f32>; 2]>,
    /// the most recent input, to run a new filter over before it takes over (it would start from silence otherwise)
    recent: [Vec<f32>; 2],
    cos: f32,
    sin: f32,
    ap: Biquad,
    hilbert: Vec<f32>,
    /// history of both channels (a ring written twice, so the dot product runs on one slice)
    hist: [Vec<f32>; 2],
    pos: usize,
}

impl Default for PhaseUnit {
    fn default() -> Self {
        // only the odd taps of an ideal Hilbert transformer are not zero; a Blackman window tames the ripple
        let hilbert = (0..HILBERT_TAPS)
            .map(|k| {
                let m = k as isize - HILBERT_DELAY as isize;
                if m % 2 == 0 {
                    return 0.0;
                }
                let x = k as f32 / (HILBERT_TAPS - 1) as f32;
                let w = 0.42 - 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos();
                2.0 / (PI * m as f32) * w
            })
            .collect();
        Self { sign: 1.0, mode: 0, layers: Vec::new(), curve_key: (String::new(), 0), sc: Vec::new(), recent: [Vec::new(), Vec::new()], cos: 1.0, sin: 0.0, ap: Biquad::default(), hilbert, hist: [vec![0.0; HILBERT_TAPS * 2], vec![0.0; HILBERT_TAPS * 2]], pos: 0 }
    }
}

/// Points of a drawn phase curve: log-spaced from 20 Hz to 20 kHz, in degrees (-180 .. 180).
pub const CURVE_N: usize = 64;
const CURVE_TAPS: usize = 4096;

/// Frequency of point `i` of a drawn curve.
pub fn curve_freq(i: usize) -> f32 {
    20.0 * 1000f32.powf(i as f32 / (CURVE_N - 1) as f32)
}

/// A drawn curve as stored in the node ("deg;deg;..."); anything unreadable is a flat 0.
pub fn curve_parse(s: &str) -> Vec<f32> {
    let v: Vec<f32> = s.split(';').filter_map(|t| t.trim().parse::<f32>().ok()).map(|d| d.clamp(-180.0, 180.0)).collect();
    if v.len() == CURVE_N { v } else { vec![0.0; CURVE_N] }
}

pub fn curve_encode(v: &[f32]) -> String {
    if v.iter().all(|d| d.abs() < 0.05) {
        return String::new();
    }
    v.iter().map(|d| format!("{d:.1}")).collect::<Vec<_>>().join(";")
}

/// The phase (radians) of a drawn curve at `f` Hz: straight between the points on a log axis, flat beyond the ends.
fn curve_at(v: &[f32], f: f32) -> f32 {
    let t = ((f.max(1.0) / 20.0).ln() / 1000f32.ln() * (CURVE_N - 1) as f32).clamp(0.0, (CURVE_N - 1) as f32);
    let i = (t.floor() as usize).min(CURVE_N - 2);
    let a = t - i as f32;
    (v[i] * (1.0 - a) + v[i + 1] * a).to_radians()
}

impl PhaseUnit {
    /// `p`: polarity (0 normal, 1 inverted), mode, shift in degrees, frequency, Q.
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.sign = if p.first().copied().unwrap_or(0.0) >= 0.5 { -1.0 } else { 1.0 };
        self.mode = p.get(1).copied().unwrap_or(0.0).round().clamp(0.0, 3.0) as usize;
        let phi = p.get(2).copied().unwrap_or(0.0).to_radians();
        self.cos = phi.cos();
        self.sin = phi.sin();
        // 0 and 180 degrees are only a sign: no need to delay the signal for them
        if self.mode == 1 && self.sin.abs() < 1e-4 {
            self.sign *= self.cos.signum();
            self.mode = 0;
        }
        self.ap.set(BqType::AllPass, fs, p.get(3).copied().unwrap_or(1000.0), 0.0, p.get(4).copied().unwrap_or(0.7));
    }

    /// Make the filter of a drawn curve (only in that mode, and only when the curve or the rate changed).
    pub fn set_curve(&mut self, s: &str, fs: f32) {
        if self.mode != 3 {
            self.layers.clear();
            self.curve_key = (String::new(), 0);
            return;
        }
        let key = (s.to_string(), fs as u32);
        if self.curve_key == key {
            return;
        }
        let first = self.layers.is_empty();
        self.curve_key = key.clone();
        // a flat curve is "off": no filter, no delay
        if s.is_empty() {
            self.add_layer(None, first);
            return;
        }
        let v = curve_parse(s);
        let n = CURVE_TAPS;
        let mut spec = vec![Complex32::new(0.0, 0.0); n];
        for k in 0..=n / 2 {
            let ph = curve_at(&v, k as f32 * fs / n as f32);
            let h = if k == 0 || k == n / 2 { Complex32::new(ph.cos(), 0.0) } else { Complex32::new(ph.cos(), ph.sin()) };
            spec[k] = h;
            if k > 0 && k < n / 2 {
                spec[n - k] = h.conj();
            }
        }
        FftPlanner::<f32>::new().plan_fft_inverse(n).process(&mut spec);
        // the zero-phase response sits around tap 0: move it to the middle and window it
        let taps: Vec<f32> = (0..n)
            .map(|m| {
                let x = m as f32 / (n - 1) as f32;
                let w = 0.42 - 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos();
                spec[(m + n / 2) % n].re / n as f32 * w
            })
            .collect();
        let mut u = ConvUnit::new();
        u.load(vec![taps.clone(), taps], 1.0, 256, n as f32 / fs);
        u.set(&[100.0, 0.0]);
        // warm up on the recent input: its filter state is what the signal was a moment ago, not silence
        if !first {
            let (mut a, mut b) = (self.recent[0].clone(), self.recent[1].clone());
            u.process(&mut a, &mut b);
        }
        self.add_layer(Some(u), first);
    }

    /// A new filter takes over: at once for the first one, else it fades in over about 80 ms.
    fn add_layer(&mut self, unit: Option<ConvUnit>, first: bool) {
        if first {
            self.layers.clear();
            self.layers.push(Layer { unit, w: 1.0 });
            return;
        }
        self.layers.push(Layer { unit, w: 0.0 });
        // (a hand that draws changes the curve every frame: do not pile up layers)
        while self.layers.len() > 5 {
            let last = self.layers.len() - 1;
            if let Some((i, _)) = self.layers[..last].iter().enumerate().min_by(|a, b| a.1.w.total_cmp(&b.1.w)) {
                let w = self.layers.remove(i).w;
                // its weight goes to the newest older layer, so the sum stays 1
                let k = self.layers.len() - 2;
                self.layers[k].w += w;
            }
        }
    }

    /// Frames of delay the node adds in its current mode.
    pub fn latency(&self) -> usize {
        match self.mode {
            1 => HILBERT_DELAY,
            3 => self.layers.last().and_then(|l| l.unit.as_ref()).map_or(0, |c| c.latency() + CURVE_TAPS / 2),
            _ => 0,
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        match self.mode {
            2 => self.ap.process(l, r),
            3 => {
                // (remember the input: a new filter is run over it before it takes over)
                for (h, x) in self.recent.iter_mut().zip([&*l, &*r]) {
                    h.extend_from_slice(x);
                    let keep = CURVE_TAPS + 1024;
                    if h.len() > keep {
                        h.drain(..h.len() - keep);
                    }
                }
                let n = l.len();
                if self.layers.len() <= 1 {
                    if let Some(c) = self.layers.last_mut().and_then(|x| x.unit.as_mut()) {
                        c.process(l, r);
                    }
                } else {
                    let step = 1.0 / (0.08 * self.curve_key.1.max(8000) as f32);
                    let count = self.layers.len();
                    self.sc.resize_with(count, Default::default);
                    for (layer, sc) in self.layers.iter_mut().zip(self.sc.iter_mut()) {
                        for (dst, src) in sc.iter_mut().zip([&*l, &*r]) {
                            dst.clear();
                            dst.extend_from_slice(src);
                        }
                        if let Some(u) = layer.unit.as_mut() {
                            let [a, b] = sc;
                            u.process(a, b);
                        }
                    }
                    let last = count - 1;
                    for k in 0..n {
                        let prev = self.layers[last].w;
                        let now = (prev + step).min(1.0);
                        let scale = if prev < 1.0 { (1.0 - now) / (1.0 - prev) } else { 1.0 };
                        let (mut ol, mut or) = (0f32, 0f32);
                        for (i, (layer, sc)) in self.layers.iter_mut().zip(self.sc.iter()).enumerate() {
                            layer.w = if i == last { now } else { layer.w * scale };
                            ol += sc[0][k] * layer.w;
                            or += sc[1][k] * layer.w;
                        }
                        l[k] = ol;
                        r[k] = or;
                    }
                    // the layers that have faded out are dropped; when only the newest is left it goes on alone
                    let last = self.layers.len() - 1;
                    let mut i = 0;
                    self.layers.retain(|x| {
                        i += 1;
                        i - 1 == last || x.w > 1e-4
                    });
                    if self.layers.last().is_some_and(|x| x.w >= 0.9999) {
                        let keep = self.layers.pop().unwrap();
                        self.layers.clear();
                        self.layers.push(Layer { unit: keep.unit, w: 1.0 });
                    }
                }
            }
            1 => {
                // y = cos(phi) * x(n - D) + sin(phi) * H{x}(n - D): every frequency is turned by phi
                let start = self.pos;
                let mut end = start;
                for (c, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let h = &mut self.hist[c];
                    let mut w = start;
                    for v in ch.iter_mut() {
                        w = (w + 1) % HILBERT_TAPS;
                        h[w] = *v;
                        h[w + HILBERT_TAPS] = *v;
                        let win = &h[w + 1..w + 1 + HILBERT_TAPS];
                        let mut acc = 0f32;
                        for (a, b) in win.iter().step_by(2).zip(self.hilbert.iter().rev().step_by(2)) {
                            acc += a * b;
                        }
                        *v = self.cos * win[HILBERT_TAPS - 1 - HILBERT_DELAY] + self.sin * acc;
                    }
                    end = w;
                }
                self.pos = end;
            }
            _ => {}
        }
        if self.sign < 0.0 {
            l.iter_mut().for_each(|v| *v = -*v);
            r.iter_mut().for_each(|v| *v = -*v);
        }
    }
}

// ---------------------------------------------------------------------- frequency shifter

/// A Hilbert transformer of `taps` (odd) taps, windowed; `taps / 2` is its delay.
fn hilbert_taps(taps: usize) -> Vec<f32> {
    let d = (taps - 1) / 2;
    (0..taps)
        .map(|k| {
            let m = k as isize - d as isize;
            if m % 2 == 0 {
                return 0.0;
            }
            let x = k as f32 / (taps - 1) as f32;
            let w = 0.42 - 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos();
            2.0 / (PI * m as f32) * w
        })
        .collect()
}

const DEEP_TAPS: usize = 16383;
const DEEP_BLOCK: usize = 512;

/// Frequency shifter: every frequency of the signal moves by the same number of Hz (single sideband: the analytic signal times a
/// rotating phasor). Down by 30 kHz makes a 40 kHz bat call a 10 kHz chirp, up by 100 Hz makes 10 Hz infrasound audible as 110 Hz.
/// What would fall below 0 Hz (shifting down) or above half the sample rate (shifting up) is filtered off first.
pub struct ShiftUnit {
    up: bool,
    shift: f64,
    mix: f32,
    deep: bool,
    filter: bool,
    fs: f32,
    theta: f64,
    pre: super::dsp::FilterUnit,
    key: (bool, u32),
    // the short Hilbert transformer (direct form)
    hilbert: Vec<f32>,
    hist: [Vec<f32>; 2],
    pos: usize,
    // the long one (partitioned convolution) and the dry signal delayed to match
    conv: Option<ConvUnit>,
    dry: [Vec<f32>; 2],
    dpos: usize,
    scratch: [Vec<f32>; 2],
    scratch_im: [Vec<f32>; 2],
}

const SHORT_TAPS: usize = 511;

impl ShiftUnit {
    pub fn new() -> Self {
        Self {
            up: false,
            shift: 1000.0,
            mix: 1.0,
            deep: false,
            filter: true,
            fs: 48000.0,
            theta: 0.0,
            pre: Default::default(),
            key: (false, 0),
            hilbert: hilbert_taps(SHORT_TAPS),
            hist: [vec![0.0; SHORT_TAPS * 2], vec![0.0; SHORT_TAPS * 2]],
            pos: 0,
            conv: None,
            dry: [Vec::new(), Vec::new()],
            dpos: 0,
            scratch: [Vec::new(), Vec::new()],
            scratch_im: [Vec::new(), Vec::new()],
        }
    }

    /// `p`: direction (0 down, 1 up), shift in Hz, quality (0 short, 1 deep), filter (0 on), mix in %.
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.fs = fs;
        self.up = p.first().copied().unwrap_or(0.0) >= 0.5;
        self.shift = (p.get(1).copied().unwrap_or(1000.0) as f64).clamp(0.0, fs as f64 * 0.49);
        let deep = p.get(2).copied().unwrap_or(0.0) >= 0.5;
        self.filter = p.get(3).copied().unwrap_or(0.0) < 0.5;
        self.mix = p.get(4).copied().unwrap_or(100.0) * 0.01;
        if deep != self.deep || (deep && self.conv.is_none()) || self.key.1 != fs as u32 {
            self.deep = deep;
            self.key.1 = fs as u32;
            if deep {
                let mut c = ConvUnit::with_block(DEEP_BLOCK);
                let h = hilbert_taps(DEEP_TAPS);
                c.load(vec![h.clone(), h], 1.0, DEEP_BLOCK, DEEP_TAPS as f32 / fs);
                c.set(&[100.0, 0.0]);
                self.conv = Some(c);
                let len = self.latency();
                self.dry = [vec![0.0; len], vec![0.0; len]];
                self.dpos = 0;
            } else {
                self.conv = None;
            }
        }
        // what would fold over is removed before shifting
        if self.filter {
            let (low, f) = if self.up { (true, fs as f64 * 0.5 - self.shift) } else { (false, self.shift) };
            if f > 20.0 && f < fs as f64 * 0.47 {
                self.pre.set(&[if low { 1.0 } else { 0.0 }, f as f32, 1.0], fs);
            } else {
                self.pre = Default::default();
            }
        } else {
            self.pre = Default::default();
        }
    }

    /// Frames the node delays the signal by.
    pub fn latency(&self) -> usize {
        if self.deep { (DEEP_TAPS - 1) / 2 + DEEP_BLOCK } else { (SHORT_TAPS - 1) / 2 }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.filter {
            self.pre.process(l, r);
        }
        let n = l.len();
        // the analytic signal: the input delayed by the latency (re) and its Hilbert transform (im)
        // (buffers kept between blocks: nothing is allocated while audio runs)
        let (mut re, mut im) = ([std::mem::take(&mut self.scratch[0]), std::mem::take(&mut self.scratch[1])], [std::mem::take(&mut self.scratch_im[0]), std::mem::take(&mut self.scratch_im[1])]);
        for c in 0..2 {
            re[c].clear();
            re[c].resize(n, 0.0);
            im[c].clear();
            im[c].resize(n, 0.0);
        }
        if let Some(conv) = self.conv.as_mut() {
            im[0].copy_from_slice(l);
            im[1].copy_from_slice(r);
            let [i0, i1] = &mut im;
            conv.process(i0, i1);
            let len = self.dry[0].len();
            for (c, x) in [&*l, &*r].into_iter().enumerate() {
                let mut p = self.dpos;
                for i in 0..n {
                    re[c][i] = self.dry[c][p];
                    self.dry[c][p] = x[i];
                    p = (p + 1) % len;
                }
            }
            self.dpos = (self.dpos + n) % len;
        } else {
            let (start, d) = (self.pos, (SHORT_TAPS - 1) / 2);
            let mut end = start;
            for (c, x) in [&*l, &*r].into_iter().enumerate() {
                let h = &mut self.hist[c];
                let mut w = start;
                for i in 0..n {
                    w = (w + 1) % SHORT_TAPS;
                    h[w] = x[i];
                    h[w + SHORT_TAPS] = x[i];
                    let win = &h[w + 1..w + 1 + SHORT_TAPS];
                    let mut acc = 0f32;
                    for (a, b) in win.iter().step_by(2).zip(self.hilbert.iter().rev().step_by(2)) {
                        acc += a * b;
                    }
                    re[c][i] = win[SHORT_TAPS - 1 - d];
                    im[c][i] = acc;
                }
                end = w;
            }
            self.pos = end;
        }
        // the rotation: up = x cos - H sin, down = x cos + H sin
        let w = 2.0 * std::f64::consts::PI * self.shift / self.fs as f64;
        let sgn = if self.up { -1.0f32 } else { 1.0 };
        let mix = self.mix;
        for i in 0..n {
            let (s, c) = (self.theta.sin() as f32, self.theta.cos() as f32);
            self.theta = (self.theta + w) % (2.0 * std::f64::consts::PI);
            let (a, b) = (re[0][i] * c + sgn * im[0][i] * s, re[1][i] * c + sgn * im[1][i] * s);
            l[i] = re[0][i] * (1.0 - mix) + a * mix;
            r[i] = re[1][i] * (1.0 - mix) + b * mix;
        }
        self.scratch = re;
        self.scratch_im = im;
    }
}

// ---------------------------------------------------------------------- convolution

/// Uniformly partitioned convolution with an impulse response (overlap-save, one FFT per block and channel).
/// The output is `block` frames late; the dry signal is delayed by the same amount so the mix stays aligned.
pub struct ConvUnit {
    block: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    /// spectra of the impulse response partitions, per channel
    parts: [Vec<Vec<Complex32>>; 2],
    /// spectra of the most recent input blocks (newest first), per channel
    fdl: [std::collections::VecDeque<Vec<Complex32>>; 2],
    prev: [Vec<f32>; 2],
    inbuf: [Vec<f32>; 2],
    outbuf: [Vec<f32>; 2],
    dry: [Vec<f32>; 2],
    pos: usize,
    scratch: Vec<Complex32>,
    acc: Vec<Complex32>,
    mix: f32,
    gain: f32,
    pub loaded: bool,
    pub ir_secs: f32,
}

impl ConvUnit {
    pub fn new() -> Self {
        Self::with_block(256)
    }

    fn with_block(block: usize) -> Self {
        let mut pl = FftPlanner::<f32>::new();
        let n = block * 2;
        Self {
            block,
            fft: pl.plan_fft_forward(n),
            ifft: pl.plan_fft_inverse(n),
            parts: [Vec::new(), Vec::new()],
            fdl: [Default::default(), Default::default()],
            prev: [vec![0.0; block], vec![0.0; block]],
            inbuf: [vec![0.0; block], vec![0.0; block]],
            outbuf: [vec![0.0; block], vec![0.0; block]],
            dry: [vec![0.0; block], vec![0.0; block]],
            pos: 0,
            scratch: vec![Complex32::new(0.0, 0.0); n],
            acc: vec![Complex32::new(0.0, 0.0); n],
            mix: 0.5,
            gain: 1.0,
            loaded: false,
            ir_secs: 0.0,
        }
    }

    /// Longest impulse response that is used (seconds).
    pub const MAX_SECS: f32 = 6.0;

    /// Block size that keeps the cost reasonable for an IR of this length.
    fn block_for(secs: f32) -> usize {
        if secs < 0.6 { 256 } else if secs < 2.0 { 512 } else { 1024 }
    }

    /// Install an impulse response (`ch` has one or two channels at `ir_sr`); it is resampled to `fs`, limited in length
    /// and scaled so that its energy is 1 (noise keeps its level).
    pub fn set_ir(&mut self, ch: &[Vec<f32>], ir_sr: u32, fs: u32) {
        if ch.is_empty() || ch[0].is_empty() {
            self.loaded = false;
            return;
        }
        let mut chans: Vec<Vec<f32>> = Vec::new();
        for c in ch.iter().take(2) {
            let mut v = if ir_sr == fs {
                c.clone()
            } else {
                let mut rs = Rs::new(ir_sr, fs);
                rs.push(c, c);
                let n = rs.available();
                let (mut a, mut b) = (vec![0f32; n], vec![0f32; n]);
                let k = rs.pull(&mut a, &mut b);
                a.truncate(k);
                a
            };
            v.truncate((Self::MAX_SECS * fs as f32) as usize);
            chans.push(v);
        }
        if chans.len() == 1 {
            chans.push(chans[0].clone());
        }
        let len = chans[0].len().max(chans[1].len());
        let secs = len as f32 / fs as f32;
        let energy: f32 = chans.iter().map(|c| c.iter().map(|v| v * v).sum::<f32>()).sum::<f32>() / 2.0;
        let norm = if energy > 1e-12 { 1.0 / energy.sqrt() } else { 1.0 };
        self.load(chans, norm, Self::block_for(secs), secs);
    }

    /// Install two channels of taps as they are (no scaling) and with a given block size.
    fn load(&mut self, chans: Vec<Vec<f32>>, norm: f32, block: usize, secs: f32) {
        let len = chans[0].len().max(chans[1].len());
        let (mix, gain) = (self.mix, self.gain);
        *self = Self::with_block(block);
        self.mix = mix;
        self.gain = gain;
        self.ir_secs = secs;
        let n = block * 2;
        let nparts = len.div_ceil(block).max(1);
        for (c, v) in chans.iter().enumerate() {
            for p in 0..nparts {
                let mut buf = vec![Complex32::new(0.0, 0.0); n];
                for (k, slot) in buf.iter_mut().take(block).enumerate() {
                    *slot = Complex32::new(v.get(p * block + k).copied().unwrap_or(0.0) * norm, 0.0);
                }
                self.fft.process(&mut buf);
                self.parts[c].push(buf);
            }
            self.fdl[c] = (0..nparts).map(|_| vec![Complex32::new(0.0, 0.0); n]).collect();
        }
        self.loaded = true;
    }

    pub fn set(&mut self, p: &[f32]) {
        self.mix = p.first().copied().unwrap_or(50.0) * 0.01;
        self.gain = db_to_lin(p.get(1).copied().unwrap_or(0.0));
    }

    /// Frames the node delays the signal by.
    pub fn latency(&self) -> usize {
        self.block
    }

    fn run_block(&mut self, c: usize) {
        let b = self.block;
        // frame = previous block followed by the new one
        for k in 0..b {
            self.scratch[k] = Complex32::new(self.prev[c][k], 0.0);
            self.scratch[b + k] = Complex32::new(self.inbuf[c][k], 0.0);
        }
        self.prev[c].copy_from_slice(&self.inbuf[c]);
        self.fft.process(&mut self.scratch);
        // recycle the oldest spectrum for the newest block
        let mut spec = self.fdl[c].pop_back().unwrap_or_else(|| vec![Complex32::new(0.0, 0.0); b * 2]);
        spec.copy_from_slice(&self.scratch);
        self.fdl[c].push_front(spec);
        self.acc.iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0));
        for (h, x) in self.parts[c].iter().zip(self.fdl[c].iter()) {
            for ((a, hv), xv) in self.acc.iter_mut().zip(h.iter()).zip(x.iter()) {
                *a += hv * xv;
            }
        }
        self.ifft.process(&mut self.acc);
        let s = 1.0 / (b * 2) as f32;
        for k in 0..b {
            self.outbuf[c][k] = self.acc[b + k].re * s;
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.loaded {
            return;
        }
        let b = self.block;
        for i in 0..l.len() {
            for (c, v) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                self.inbuf[c][self.pos] = *v;
                let wet = self.outbuf[c][self.pos] * self.gain;
                let dry = self.dry[c][self.pos];
                self.dry[c][self.pos] = *v;
                *v = dry * (1.0 - self.mix) + wet * self.mix;
            }
            self.pos += 1;
            if self.pos == b {
                self.pos = 0;
                self.run_block(0);
                self.run_block(1);
            }
        }
    }
}

// ---------------------------------------------------------------------- noise reduction

const NR_N: usize = 512;
const NR_HOP: usize = NR_N / 4;

/// Spectral noise reduction (sqrt-Hann STFT, 75 % overlap). The noise floor is either followed (the quietest recent
/// level of each bin), learned while the mode says so, or frozen from the last learning.
pub struct NoiseRedUnit {
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    win: Vec<f32>,
    ch: [NrChan; 2],
    reduce: f32,
    over: f32,
    mode: usize,
    last_mode: usize,
    profile: Vec<f32>,
    learned: u32,
    pub learning: bool,
}

struct NrChan {
    inring: Vec<f32>,
    outacc: Vec<f32>,
    pos: usize,
    smooth: Vec<f32>,
    floor: Vec<f32>,
    gain: Vec<f32>,
    buf: Vec<Complex32>,
}

impl NrChan {
    fn new() -> Self {
        let bins = NR_N / 2 + 1;
        Self { inring: vec![0.0; NR_N], outacc: vec![0.0; NR_N], pos: 0, smooth: vec![0.0; bins], floor: vec![0.0; bins], gain: vec![1.0; bins], buf: vec![Complex32::new(0.0, 0.0); NR_N] }
    }
}

impl NoiseRedUnit {
    pub fn new() -> Self {
        let mut pl = FftPlanner::<f32>::new();
        let win = (0..NR_N).map(|i| (PI * (i as f32 + 0.5) / NR_N as f32).sin()).collect();
        Self {
            fft: pl.plan_fft_forward(NR_N),
            ifft: pl.plan_fft_inverse(NR_N),
            win,
            ch: [NrChan::new(), NrChan::new()],
            reduce: 0.25,
            over: 2.0,
            mode: 0,
            last_mode: 0,
            profile: vec![0.0; NR_N / 2 + 1],
            learned: 0,
            learning: false,
        }
    }

    /// Delay of the node in frames.
    pub fn latency(&self) -> usize {
        NR_N
    }

    /// `p`: reduction in dB, sensitivity (how far above the floor still counts as noise), mode (0 follow, 1 learn, 2 frozen).
    pub fn set(&mut self, p: &[f32]) {
        self.reduce = db_to_lin(-p.first().copied().unwrap_or(12.0).clamp(0.0, 40.0));
        self.over = 1.0 + p.get(1).copied().unwrap_or(50.0).clamp(0.0, 100.0) * 0.05;
        self.mode = p.get(2).copied().unwrap_or(0.0).round().clamp(0.0, 2.0) as usize;
        if self.mode == 1 && self.last_mode != 1 {
            self.profile.iter_mut().for_each(|v| *v = 0.0);
            self.learned = 0;
        }
        self.last_mode = self.mode;
        self.learning = self.mode == 1;
    }

    pub fn has_profile(&self) -> bool {
        self.learned > 0
    }

    fn frame(&mut self, c: usize) {
        let bins = NR_N / 2 + 1;
        let ch = &mut self.ch[c];
        // window the newest NR_N samples (oldest first)
        for k in 0..NR_N {
            let v = ch.inring[(ch.pos + k) % NR_N];
            ch.buf[k] = Complex32::new(v * self.win[k], 0.0);
        }
        self.fft.process(&mut ch.buf);
        let mut pw = [0f32; NR_N / 2 + 1];
        for k in 0..bins {
            pw[k] = ch.buf[k].norm_sqr();
        }
        match self.mode {
            1 => {
                self.learned += 1;
                let a = 1.0 / self.learned as f32;
                // averaged over both channels' frames (the profile is shared, which suits two microphones of one room)
                for k in 0..bins {
                    self.profile[k] += (pw[k] - self.profile[k]) * a.max(0.01);
                }
            }
            2 if self.learned > 0 => {
                ch.floor.copy_from_slice(&self.profile);
            }
            _ => {
                for k in 0..bins {
                    ch.smooth[k] += (pw[k] - ch.smooth[k]) * 0.3;
                    // follow the quietest level: fall at once, rise slowly (about 3 dB per second)
                    ch.floor[k] = if ch.smooth[k] < ch.floor[k] { ch.smooth[k] } else { ch.floor[k] * 1.002 + 1e-12 };
                }
            }
        }
        let bias = if self.mode == 0 { 1.6 } else { 1.0 };
        for k in 0..bins {
            let noise = ch.floor[k] * bias * self.over;
            let g = if pw[k] > 1e-14 { (1.0 - noise / pw[k]).max(0.0).sqrt() } else { 0.0 };
            let g = g.max(self.reduce);
            // gains rise quickly and fall a little slower: less musical noise
            ch.gain[k] = if g > ch.gain[k] { g } else { ch.gain[k] * 0.6 + g * 0.4 };
        }
        for k in 0..bins {
            ch.buf[k] *= ch.gain[k];
            if k > 0 && k < NR_N / 2 {
                ch.buf[NR_N - k] = ch.buf[k].conj();
            }
        }
        ch.buf[0].im = 0.0;
        ch.buf[NR_N / 2].im = 0.0;
        self.ifft.process(&mut ch.buf);
        // sqrt-Hann twice with 75 % overlap sums to 2: scale it away (and the FFT's factor N)
        let s = 1.0 / (NR_N as f32 * 2.0);
        for k in 0..NR_N {
            ch.outacc[(ch.pos + k) % NR_N] += ch.buf[k].re * self.win[k] * s;
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            for (c, v) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let ch = &mut self.ch[c];
                // `pos` is where the oldest sample of the current frame sits; the slot at pos is overwritten by the newest sample
                // after it was played out
                let at = ch.pos;
                let out = ch.outacc[at];
                ch.outacc[at] = 0.0;
                ch.inring[at] = *v;
                *v = out;
            }
            self.ch[0].pos = (self.ch[0].pos + 1) % NR_N;
            self.ch[1].pos = self.ch[0].pos;
            if self.ch[0].pos % NR_HOP == 0 {
                self.frame(0);
                self.frame(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, fs: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / fs).sin()).collect()
    }

    /// Amplitude and phase (radians) of one frequency by correlation.
    fn tone(x: &[f32], f: f32, fs: f32) -> (f32, f32) {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / fs as f64;
            re += *v as f64 * w.sin();
            im += *v as f64 * w.cos();
        }
        let n = x.len() as f64;
        ((2.0 * (re * re + im * im).sqrt() / n) as f32, im.atan2(re) as f32)
    }

    #[test]
    fn phase_node_inverts_and_turns_every_frequency() {
        let fs = 48000.0;
        let mut u = PhaseUnit::default();
        u.set(&[1.0, 0.0, 0.0, 1000.0, 0.7], fs);
        let mut l = sine(1000.0, fs, 4800, 0.5);
        let mut r = l.clone();
        let orig = l.clone();
        u.process(&mut l, &mut r);
        assert!(l.iter().zip(&orig).all(|(a, b)| (a + b).abs() < 1e-6), "polarity only is a plain inversion without delay");
        // 90 degrees at three frequencies: the level stays and the phase moves by the same amount
        let shift = 90f32.to_radians();
        for f in [400.0, 1000.0, 6000.0] {
            let mut u = PhaseUnit::default();
            u.set(&[0.0, 1.0, 90.0, 1000.0, 0.7], fs);
            let x = sine(f, fs, 24000, 0.5);
            let (mut l, mut r) = (x.clone(), x.clone());
            // blocks of odd sizes, as the engine delivers them
            let mut at = 0;
            for n in [1000usize, 777, 4000, 4096, 9999, 24000] {
                let e = (at + n).min(l.len());
                if at >= e {
                    break;
                }
                let (a, b) = (&mut l[at..e], &mut r[at..e]);
                u.process(a, b);
                at = e;
            }
            let lat = u.latency();
            assert_eq!(lat, HILBERT_DELAY);
            // compare the tail with the input delayed by the latency, over whole periods
            let n = (fs / f).round() as usize * (4000.0 / (fs / f)).max(1.0) as usize;
            let a = &l[24000 - n..];
            let (amp, ph) = tone(a, f, fs);
            let (ain, pin) = tone(&x[24000 - n - lat..24000 - lat], f, fs);
            assert!((amp / ain - 1.0).abs() < 0.03, "{f} Hz level {amp} vs {ain}");
            let d = (ph - pin).rem_euclid(2.0 * PI);
            let d = if d > PI { d - 2.0 * PI } else { d };
            assert!((d.abs() - shift).abs() < 0.1, "{f} Hz phase moved by {d}");
        }
    }

    #[test]
    fn all_pass_keeps_the_level_and_turns_180_degrees_at_its_frequency() {
        let fs = 48000.0;
        let mut u = PhaseUnit::default();
        u.set(&[0.0, 2.0, 0.0, 1000.0, 0.7], fs);
        let x = sine(1000.0, fs, 24000, 0.5);
        let (mut l, mut r) = (x.clone(), x.clone());
        u.process(&mut l, &mut r);
        let (amp, ph) = tone(&l[12000..], 1000.0, fs);
        let (_, pin) = tone(&x[12000..], 1000.0, fs);
        assert!((amp - 0.5).abs() < 0.01);
        let d = (ph - pin).rem_euclid(2.0 * PI);
        assert!((d - PI).abs() < 0.1, "{d}");
    }

    #[test]
    fn convolution_with_a_spike_is_a_delay_and_a_mix_blends_the_dry_signal() {
        let fs = 48000u32;
        for secs in [0.1f32, 1.0, 3.0] {
            let mut u = ConvUnit::new();
            let n = (secs * fs as f32) as usize;
            let mut ir = vec![0.0f32; n];
            ir[100] = 1.0;
            u.set_ir(&[ir], fs, fs);
            assert!(u.loaded);
            u.set(&[100.0, 0.0]);
            let mut l = vec![0.0f32; 20000];
            l[500] = 1.0;
            let mut r = l.clone();
            let mut at = 0;
            while at < l.len() {
                let e = (at + 480).min(l.len());
                let (a, b) = (&mut l[at..e], &mut r[at..e]);
                u.process(a, b);
                at = e;
            }
            let lat = u.latency();
            let peak = l.iter().enumerate().fold((0, 0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m });
            assert_eq!(peak.0, 500 + 100 + lat, "ir {secs}s");
            assert!((peak.1 - 1.0).abs() < 1e-3, "{}", peak.1);
        }
        // dry only
        let mut u = ConvUnit::new();
        u.set_ir(&[vec![0.0, 1.0, 0.5]], fs, fs);
        u.set(&[0.0, 0.0]);
        let mut l = sine(500.0, fs as f32, 4800, 0.3);
        let mut r = l.clone();
        let orig = l.clone();
        u.process(&mut l, &mut r);
        let lat = u.latency();
        assert!((lat..4800).all(|i| (l[i] - orig[i - lat]).abs() < 1e-5));
    }

    #[test]
    fn noise_reduction_removes_steady_noise_and_keeps_a_tone() {
        let fs = 48000.0;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.1
        };
        let mut u = NoiseRedUnit::new();
        // learn from noise alone
        u.set(&[30.0, 50.0, 1.0]);
        let mut l: Vec<f32> = (0..48000).map(|_| noise()).collect();
        let mut r = l.clone();
        u.process(&mut l, &mut r);
        assert!(u.learning);
        u.set(&[30.0, 50.0, 2.0]);
        assert!(u.has_profile());
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        // noise alone is much quieter afterwards
        let mut l: Vec<f32> = (0..48000).map(|_| noise()).collect();
        let mut r = l.clone();
        let before = rms(&l[24000..]);
        u.process(&mut l, &mut r);
        let after = rms(&l[24000..]);
        assert!(after < before * 0.35, "noise {before} -> {after}");
        // a tone in the same noise survives
        let tone_sig = sine(2000.0, fs, 48000, 0.3);
        let mut l: Vec<f32> = tone_sig.iter().map(|v| v + noise()).collect();
        let mut r = l.clone();
        u.process(&mut l, &mut r);
        let lat = u.latency();
        let out = &l[24000..];
        let (amp, _) = tone(out, 2000.0, fs);
        let _ = lat;
        assert!(amp > 0.25, "tone level {amp}");
    }
}

#[cfg(test)]
mod bench {
    use super::super::dsp::*;
    use super::*;
    use std::time::Instant;

    /// `cargo test --release bench_nodes -- --ignored --nocapture`: CPU time per second of stereo audio at 48 kHz.
    #[test]
    #[ignore]
    fn bench_nodes() {
        let fs = 48000.0;
        let n = 48000;
        let mk = || -> (Vec<f32>, Vec<f32>) { ((0..n).map(|i| (i as f32 * 0.05).sin() * 0.3).collect(), (0..n).map(|i| (i as f32 * 0.07).sin() * 0.3).collect()) };
        let time = |name: &str, f: &mut dyn FnMut(&mut [f32], &mut [f32])| {
            let (mut l, mut r) = mk();
            // blocks of 480 like the engine delivers
            let t = Instant::now();
            for _ in 0..3 {
                for (a, b) in l.chunks_mut(480).zip(r.chunks_mut(480)) {
                    f(a, b);
                }
            }
            println!("{name:28} {:6.2} ms per second of audio", t.elapsed().as_secs_f64() * 1000.0 / 3.0);
        };
        let mut eq = EqUnit::default();
        eq.set(&[0.0, 100.0, 3.0, 1.0, 0.0, 500.0, 3.0, 1.0, 0.0, 900.0, 3.0, 1.0, 0.0, 2000.0, 3.0, 1.0, 0.0, 4000.0, 3.0, 1.0, 0.0, 8000.0, 3.0, 1.0, 0.0, 100.0, 3.0, 1.0, 0.0, 500.0, 3.0, 1.0, 0.0, 900.0, 3.0, 1.0, 0.0, 2000.0, 3.0, 1.0, 0.0, 4000.0, 3.0, 1.0, 0.0, 8000.0, 3.0, 1.0], fs);
        time("EQ 12 bands", &mut |a, b| eq.process(a, b));
        let mut c = CompUnit::default();
        c.set(&[-30.0, 4.0, 10.0, 100.0, 6.0, 0.0], fs);
        time("compressor", &mut |a, b| c.process(a, b));
        let mut h = DistUnit::default();
        h.set(&[0.0, 4.0, 12000.0, 100.0, 0.0, 40.0, 40.0, 50.0], fs);
        time("harmonic series", &mut |a, b| h.process(a, b));
        let mut d = DistUnit::default();
        d.set(&[12.0, 0.0, 12000.0, 100.0, -6.0, 0.0, 0.0, 0.0], fs);
        time("distortion (soft)", &mut |a, b| d.process(a, b));
        let mut p = PhaseUnit::default();
        p.set(&[0.0, 1.0, 90.0, 1000.0, 0.7], fs);
        time("phase shift 90 deg", &mut |a, b| p.process(a, b));
        let mut nr = NoiseRedUnit::new();
        nr.set(&[12.0, 50.0, 0.0]);
        time("noise reduction", &mut |a, b| nr.process(a, b));
        for secs in [0.5f32, 2.0, 5.0] {
            let mut cv = ConvUnit::new();
            let ir: Vec<f32> = (0..(secs * fs) as usize).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
            cv.set_ir(&[ir], fs as u32, fs as u32);
            cv.set(&[50.0, 0.0]);
            time(&format!("convolution {secs} s"), &mut |a, b| cv.process(a, b));
        }
        let mut j = JitterUnit::new(fs);
        j.set(&[100.0, 8.0, 0.0, 100.0]);
        time("jitter", &mut |a, b| j.process(a, b));
        let mut dl = DelayUnit::new(2.0, fs);
        dl.set(&[250.0, 30.0, 50.0], fs);
        time("delay", &mut |a, b| dl.process(a, b));
    }
}

#[cfg(test)]
mod chunking {
    use super::super::dsp::*;
    use super::*;

    fn signal(n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut s = 0x2545_F491u32;
        let mut noise = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s as f32 / u32::MAX as f32 - 0.5
        };
        ((0..n).map(|i| (i as f32 * 0.031).sin() * 0.4 + noise() * 0.1).collect(), (0..n).map(|i| (i as f32 * 0.047).sin() * 0.3 + noise() * 0.1).collect())
    }

    /// The result must not depend on how the engine happens to cut the stream into blocks.
    fn same_for_every_block_size(name: &str, mut make: impl FnMut() -> Box<dyn FnMut(&mut [f32], &mut [f32])>) {
        let (l0, r0) = signal(20000);
        let mut reference: Option<(Vec<f32>, Vec<f32>)> = None;
        for blk in [20000usize, 1, 7, 480, 1000, 1024, 4097] {
            let (mut l, mut r) = (l0.clone(), r0.clone());
            let mut f = make();
            let mut at = 0;
            while at < l.len() {
                let e = (at + blk).min(l.len());
                f(&mut l[at..e], &mut r[at..e]);
                at = e;
            }
            match &reference {
                None => reference = Some((l, r)),
                Some((rl, rr)) => {
                    let d = l.iter().zip(rl).chain(r.iter().zip(rr)).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                    assert!(d < 1e-4, "{name}: block {blk} differs by {d}");
                }
            }
        }
    }

    #[test]
    fn phase_convolution_noise_reduction_and_distortion_do_not_depend_on_the_block_size() {
        let fs = 48000.0;
        same_for_every_block_size("phase 90", || {
            let mut u = PhaseUnit::default();
            u.set(&[0.0, 1.0, 90.0, 1000.0, 0.7], fs);
            Box::new(move |l, r| u.process(l, r))
        });
        same_for_every_block_size("phase all-pass + invert", || {
            let mut u = PhaseUnit::default();
            u.set(&[1.0, 2.0, 0.0, 800.0, 1.5], fs);
            Box::new(move |l, r| u.process(l, r))
        });
        for secs in [0.05f32, 1.0, 3.0] {
            same_for_every_block_size(&format!("convolution {secs} s"), || {
                let mut u = ConvUnit::new();
                let ir: Vec<f32> = (0..(secs * fs) as usize).map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * (-(i as f32) / (fs * secs * 0.3)).exp()).collect();
                let ir2: Vec<f32> = ir.iter().rev().copied().collect();
                u.set_ir(&[ir, ir2], fs as u32, fs as u32);
                u.set(&[60.0, 3.0]);
                Box::new(move |l, r| u.process(l, r))
            });
        }
        for deep in [0.0f32, 1.0] {
            same_for_every_block_size(&format!("frequency shifter {deep}"), || {
                let mut u = ShiftUnit::new();
                u.set(&[0.0, 1234.5, deep, 0.0, 80.0], fs);
                Box::new(move |l, r| u.process(l, r))
            });
        }
        same_for_every_block_size("noise reduction", || {
            let mut u = NoiseRedUnit::new();
            u.set(&[15.0, 60.0, 0.0]);
            Box::new(move |l, r| u.process(l, r))
        });
        same_for_every_block_size("harmonic series", || {
            let mut u = DistUnit::default();
            u.set(&[0.0, 4.0, 12000.0, 70.0, 0.0, 40.0, 25.0, 50.0], fs);
            Box::new(move |l, r| u.process(l, r))
        });
        same_for_every_block_size("distortion fold", || {
            let mut u = DistUnit::default();
            u.set(&[18.0, 3.0, 9000.0, 100.0, -6.0, 0.0, 0.0, 0.0], fs);
            Box::new(move |l, r| u.process(l, r))
        });
    }

    #[test]
    fn the_frequency_shifter_moves_every_frequency_by_the_same_amount() {
        let level = |x: &[f32], f: f32, fs: f32| {
            let (mut re, mut im) = (0f64, 0f64);
            for (i, v) in x.iter().enumerate() {
                let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / fs as f64;
                re += *v as f64 * w.cos();
                im += *v as f64 * w.sin();
            }
            (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
        };
        let run = |fs: f32, f_in: f32, p: [f32; 5], secs: f32| {
            let mut u = ShiftUnit::new();
            u.set(&p, fs);
            let n = (fs * secs) as usize;
            let mut l: Vec<f32> = (0..n).map(|i| 0.5 * (2.0 * PI * f_in * i as f32 / fs).sin()).collect();
            let mut r = l.clone();
            for (a, b) in l.chunks_mut(480).zip(r.chunks_mut(480)) {
                u.process(a, b);
            }
            let skip = n / 2;
            l[skip..].to_vec()
        };
        // up by 300 Hz at 48 kHz: 1000 Hz becomes 1300 Hz, nothing is left at 700 Hz (that would be a ring modulator) or at 1000 Hz
        let y = run(48000.0, 1000.0, [1.0, 300.0, 0.0, 0.0, 100.0], 1.0);
        let n = y.len();
        let (a, img, orig) = (level(&y[n - 24000..], 1300.0, 48000.0), level(&y[n - 24000..], 700.0, 48000.0), level(&y[n - 24000..], 1000.0, 48000.0));
        assert!(a > 0.4 && img < 0.03 && orig < 0.03, "up: shifted {a} image {img} original {orig}");
        // down by 20 kHz at 96 kHz: a 25 kHz bat call becomes 5 kHz; a 3 kHz tone below the shift is filtered, not folded
        let y = run(96000.0, 25000.0, [0.0, 20000.0, 0.0, 0.0, 100.0], 0.5);
        let n = y.len();
        assert!(level(&y[n - 20000..], 5000.0, 96000.0) > 0.4, "ultrasound heard at 5 kHz");
        let y = run(96000.0, 3000.0, [0.0, 20000.0, 0.0, 0.0, 100.0], 0.5);
        let n = y.len();
        assert!(level(&y[n - 20000..], 17000.0, 96000.0) < 0.05, "what falls below 0 Hz is filtered, not folded to 17 kHz");
        // the deep mode reaches infrasound: 20 Hz up by 200 Hz is 220 Hz
        let y = run(48000.0, 20.0, [1.0, 200.0, 1.0, 0.0, 100.0], 2.0);
        let n = y.len();
        let (a, img) = (level(&y[n - 24000..], 220.0, 48000.0), level(&y[n - 24000..], 180.0, 48000.0));
        assert!(a > 0.38 && img < 0.06, "infrasound: {a} / image {img}");
        // a zero shift with the mix at 0 % is the delayed input
        let y = run(48000.0, 1000.0, [1.0, 1.0, 0.0, 1.0, 0.0], 0.5);
        assert!(level(&y[y.len() - 10000..], 1000.0, 48000.0) > 0.45);
    }

    #[test]
    fn zero_degrees_changes_nothing_and_a_drawn_curve_turns_only_what_is_drawn() {
        let fs = 48000.0;
        let n = 24000;
        let tone = |f: f32| -> Vec<f32> { (0..n).map(|i| (2.0 * PI * f * i as f32 / fs).sin() * 0.5).collect() };
        // "all frequencies" at 0 degrees: no delay, the same samples
        let mut u = PhaseUnit::default();
        u.set(&[0.0, 1.0, 0.0, 1000.0, 0.7], fs);
        assert_eq!(u.latency(), 0);
        let (mut l, mut r) = (tone(1000.0), vec![0.0; n]);
        let x = l.clone();
        u.process(&mut l, &mut r);
        assert_eq!(l, x);
        // a curve: 90 degrees from 2 kHz up, nothing below 300 Hz; a 5 kHz tone is shifted by a quarter period, a 100 Hz tone is not
        let mut v = vec![0.0f32; CURVE_N];
        for (i, d) in v.iter_mut().enumerate() {
            let f = curve_freq(i);
            *d = if f >= 2000.0 { 90.0 } else if f > 300.0 { 90.0 * (f - 300.0) / 1700.0 } else { 0.0 };
        }
        let s = curve_encode(&v);
        // a flat curve is off: no delay, the same samples
        let mut off = PhaseUnit::default();
        off.set(&[0.0, 3.0, 0.0, 1000.0, 0.7], fs);
        off.set_curve("", fs);
        assert_eq!(off.latency(), 0);
        let (mut l, mut r) = (tone(1000.0), vec![0.0; n]);
        let x = l.clone();
        off.process(&mut l, &mut r);
        assert_eq!(l, x);
        let mut u = PhaseUnit::default();
        u.set(&[0.0, 3.0, 0.0, 1000.0, 0.7], fs);
        u.set_curve(&s, fs);
        let lat = u.latency();
        assert!(lat > 2000);
        let phase_of = |u: &mut PhaseUnit, f: f32, shifted: bool| -> f32 {
            let (mut l, mut r) = (tone(f), vec![0.0; n]);
            if shifted {
                u.process(&mut l, &mut r);
            }
            // correlate the tail with sine / cosine of the input (delay removed by the latency)
            let (mut a, mut b) = (0f32, 0f32);
            for i in 12000..n {
                let t = (i as f32 - if shifted { lat as f32 } else { 0.0 }) / fs;
                a += l[i] * (2.0 * PI * f * t).sin();
                b += l[i] * (2.0 * PI * f * t).cos();
            }
            b.atan2(a).to_degrees()
        };
        let d5k = phase_of(&mut u, 5000.0, true) - phase_of(&mut u, 5000.0, false);
        assert!((d5k - 90.0).abs() < 6.0, "5 kHz turned by {d5k}");
        let mut u2 = PhaseUnit::default();
        u2.set(&[0.0, 3.0, 0.0, 1000.0, 0.7], fs);
        u2.set_curve(&s, fs);
        let d100 = phase_of(&mut u2, 100.0, true) - phase_of(&mut u2, 100.0, false);
        assert!(d100.abs() < 6.0, "100 Hz turned by {d100}");
    }

    #[test]
    fn changing_the_drawn_curve_fades_instead_of_cutting_the_sound() {
        let fs = 48000.0;
        let mut u = PhaseUnit::default();
        u.set(&[0.0, 3.0, 0.0, 1000.0, 0.7], fs);
        let mut v = vec![0.0f32; CURVE_N];
        for d in v.iter_mut() {
            *d = 90.0;
        }
        u.set_curve(&curve_encode(&v), fs);
        let blk = 480;
        let mut worst = 0f32;
        let mut prev = 0f32;
        let mut t = 0usize;
        let mut out_all = Vec::new();
        for k in 0..400 {
            // the curve changes every few blocks, like while it is drawn
            if k % 7 == 3 {
                for d in v.iter_mut() {
                    *d = (*d + 25.0).min(180.0) % 180.0;
                }
                u.set_curve(&curve_encode(&v), fs);
            }
            let mut l: Vec<f32> = (0..blk).map(|i| 0.5 * (2.0 * PI * 500.0 * (t + i) as f32 / fs).sin()).collect();
            let mut r = l.clone();
            u.process(&mut l, &mut r);
            t += blk;
            for x in &l {
                if k > 60 {
                    worst = worst.max((x - prev).abs());
                }
                prev = *x;
            }
            out_all.extend_from_slice(&l);
        }
        // a 500 Hz sine of 0.5 moves by at most 0.033 per sample; a cut or a restart would jump far more than that
        assert!(worst < 0.12, "jump {worst}");
        // and the sound never dies out
        let tail = &out_all[out_all.len() - 24000..];
        let rms = (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(rms > 0.2, "rms {rms}");
    }

    #[test]
    fn the_stereo_channels_stay_separate_in_the_new_effects() {
        let fs = 48000.0;
        for k in 0..4 {
            let n = 12000;
            let mut l: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
            let mut r = vec![0f32; n];
            match k {
                0 => {
                    let mut u = PhaseUnit::default();
                    u.set(&[0.0, 1.0, 45.0, 1000.0, 0.7], fs);
                    u.process(&mut l, &mut r);
                }
                1 => {
                    let mut u = ConvUnit::new();
                    u.set_ir(&[(0..2000).map(|i| (i as f32 * 0.1).sin() * (-(i as f32) / 500.0).exp()).collect()], 48000, 48000);
                    u.set(&[100.0, 0.0]);
                    u.process(&mut l, &mut r);
                }
                2 => {
                    let mut u = NoiseRedUnit::new();
                    u.set(&[12.0, 50.0, 0.0]);
                    u.process(&mut l, &mut r);
                }
                _ => {
                    let mut u = DistUnit::default();
                    u.set(&[0.0, 4.0, 12000.0, 100.0, 0.0, 40.0, 40.0, 50.0], fs);
                    u.process(&mut l, &mut r);
                }
            }
            assert!(r.iter().all(|v| v.abs() < 1e-6), "effect {k}: the silent right channel got signal");
            assert!(l.iter().any(|v| v.abs() > 0.05), "effect {k}: the left channel is silent");
        }
    }

    #[test]
    fn the_band_listen_filter_passes_its_band_and_stops_the_rest() {
        let fs = 48000.0f32;
        let band = |f: f32| {
            crate::shared::BAND.set(true, 500.0, 2000.0);
            let mut u = BandFilter::default();
            let x: Vec<f32> = (0..24000).map(|i| 0.5 * (2.0 * PI * f * i as f32 / fs).sin()).collect();
            let (mut l, mut r) = (x.clone(), x.clone());
            for (a, b) in l.chunks_mut(480).zip(r.chunks_mut(480)) {
                u.process(a, b, fs);
            }
            crate::shared::BAND.set(false, 0.0, 0.0);
            (l[12000..].iter().map(|v| v * v).sum::<f32>() / 12000.0).sqrt() / (0.5 / 2f32.sqrt())
        };
        // the test shares a global switch with nothing else that runs audio, but keep it in one test
        assert!(band(1000.0) > 0.85, "in band {}", band(1000.0));
        assert!(band(60.0) < 0.05, "below {}", band(60.0));
        assert!(band(12000.0) < 0.05, "above {}", band(12000.0));
        // off: untouched
        let mut u = BandFilter::default();
        let mut l = vec![0.3f32; 100];
        let mut r = l.clone();
        u.process(&mut l, &mut r, fs);
        assert!(l.iter().all(|v| *v == 0.3));
    }
}
