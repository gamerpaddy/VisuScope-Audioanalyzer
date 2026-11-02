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
#[derive(Clone)]
pub struct PhaseUnit {
    sign: f32,
    /// 0 polarity only, 1 constant shift, 2 all-pass
    mode: usize,
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
        Self { sign: 1.0, mode: 0, cos: 1.0, sin: 0.0, ap: Biquad::default(), hilbert, hist: [vec![0.0; HILBERT_TAPS * 2], vec![0.0; HILBERT_TAPS * 2]], pos: 0 }
    }
}

impl PhaseUnit {
    /// `p`: polarity (0 normal, 1 inverted), mode, shift in degrees, frequency, Q.
    pub fn set(&mut self, p: &[f32], fs: f32) {
        self.sign = if p.first().copied().unwrap_or(0.0) >= 0.5 { -1.0 } else { 1.0 };
        self.mode = p.get(1).copied().unwrap_or(0.0).round().clamp(0.0, 2.0) as usize;
        let phi = p.get(2).copied().unwrap_or(0.0).to_radians();
        self.cos = phi.cos();
        self.sin = phi.sin();
        self.ap.set(BqType::AllPass, fs, p.get(3).copied().unwrap_or(1000.0), 0.0, p.get(4).copied().unwrap_or(0.7));
    }

    /// Frames of delay the node adds in its current mode.
    pub fn latency(&self) -> usize {
        if self.mode == 1 { HILBERT_DELAY } else { 0 }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        match self.mode {
            2 => self.ap.process(l, r),
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
        let block = Self::block_for(secs);
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
