//! Signal processing of the graph nodes. Every unit works on a stereo block in place and keeps its
//! state when its parameters change, so knobs can be turned while audio runs.

use std::collections::VecDeque;
use std::f32::consts::PI;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

// ---------------------------------------------------------------------- biquad

#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
    pub active: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BqType {
    Bell,
    LowShelf,
    HighShelf,
    HighPass,
    LowPass,
    Notch,
}

impl Biquad {
    /// RBJ cookbook coefficients; the filter state is kept.
    pub fn set(&mut self, t: BqType, fs: f32, f: f32, gain_db: f32, q: f32) {
        let f = f.clamp(10.0, fs * 0.49);
        let q = q.max(0.05);
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / fs;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match t {
            BqType::Bell => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
            BqType::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (a * ((a + 1.0) - (a - 1.0) * cw + k), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - k), (a + 1.0) + (a - 1.0) * cw + k, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - k)
            }
            BqType::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (a * ((a + 1.0) + (a - 1.0) * cw + k), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - k), (a + 1.0) - (a - 1.0) * cw + k, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - k)
            }
            BqType::HighPass => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BqType::LowPass => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BqType::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
        self.active = true;
    }

    #[inline]
    fn tick(&mut self, x: f32, c: usize) -> f32 {
        let y = self.b0 * x + self.z1[c];
        self.z1[c] = self.b1 * x - self.a1 * y + self.z2[c];
        self.z2[c] = self.b2 * x - self.a2 * y;
        y
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.active {
            return;
        }
        for v in l.iter_mut() {
            *v = self.tick(*v, 0);
        }
        for v in r.iter_mut() {
            *v = self.tick(*v, 1);
        }
    }

    /// Magnitude response in dB at `f` (for drawing the curve of an EQ node).
    pub fn response_db(&self, fs: f32, f: f32) -> f32 {
        if !self.active {
            return 0.0;
        }
        let w = 2.0 * PI * f / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-20)).max(1e-20).log10()
    }
}

/// Most bands an equaliser node can have.
pub const EQ_MAX_BANDS: usize = 12;

/// Parametric equaliser with up to `EQ_MAX_BANDS` bands (the node's parameters hold four values per band).
#[derive(Clone, Default)]
pub struct EqUnit {
    pub bands: Vec<Biquad>,
}

impl EqUnit {
    /// `p` is the node's parameter list: type, freq, gain, Q per band.
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.bands.resize((p.len() / 4).clamp(1, EQ_MAX_BANDS), Biquad::default());
        for (i, b) in self.bands.iter_mut().enumerate() {
            let o = i * 4;
            let (t, f, g, q) = (p.get(o).copied().unwrap_or(6.0).round() as usize, p.get(o + 1).copied().unwrap_or(1000.0), p.get(o + 2).copied().unwrap_or(0.0), p.get(o + 3).copied().unwrap_or(1.0));
            let ty = match t {
                0 => BqType::Bell,
                1 => BqType::LowShelf,
                2 => BqType::HighShelf,
                3 => BqType::HighPass,
                4 => BqType::LowPass,
                // a notch is a narrow dip: its depth is the (negative) level of the band
                5 => BqType::Bell,
                _ => {
                    b.active = false;
                    continue;
                }
            };
            let g = if t == 5 { -g.abs() } else { g };
            b.set(ty, fs, f, g, q);
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for b in self.bands.iter_mut() {
            b.process(l, r);
        }
    }

    pub fn response_db(&self, fs: f32, f: f32) -> f32 {
        self.bands.iter().map(|b| b.response_db(fs, f)).sum()
    }
}

/// High-pass or low-pass of 12, 24 or 48 dB/oct (cascaded Butterworth sections).
#[derive(Clone, Default)]
pub struct FilterUnit {
    stages: Vec<Biquad>,
}

impl FilterUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        let low = p.first().copied().unwrap_or(0.0).round() as usize == 1;
        let f = p.get(1).copied().unwrap_or(80.0);
        let n = [1usize, 2, 4][(p.get(2).copied().unwrap_or(1.0).round() as usize).min(2)];
        self.stages.resize(n, Biquad::default());
        // Butterworth: section k of 2n poles has Q = 1 / (2 cos((2k+1) pi / (4n)))
        for (k, s) in self.stages.iter_mut().enumerate() {
            let q = 1.0 / (2.0 * ((2 * k + 1) as f32 * PI / (4.0 * n as f32)).cos());
            s.set(if low { BqType::LowPass } else { BqType::HighPass }, fs, f, 0.0, q);
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for s in self.stages.iter_mut() {
            s.process(l, r);
        }
    }
}

// ---------------------------------------------------------------------- dynamics

#[inline]
fn coef(ms: f32, fs: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * fs)).exp()
}

/// Stereo-linked feed-forward compressor with a soft knee; `gr_db` is the current gain reduction.
#[derive(Clone, Default)]
pub struct CompUnit {
    thr: f32,
    ratio: f32,
    knee: f32,
    makeup: f32,
    att: f32,
    rel: f32,
    g: f32,
    pub gr_db: f32,
}

impl CompUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.thr = p.first().copied().unwrap_or(-18.0);
        self.ratio = p.get(1).copied().unwrap_or(4.0).max(1.0);
        self.att = coef(p.get(2).copied().unwrap_or(10.0), fs);
        self.rel = coef(p.get(3).copied().unwrap_or(120.0), fs);
        self.knee = p.get(4).copied().unwrap_or(6.0).max(0.0);
        self.makeup = p.get(5).copied().unwrap_or(0.0);
    }

    /// Gain change in dB for an input level in dB (static curve).
    pub fn curve(&self, x: f32) -> f32 {
        let over = x - self.thr;
        let k = self.knee;
        let y = if k > 0.0 && over.abs() <= k * 0.5 {
            let t = over + k * 0.5;
            x + (1.0 / self.ratio - 1.0) * t * t / (2.0 * k)
        } else if over > 0.0 {
            self.thr + over / self.ratio
        } else {
            x
        };
        y - x
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let mut worst = 0f32;
        for i in 0..l.len() {
            let level = l[i].abs().max(r[i].abs());
            let target = if level < 1e-6 { 0.0 } else { self.curve(lin_to_db(level)) };
            // reduction grows with the attack time constant and recovers with the release one
            let c = if target < self.g { self.att } else { self.rel };
            self.g = c * self.g + (1.0 - c) * target;
            let gain = db_to_lin(self.g + self.makeup);
            l[i] *= gain;
            r[i] *= gain;
            worst = worst.min(self.g);
        }
        self.gr_db = worst;
    }
}

/// Look-ahead peak limiter; nothing leaves it above the ceiling.
#[derive(Clone)]
pub struct LimiterUnit {
    ceil: f32,
    rel: f32,
    att: f32,
    g: f32,
    la: usize,
    buf: [Vec<f32>; 2],
    pos: usize,
    pub gr_db: f32,
}

impl Default for LimiterUnit {
    fn default() -> Self {
        Self { ceil: 0.89, rel: 0.999, att: 0.9, g: 1.0, la: 48, buf: [vec![0.0; 48], vec![0.0; 48]], pos: 0, gr_db: 0.0 }
    }
}

impl LimiterUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.ceil = db_to_lin(p.first().copied().unwrap_or(-1.0));
        self.rel = coef(p.get(1).copied().unwrap_or(60.0), fs);
        let la = ((fs * 0.001) as usize).max(8);
        if la != self.la {
            self.la = la;
            self.buf = [vec![0.0; la], vec![0.0; la]];
            self.pos = 0;
        }
        // the gain falls within the look-ahead time
        self.att = coef(0.25, fs);
    }

    /// Delay of the limiter in frames.
    pub fn latency(&self) -> usize {
        self.la
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let mut worst = 1f32;
        for i in 0..l.len() {
            let peak = l[i].abs().max(r[i].abs());
            let want = if peak > self.ceil { self.ceil / peak } else { 1.0 };
            if want < self.g {
                // the gain reaches the target within the look-ahead time
                self.g = self.att * self.g + (1.0 - self.att) * want;
            } else {
                self.g = self.rel * self.g + (1.0 - self.rel) * want;
            }
            let (dl, dr) = (self.buf[0][self.pos], self.buf[1][self.pos]);
            self.buf[0][self.pos] = l[i];
            self.buf[1][self.pos] = r[i];
            self.pos = (self.pos + 1) % self.la;
            // the gain computed for the newest sample applies to the delayed ones; clamp as a last resort
            l[i] = (dl * self.g).clamp(-self.ceil, self.ceil);
            r[i] = (dr * self.g).clamp(-self.ceil, self.ceil);
            worst = worst.min(self.g);
        }
        self.gr_db = lin_to_db(worst);
    }
}

#[derive(Clone, Default)]
pub struct GateUnit {
    thr: f32,
    range: f32,
    att: f32,
    rel: f32,
    env: f32,
    g: f32,
    pub open: bool,
}

impl GateUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.thr = db_to_lin(p.first().copied().unwrap_or(-50.0));
        self.att = coef(p.get(1).copied().unwrap_or(2.0), fs);
        self.rel = coef(p.get(2).copied().unwrap_or(120.0), fs);
        self.range = db_to_lin(p.get(3).copied().unwrap_or(-80.0));
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            let level = l[i].abs().max(r[i].abs());
            // fast attack, slow decay envelope
            self.env = if level > self.env { level } else { self.env * 0.9995 + level * 0.0005 };
            let target = if self.env > self.thr { 1.0 } else { self.range };
            let c = if target > self.g { self.att } else { self.rel };
            self.g = c * self.g + (1.0 - c) * target;
            l[i] *= self.g;
            r[i] *= self.g;
        }
        self.open = self.g > 0.5;
    }
}

// ---------------------------------------------------------------------- time and space

/// Delay line with feedback and a dry / wet mix.
#[derive(Clone, Default)]
pub struct DelayUnit {
    buf: [Vec<f32>; 2],
    pos: usize,
    n: usize,
    fb: f32,
    mix: f32,
}

impl DelayUnit {
    pub fn new(max_secs: f32, fs: f32) -> Self {
        let cap = ((max_secs * fs) as usize + 2).max(4);
        Self { buf: [vec![0.0; cap], vec![0.0; cap]], pos: 0, n: 0, fb: 0.0, mix: 1.0 }
    }

    pub fn set_time(&mut self, ms: f32, fs: f32) {
        self.n = ((ms * 0.001 * fs) as usize).min(self.buf[0].len() - 1);
    }

    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.set_time(p.first().copied().unwrap_or(250.0), fs);
        self.fb = p.get(1).copied().unwrap_or(30.0) * 0.01;
        self.mix = p.get(2).copied().unwrap_or(50.0) * 0.01;
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let cap = self.buf[0].len();
        for i in 0..l.len() {
            let rp = (self.pos + cap - self.n) % cap;
            let (dl, dr) = (self.buf[0][rp], self.buf[1][rp]);
            self.buf[0][self.pos] = l[i] + dl * self.fb;
            self.buf[1][self.pos] = r[i] + dr * self.fb;
            self.pos = (self.pos + 1) % cap;
            l[i] = l[i] * (1.0 - self.mix) + dl * self.mix;
            r[i] = r[i] * (1.0 - self.mix) + dr * self.mix;
        }
    }
}

/// Mid / side width and balance.
pub fn stereo_width(l: &mut [f32], r: &mut [f32], width: f32, balance: f32) {
    let (gl, gr) = (1.0 - balance.max(0.0), 1.0 + balance.min(0.0));
    for i in 0..l.len() {
        let (m, s) = (0.5 * (l[i] + r[i]), 0.5 * (l[i] - r[i]) * width);
        l[i] = (m + s) * gl;
        r[i] = (m - s) * gr;
    }
}

/// The "Channel utility" modes.
pub fn utility(l: &mut [f32], r: &mut [f32], mode: usize) {
    match mode {
        1 => r.copy_from_slice(l),
        2 => l.copy_from_slice(r),
        3 => {
            for i in 0..l.len() {
                let m = 0.5 * (l[i] + r[i]);
                l[i] = m;
                r[i] = m;
            }
        }
        4 => {
            for i in 0..l.len() {
                std::mem::swap(&mut l[i], &mut r[i]);
            }
        }
        5 => l.iter_mut().for_each(|v| *v = -*v),
        6 => r.iter_mut().for_each(|v| *v = -*v),
        7 => {
            l.iter_mut().for_each(|v| *v = -*v);
            r.iter_mut().for_each(|v| *v = -*v);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------- distortion family

/// One-pole low-pass (the tone control of the distortions) and a DC blocker, per channel.
#[derive(Clone, Copy, Default)]
struct Tone {
    a: f32,
    z: [f32; 2],
}

impl Tone {
    fn set(&mut self, hz: f32, fs: f32) {
        self.a = 1.0 - (-2.0 * PI * hz.clamp(20.0, fs * 0.45) / fs).exp();
    }
    #[inline]
    fn tick(&mut self, x: f32, c: usize) -> f32 {
        self.z[c] += self.a * (x - self.z[c]);
        self.z[c]
    }
}

#[derive(Clone, Copy, Default)]
struct DcBlock {
    z: [f32; 2],
}

impl DcBlock {
    #[inline]
    fn tick(&mut self, x: f32, c: usize) -> f32 {
        // removes what is slower than about 10 Hz
        self.z[c] += 0.0015 * (x - self.z[c]);
        x - self.z[c]
    }
}

/// Adds a series of even harmonics (2nd, 4th, 6th ...) and / or of odd ones (3rd, 5th, 7th ...) to the signal.
/// The input is first normalised by its own level, so the harmonics keep the same size relative to the signal whether it is
/// loud or quiet; at 100 % their total is about half the level of the signal. "Richness" decides how far up the series reaches.
#[derive(Clone)]
pub struct HarmUnit {
    even: f32,
    odd: f32,
    out: f32,
    /// shape of the even term (exponent of |x|), drive of the odd term, and the gains / constants that make each
    /// term a pure series of harmonics of a fixed size
    pe: f32,
    k_odd: f32,
    mean_e: f32,
    f1_odd: f32,
    ge: f32,
    go: f32,
    env: [f32; 2],
    release: f32,
    dc: DcBlock,
}

impl Default for HarmUnit {
    fn default() -> Self {
        let mut u = Self { even: 0.3, odd: 0.0, out: 1.0, pe: 1.0, k_odd: 3.0, mean_e: 0.0, f1_odd: 0.0, ge: 1.0, go: 1.0, env: [0.0; 2], release: 0.999, dc: DcBlock::default() };
        u.tune(0.5);
        u
    }
}

impl HarmUnit {
    /// Work out the constants of the two shapers for a richness between 0 and 1 (numerically, over one cycle).
    fn tune(&mut self, rich: f32) {
        self.pe = 1.0 - 0.75 * rich;
        self.k_odd = 1.5 + 12.0 * rich;
        const N: usize = 1024;
        let (mut me, mut f1) = (0f64, 0f64);
        for i in 0..N {
            let s = (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / N as f64).sin();
            me += s.abs().powf(self.pe as f64);
            f1 += (self.k_odd as f64 * s).tanh() * s;
        }
        self.mean_e = (me / N as f64) as f32;
        self.f1_odd = (2.0 * f1 / N as f64) as f32;
        let (mut se, mut so) = (0f64, 0f64);
        for i in 0..N {
            let s = (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / N as f64).sin() as f32;
            let (he, ho) = (s.abs().powf(self.pe) - self.mean_e, (self.k_odd * s).tanh() - self.f1_odd * s);
            se += (he * he) as f64;
            so += (ho * ho) as f64;
        }
        // the harmonics of one term add up to an RMS of 0.35 (relative to a signal of amplitude 1: that is half its RMS)
        self.ge = 0.35 / ((se / N as f64).sqrt() as f32).max(1e-6);
        self.go = 0.35 / ((so / N as f64).sqrt() as f32).max(1e-6);
    }

    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.even = p.first().copied().unwrap_or(30.0) * 0.01;
        self.odd = p.get(1).copied().unwrap_or(0.0) * 0.01;
        let rich = p.get(2).copied().unwrap_or(50.0) * 0.01;
        self.out = db_to_lin(p.get(3).copied().unwrap_or(0.0));
        self.release = (-1.0 / (0.06 * fs.max(1000.0))).exp();
        self.tune(rich.clamp(0.0, 1.0));
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (c, ch) in [l, r].into_iter().enumerate() {
            for v in ch.iter_mut() {
                let x = *v;
                let a = x.abs();
                // fast attack, slow release
                self.env[c] = if a > self.env[c] { a } else { self.env[c] * self.release };
                let env = self.env[c];
                if env < 1e-5 {
                    *v = x * self.out;
                    continue;
                }
                let u = (x / env).clamp(-1.0, 1.0);
                let mut h = 0.0;
                if self.even > 0.0 {
                    let e = u.abs().powf(self.pe) - self.mean_e;
                    h += self.even * self.ge * self.dc.tick(e, c);
                }
                if self.odd > 0.0 {
                    h += self.odd * self.go * ((self.k_odd * u).tanh() - self.f1_odd * u);
                }
                *v = (x + env * h) * self.out;
            }
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
pub enum DistShape {
    Soft,
    Hard,
    Asym,
    Fold,
}

#[inline]
fn shape(x: f32, s: DistShape) -> f32 {
    match s {
        DistShape::Soft => x.tanh(),
        DistShape::Hard => x.clamp(-1.0, 1.0),
        // a bias before the curve makes the two halves differ: even harmonics
        DistShape::Asym => (x + 0.35).tanh() - 0.35f32.tanh(),
        DistShape::Fold => {
            let t = (x + 1.0).rem_euclid(4.0);
            if t < 2.0 { t - 1.0 } else { 3.0 - t }
        }
    }
}

/// Overdrive / distortion: gain, a shaping curve, a tone low-pass and a dry / wet mix.
#[derive(Clone)]
pub struct DistUnit {
    drive: f32,
    shape: DistShape,
    mix: f32,
    out: f32,
    tone: Tone,
    dc: DcBlock,
}

impl Default for DistUnit {
    fn default() -> Self {
        Self { drive: 4.0, shape: DistShape::Soft, mix: 1.0, out: 0.5, tone: Tone::default(), dc: DcBlock::default() }
    }
}

impl DistUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.drive = db_to_lin(p.first().copied().unwrap_or(12.0));
        self.shape = match p.get(1).copied().unwrap_or(0.0).round() as usize {
            1 => DistShape::Hard,
            2 => DistShape::Asym,
            3 => DistShape::Fold,
            _ => DistShape::Soft,
        };
        self.tone.set(p.get(2).copied().unwrap_or(12000.0), fs);
        self.mix = p.get(3).copied().unwrap_or(100.0) * 0.01;
        self.out = db_to_lin(p.get(4).copied().unwrap_or(-6.0));
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (c, ch) in [l, r].into_iter().enumerate() {
            for v in ch.iter_mut() {
                let dry = *v;
                let mut y = shape(dry * self.drive, self.shape);
                y = self.dc.tick(y, c);
                y = self.tone.tick(y, c) * self.out;
                *v = dry * (1.0 - self.mix) + y * self.mix;
            }
        }
    }
}

/// Fuzz: a lot of gain into an uneven clipper, with a gate that lets the fuzz sputter when the input fades.
#[derive(Clone, Default)]
pub struct FuzzUnit {
    gain: f32,
    bias: f32,
    gate: f32,
    out: f32,
    tone: Tone,
    dc: DcBlock,
    env: f32,
    g: f32,
}

impl FuzzUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        let amt = p.first().copied().unwrap_or(70.0) * 0.01;
        self.gain = 1.0 + amt * amt * 120.0;
        self.bias = 0.25 * amt;
        // 0 % = open, 100 % = closes below about -25 dB
        self.gate = db_to_lin(-70.0 + 45.0 * p.get(1).copied().unwrap_or(10.0) * 0.01) * if p.get(1).copied().unwrap_or(10.0) > 0.5 { 1.0 } else { 0.0 };
        self.tone.set(p.get(2).copied().unwrap_or(5000.0), fs);
        self.out = db_to_lin(p.get(3).copied().unwrap_or(-10.0));
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            let level = l[i].abs().max(r[i].abs());
            self.env = if level > self.env { level } else { self.env * 0.9995 };
            let target = if self.env > self.gate { 1.0 } else { 0.0 };
            self.g += (target - self.g) * if target > self.g { 0.02 } else { 0.002 };
            for (c, v) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let x = *v * self.gain + self.bias;
                // the negative half clips earlier than the positive one
                let y = if x > 0.0 { x.min(1.0) } else { x.max(-0.7) };
                let y = self.dc.tick(y, c) * self.g;
                *v = self.tone.tick(y, c) * self.out;
            }
        }
    }
}

/// Bit crusher: fewer bits and / or a held sample (sample rate reduction).
#[derive(Clone, Default)]
pub struct CrushUnit {
    levels: f32,
    hold: usize,
    mix: f32,
    count: usize,
    held: [f32; 2],
}

impl CrushUnit {
    pub fn set(&mut self, p: &[f32]) {
        let bits = p.first().copied().unwrap_or(8.0).round().clamp(1.0, 24.0);
        self.levels = 2f32.powf(bits - 1.0);
        self.hold = (p.get(1).copied().unwrap_or(1.0).round() as usize).clamp(1, 64);
        self.mix = p.get(2).copied().unwrap_or(100.0) * 0.01;
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            if self.count == 0 {
                // 2^bits levels: -levels ..= levels - 1
                let q = |x: f32| (x * self.levels).round().clamp(-self.levels, self.levels - 1.0) / self.levels;
                self.held = [q(l[i]), q(r[i])];
            }
            self.count = (self.count + 1) % self.hold;
            l[i] = l[i] * (1.0 - self.mix) + self.held[0] * self.mix;
            r[i] = r[i] * (1.0 - self.mix) + self.held[1] * self.mix;
        }
    }
}

/// Timing jitter / wow and flutter: a short delay line whose delay moves by random (or sine) amounts.
#[derive(Clone)]
pub struct JitterUnit {
    buf: [Vec<f32>; 2],
    pos: usize,
    fs: f32,
    depth: f32,
    rate: f32,
    sine: bool,
    mix: f32,
    phase: f32,
    rng: u32,
    from: f32,
    to: f32,
}

impl JitterUnit {
    pub fn new(fs: f32) -> Self {
        let n = ((fs * 0.006) as usize + 64).next_power_of_two();
        Self { buf: [vec![0.0; n], vec![0.0; n]], pos: 0, fs, depth: 0.0, rate: 8.0, sine: false, mix: 1.0, phase: 0.0, rng: 0x1234_5678, from: 0.0, to: 0.0 }
    }

    pub fn set(&mut self, p: &[f32]) {
        // depth in microseconds (the delay swings by +- this much)
        self.depth = p.first().copied().unwrap_or(100.0).clamp(0.0, 2000.0) * 1e-6 * self.fs;
        self.rate = p.get(1).copied().unwrap_or(8.0).max(0.05);
        self.sine = p.get(2).copied().unwrap_or(0.0) >= 0.5;
        self.mix = p.get(3).copied().unwrap_or(100.0) * 0.01;
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Delay of the unit in frames when the modulation is at its centre.
    pub fn centre_delay(&self) -> f32 {
        self.depth + 2.0
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let mask = self.buf[0].len() - 1;
        for i in 0..l.len() {
            self.phase += self.rate / self.fs;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.from = self.to;
                self.to = self.rand();
            }
            let m = if self.sine { (2.0 * PI * self.phase).sin() } else { self.from + (self.to - self.from) * self.phase };
            let d = (self.depth + 2.0 + self.depth * m).clamp(1.0, mask as f32 - 2.0);
            let (di, fr) = (d.floor() as usize, d.fract());
            for (c, v) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                self.buf[c][self.pos & mask] = *v;
                let a = self.buf[c][self.pos.wrapping_sub(di) & mask];
                let b = self.buf[c][self.pos.wrapping_sub(di + 1) & mask];
                let wet = a + (b - a) * fr;
                *v = *v * (1.0 - self.mix) + wet * self.mix;
            }
            self.pos = self.pos.wrapping_add(1);
        }
    }
}

/// Tremolo: the level swings with a low frequency oscillator.
#[derive(Clone, Default)]
pub struct TremUnit {
    rate: f32,
    depth: f32,
    shape: usize,
    phase: f32,
    fs: f32,
}

impl TremUnit {
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.fs = fs;
        self.rate = p.first().copied().unwrap_or(5.0);
        self.depth = p.get(1).copied().unwrap_or(50.0) * 0.01;
        self.shape = p.get(2).copied().unwrap_or(0.0).round() as usize;
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            self.phase = (self.phase + self.rate / self.fs.max(1.0)).fract();
            let s = match self.shape {
                1 => 1.0 - 4.0 * (self.phase - 0.5).abs(),
                2 => {
                    // a square wave with short ramps (no clicks)
                    ((self.phase * 2.0 * PI).sin() * 8.0).clamp(-1.0, 1.0)
                }
                _ => (self.phase * 2.0 * PI).sin(),
            };
            let g = 1.0 - self.depth * 0.5 * (1.0 - s);
            l[i] *= g;
            r[i] *= g;
        }
    }
}
// ---------------------------------------------------------------------- sample rate conversion

/// Streaming resampler for one source: frames come in at the source rate, go out at the graph rate.
/// Cubic (Catmull-Rom) interpolation when the rates differ, plain copying when they match.
pub struct Rs {
    buf: VecDeque<[f32; 2]>,
    pos: f64,
    /// input frames per output frame
    ratio: f64,
}

impl Rs {
    pub fn new(from: u32, to: u32) -> Self {
        Self { buf: VecDeque::new(), pos: 0.0, ratio: from.max(1) as f64 / to.max(1) as f64 }
    }

    pub fn same_rate(&self) -> bool {
        (self.ratio - 1.0).abs() < 1e-9
    }

    pub fn push(&mut self, l: &[f32], r: &[f32]) {
        self.buf.extend(l.iter().copied().zip(r.iter().copied()).map(|(a, b)| [a, b]));
    }

    pub fn push_frame(&mut self, f: [f32; 2]) {
        self.buf.push_back(f);
    }

    /// Output frames that can be produced from what is queued.
    pub fn available(&self) -> usize {
        if self.same_rate() {
            return self.buf.len();
        }
        let have = self.buf.len() as f64 - self.pos - 3.0;
        if have <= 0.0 { 0 } else { (have / self.ratio) as usize }
    }

    pub fn queued(&self) -> usize {
        self.buf.len()
    }

    /// Drop the oldest frames so that no more than `keep` remain (drift control).
    pub fn trim(&mut self, keep: usize) {
        if self.buf.len() > keep {
            let d = self.buf.len() - keep;
            self.buf.drain(..d);
        }
    }

    /// Produce up to `l.len()` frames; returns how many were written (the caller pads the rest).
    pub fn pull(&mut self, l: &mut [f32], r: &mut [f32]) -> usize {
        let n = l.len().min(r.len());
        if self.same_rate() {
            let k = n.min(self.buf.len());
            for i in 0..k {
                let f = self.buf.pop_front().unwrap();
                l[i] = f[0];
                r[i] = f[1];
            }
            return k;
        }
        let k = n.min(self.available());
        for i in 0..k {
            let base = self.pos.floor() as usize;
            let t = (self.pos - base as f64) as f32;
            for c in 0..2 {
                // points p0..p3 around the position (the first output needs one frame of history: use p1 twice)
                let p1 = self.buf[base][c];
                let p0 = if base > 0 { self.buf[base - 1][c] } else { p1 };
                let (p2, p3) = (self.buf[base + 1][c], self.buf[base + 2][c]);
                let v = p1 + 0.5 * t * (p2 - p0 + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)));
                if c == 0 {
                    l[i] = v;
                } else {
                    r[i] = v;
                }
            }
            self.pos += self.ratio;
        }
        // forget the consumed frames but keep one of history
        let drop = (self.pos.floor() as usize).saturating_sub(1).min(self.buf.len());
        self.buf.drain(..drop);
        self.pos -= drop as f64;
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, fs: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / fs).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn eq_boosts_its_band_and_leaves_the_rest() {
        let fs = 48000.0;
        let mut eq = EqUnit::default();
        let mut p = vec![6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0, 6.0, 1000.0, 0.0, 1.0];
        p[0] = 0.0;
        p[2] = 12.0;
        eq.set(&p, fs);
        assert!((eq.response_db(fs, 1000.0) - 12.0).abs() < 0.2);
        assert!(eq.response_db(fs, 50.0).abs() < 0.5);
        let mut l = sine(1000.0, fs, 9600, 0.1);
        let mut r = l.clone();
        eq.process(&mut l, &mut r);
        let g = lin_to_db(rms(&l[4800..]) / rms(&sine(1000.0, fs, 4800, 0.1)));
        assert!((g - 12.0).abs() < 0.5, "{g}");
    }

    #[test]
    fn eq_notch_depth_follows_the_gain_and_more_bands_work() {
        let fs = 48000.0;
        let mut eq = EqUnit::default();
        // one notch at 1 kHz, -30 dB deep, plus a ninth band: 9 bands in all
        let mut p: Vec<f32> = Vec::new();
        for b in 0..9 {
            p.extend(if b == 0 { [5.0, 1000.0, -30.0, 4.0] } else if b == 8 { [0.0, 4000.0, 6.0, 1.0] } else { [6.0, 1000.0, 0.0, 1.0] });
        }
        eq.set(&p, fs);
        assert_eq!(eq.bands.len(), 9);
        assert!((eq.response_db(fs, 1000.0) + 30.0).abs() < 1.0, "{}", eq.response_db(fs, 1000.0));
        assert!(eq.response_db(fs, 100.0).abs() < 0.5);
        assert!((eq.response_db(fs, 4000.0) - 6.0).abs() < 0.6);
        // a shallower notch really is shallower
        p[2] = -10.0;
        eq.set(&p, fs);
        assert!((eq.response_db(fs, 1000.0) + 10.0).abs() < 1.0);
    }

    /// Level (linear amplitude) of one frequency in a signal, by correlation over whole periods.
    fn tone_level(x: &[f32], f: f32, fs: f32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / fs as f64;
            re += *v as f64 * w.cos();
            im += *v as f64 * w.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    #[test]
    fn harmonics_node_makes_a_series_of_even_or_odd_harmonics() {
        let fs = 48000.0;
        let run = |even: f32, odd: f32, rich: f32, amp: f32| {
            let mut u = HarmUnit::default();
            u.set(&[even, odd, rich, 0.0], fs);
            let mut l = sine(1000.0, fs, 48000, amp);
            let mut r = l.clone();
            u.process(&mut l, &mut r);
            l[12000..].to_vec()
        };
        let a = 0.8;
        // even: 2nd and 4th (and more), no odd ones
        let e = run(100.0, 0.0, 50.0, a);
        let (h2, h3, h4) = (tone_level(&e, 2000.0, fs), tone_level(&e, 3000.0, fs), tone_level(&e, 4000.0, fs));
        assert!(h2 > 0.2 * a, "2nd {h2}");
        assert!(h4 > 0.03 * a, "4th {h4}");
        assert!(h3 < 0.01 * a, "3rd {h3}");
        // odd: 3rd, 5th, 7th, no even ones
        let o = run(0.0, 100.0, 50.0, a);
        let (o2, o3, o5, o7) = (tone_level(&o, 2000.0, fs), tone_level(&o, 3000.0, fs), tone_level(&o, 5000.0, fs), tone_level(&o, 7000.0, fs));
        assert!(o3 > 0.2 * a, "3rd {o3}");
        assert!(o5 > 0.05 * a && o7 > 0.02 * a, "5th {o5} 7th {o7}");
        assert!(o2 < 0.01 * a, "2nd {o2}");
        // the fundamental stays what it was
        assert!((tone_level(&o, 1000.0, fs) - a).abs() < 0.03, "fundamental {}", tone_level(&o, 1000.0, fs));
        // richness reaches further up the series
        let poor = run(0.0, 100.0, 0.0, a);
        let rich = run(0.0, 100.0, 100.0, a);
        assert!(tone_level(&rich, 9000.0, fs) > tone_level(&poor, 9000.0, fs) * 2.0);
        // a quiet signal gets the same amount of harmonics relative to its level
        let loud = tone_level(&run(0.0, 100.0, 50.0, 0.8), 3000.0, fs) / 0.8;
        let quiet = tone_level(&run(0.0, 100.0, 50.0, 0.01), 3000.0, fs) / 0.01;
        assert!((loud / quiet - 1.0).abs() < 0.2, "loud {loud} quiet {quiet}");
        // nothing added: the input comes back
        let n = run(0.0, 0.0, 50.0, a);
        assert!((tone_level(&n, 1000.0, fs) - a).abs() < 0.01);
    }
    #[test]
    fn distortions_stay_bounded_and_add_harmonics() {
        let fs = 48000.0;
        for shape in 0..4 {
            let mut u = DistUnit::default();
            u.set(&[24.0, shape as f32, 20000.0, 100.0, 0.0], fs);
            let mut l = sine(500.0, fs, 24000, 0.9);
            let mut r = l.clone();
            u.process(&mut l, &mut r);
            let peak = l.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak <= 1.5 && peak > 0.2, "shape {shape} peak {peak}");
            let h3 = tone_level(&l[4000..], 1500.0, fs);
            let h2 = tone_level(&l[4000..], 1000.0, fs);
            if shape == 2 {
                assert!(h2 > 0.02, "asymmetric clipping makes even harmonics: {h2}");
            }
            if shape == 0 || shape == 1 {
                assert!(h3 > 0.02 && h2 < 0.01, "symmetric clipping makes odd harmonics: h2 {h2} h3 {h3}");
            }
        }
        let mut fz = FuzzUnit::default();
        fz.set(&[100.0, 0.0, 8000.0, -6.0], fs);
        let mut l = sine(200.0, fs, 24000, 0.5);
        let mut r = l.clone();
        fz.process(&mut l, &mut r);
        assert!(l.iter().all(|v| v.abs() <= 1.0) && l.iter().any(|v| v.abs() > 0.2));
    }

    #[test]
    fn bitcrusher_jitter_and_tremolo() {
        let fs = 48000.0;
        let mut c = CrushUnit::default();
        c.set(&[3.0, 1.0, 100.0]);
        let mut l = sine(300.0, fs, 4800, 0.9);
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        let mut levels: Vec<i32> = l.iter().map(|v| (v * 1000.0).round() as i32).collect();
        levels.sort();
        levels.dedup();
        assert!(levels.len() <= 8, "3 bits have at most 8 levels, got {}", levels.len());
        // held samples: with a hold of 8 the output changes at most every 8th frame
        let mut c2 = CrushUnit::default();
        c2.set(&[24.0, 8.0, 100.0]);
        let mut l = sine(300.0, fs, 4800, 0.9);
        let mut r = l.clone();
        c2.process(&mut l, &mut r);
        assert!(l.windows(2).filter(|w| w[0] != w[1]).count() <= 4800 / 8 + 1);
        // jitter with no depth is a plain short delay; with depth it still carries the signal
        let mut j = JitterUnit::new(fs);
        j.set(&[0.0, 8.0, 0.0, 100.0]);
        let mut l = sine(1000.0, fs, 9600, 0.5);
        let mut r = l.clone();
        j.process(&mut l, &mut r);
        assert!((tone_level(&l[2000..], 1000.0, fs) - 0.5).abs() < 0.02);
        j.set(&[800.0, 20.0, 0.0, 100.0]);
        let mut l = sine(1000.0, fs, 9600, 0.5);
        let mut r = l.clone();
        j.process(&mut l, &mut r);
        let clean = tone_level(&l[2000..], 1000.0, fs);
        assert!(clean < 0.5 && clean > 0.1, "jitter smears the tone: {clean}");
        // tremolo at full depth goes to silence at the bottom of its swing
        let mut t = TremUnit::default();
        t.set(&[10.0, 100.0, 0.0], fs);
        let mut l = vec![1.0f32; 4800];
        let mut r = l.clone();
        t.process(&mut l, &mut r);
        let (lo, hi) = l.iter().fold((9f32, -9f32), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(lo < 0.05 && hi > 0.95, "{lo} {hi}");
    }
    #[test]
    fn filters_cut_the_right_side() {
        let fs = 48000.0;
        for (low, f_test, expect_cut) in [(0.0, 100.0, true), (0.0, 4000.0, false), (1.0, 100.0, false), (1.0, 12000.0, true)] {
            let mut fu = FilterUnit::default();
            fu.set(&[low, 1000.0, 1.0], fs);
            let mut l = sine(f_test, fs, 19200, 0.5);
            let mut r = l.clone();
            fu.process(&mut l, &mut r);
            let g = lin_to_db(rms(&l[9600..]) / rms(&sine(f_test, fs, 9600, 0.5)));
            if expect_cut {
                assert!(g < -25.0, "low={low} f={f_test} gain {g}");
            } else {
                assert!(g > -1.0, "low={low} f={f_test} gain {g}");
            }
        }
    }

    #[test]
    fn compressor_reduces_loud_signals_by_the_ratio() {
        let fs = 48000.0;
        let mut c = CompUnit::default();
        c.set(&[-20.0, 4.0, 1.0, 50.0, 0.0, 0.0], fs);
        // static curve: 8 dB over the threshold -> 6 dB less
        assert!((c.curve(-12.0) + 6.0).abs() < 1e-3);
        assert!(c.curve(-30.0).abs() < 1e-6);
        let mut l = sine(1000.0, fs, 48000, 0.5);
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        let out_peak = l[24000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        // 0.5 = -6 dB is 14 dB over: expect about -20 + 14/4 = -16.5 dB
        assert!((lin_to_db(out_peak) + 16.5).abs() < 1.0, "{}", lin_to_db(out_peak));
        assert!(c.gr_db < -5.0);
    }

    #[test]
    fn limiter_never_exceeds_the_ceiling() {
        let fs = 48000.0;
        let mut u = LimiterUnit::default();
        u.set(&[-6.0, 50.0], fs);
        let mut l: Vec<f32> = sine(300.0, fs, 24000, 2.0);
        let mut r = l.clone();
        u.process(&mut l, &mut r);
        let ceil = db_to_lin(-6.0);
        assert!(l.iter().chain(r.iter()).all(|v| v.abs() <= ceil + 1e-6));
        assert!(u.gr_db < -3.0);
        // a quiet signal passes unchanged (after the latency)
        let mut q = sine(300.0, fs, 4800, 0.1);
        let mut qr = q.clone();
        let orig = q.clone();
        let mut u2 = LimiterUnit::default();
        u2.set(&[-6.0, 50.0], fs);
        u2.process(&mut q, &mut qr);
        let la = u2.latency();
        for i in 2000..4000 {
            assert!((q[i] - orig[i - la]).abs() < 1e-3);
        }
    }

    #[test]
    fn gate_closes_on_silence_and_opens_on_signal() {
        let fs = 48000.0;
        let mut g = GateUnit::default();
        g.set(&[-40.0, 1.0, 50.0, -80.0], fs);
        let mut l = vec![0.001f32; 9600];
        let mut r = l.clone();
        g.process(&mut l, &mut r);
        assert!(!g.open && l[9599].abs() < 1e-4);
        let mut l = sine(500.0, fs, 9600, 0.3);
        let mut r = l.clone();
        g.process(&mut l, &mut r);
        assert!(g.open);
    }

    #[test]
    fn delay_shifts_by_the_time() {
        let fs = 48000.0;
        let mut d = DelayUnit::new(1.0, fs);
        d.set(&[10.0, 0.0, 100.0], fs);
        let mut l = vec![0.0f32; 2000];
        l[0] = 1.0;
        let mut r = l.clone();
        d.process(&mut l, &mut r);
        assert_eq!(l.iter().position(|v| v.abs() > 0.5), Some(480));
    }

    #[test]
    fn width_and_utility() {
        let (mut l, mut r) = (vec![1.0f32; 4], vec![-1.0f32; 4]);
        stereo_width(&mut l, &mut r, 0.0, 0.0);
        assert!(l[0].abs() < 1e-6 && r[0].abs() < 1e-6, "width 0 is mono");
        let (mut l, mut r) = (vec![0.5f32; 4], vec![0.25f32; 4]);
        utility(&mut l, &mut r, 4);
        assert_eq!((l[0], r[0]), (0.25, 0.5));
        utility(&mut l, &mut r, 1);
        assert_eq!(r[0], 0.25);
    }

    #[test]
    fn resampler_keeps_a_sine_and_the_length() {
        let (from, to) = (44100u32, 48000u32);
        let x = sine(1000.0, from as f32, 44100, 0.5);
        let mut rs = Rs::new(from, to);
        rs.push(&x, &x);
        let (mut l, mut r) = (vec![0f32; 47000], vec![0f32; 47000]);
        let n = rs.pull(&mut l, &mut r);
        assert!(n > 46900 && n <= 48000, "{n}");
        let want = sine(1000.0, to as f32, 48000, 0.5);
        let err = (1000..n - 10).map(|i| (l[i] - want[i]).abs()).fold(0f32, f32::max);
        assert!(err < 0.01, "max error {err}");
        // same rate: plain copy, nothing lost
        let mut same = Rs::new(48000, 48000);
        same.push(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
        let (mut a, mut b) = ([0f32; 5], [0f32; 5]);
        assert_eq!(same.pull(&mut a, &mut b), 3);
        assert_eq!(&a[..3], &[1.0, 2.0, 3.0]);
    }
}
