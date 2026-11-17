//! State shared between the audio callback, the DSP thread and the UI.

use crate::dsp::Window;
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
use std::time::Instant;

pub const RING_CAP: usize = 1 << 20;
pub const RING_MASK: usize = RING_CAP - 1;
/// Frequency resolution of the spectrogram / waterfall / 3D history.
pub const ROWS: usize = 4096;
/// Lowest frequency of the logarithmic axis.
pub const FMIN_LOG: f32 = 10.0;
pub const MAXH: usize = 10;
/// Most input channels that get a level meter.
pub const MAX_CH: usize = 32;
/// History dB range encoded into u8.
pub const HIST_DB_RANGE: f32 = 150.0;

pub struct Ring {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    pub total: u64,
}

impl Ring {
    pub fn new() -> Self {
        Self { l: vec![0.0; RING_CAP], r: vec![0.0; RING_CAP], total: 0 }
    }

    #[inline]
    pub fn push(&mut self, l: f32, r: f32) {
        let i = (self.total as usize) & RING_MASK;
        self.l[i] = l;
        self.r[i] = r;
        self.total += 1;
    }

    /// Copy the most recent `l.len()` samples (zero padded at the front if not available yet).
    pub fn copy_last(&self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        let avail = (self.total as usize).min(n).min(RING_CAP);
        let pad = n - avail;
        l[..pad].fill(0.0);
        r[..pad].fill(0.0);
        let start = self.total as usize - avail;
        self.copy_abs(start, &mut l[pad..], &mut r[pad..]);
    }

    /// Like `copy_last`, but each channel looks `sl` / `sr` samples further back (zero padded before the start). Only the
    /// reference for the tests now: the program uses `copy_last_shifted_chunked`.
    #[cfg(test)]
    pub fn copy_last_shifted(&self, l: &mut [f32], r: &mut [f32], sl: usize, sr: usize) {
        let n = l.len() as isize;
        let tot = self.total as isize;
        for (out, back, src) in [(l, sl, &self.l), (r, sr, &self.r)] {
            let start = tot - back as isize - n;
            for (i, o) in out.iter_mut().enumerate() {
                let idx = start + i as isize;
                *o = if idx < 0 || tot - idx > RING_CAP as isize { 0.0 } else { src[(idx as usize) & RING_MASK] };
            }
        }
    }

    /// Copy `out.len()` samples of one channel starting at absolute sample index `start`.
    fn copy_abs_one(&self, right: bool, start: usize, out: &mut [f32]) {
        let src = if right { &self.r } else { &self.l };
        let len = out.len();
        let s = start & RING_MASK;
        let first = (RING_CAP - s).min(len);
        out[..first].copy_from_slice(&src[s..s + first]);
        if first < len {
            out[first..].copy_from_slice(&src[..len - first]);
        }
    }

    /// Copy `l.len()` samples starting at absolute sample index `start`.
    pub fn copy_abs(&self, start: usize, l: &mut [f32], r: &mut [f32]) {
        let len = l.len();
        let s = start & RING_MASK;
        let first = (RING_CAP - s).min(len);
        l[..first].copy_from_slice(&self.l[s..s + first]);
        r[..first].copy_from_slice(&self.r[s..s + first]);
        if first < len {
            let rest = len - first;
            l[first..].copy_from_slice(&self.l[..rest]);
            r[first..].copy_from_slice(&self.r[..rest]);
        }
    }
}

/// Like `Ring::copy_last`, but the lock is taken for short pieces only. The audio callback waits for this lock, and a copy of
/// a quarter million samples in one go would hold it up long enough for the sound card to run dry if this thread is
/// descheduled in the middle. The window is fixed by the position at the start, so it is still one consistent piece of signal.
pub fn copy_last_chunked(ring: &Mutex<Ring>, l: &mut [f32], r: &mut [f32]) {
    const PIECE: usize = 8192;
    let n = l.len();
    let total = ring.lock().total as usize;
    // stay well inside the ring: the oldest samples must not be overwritten while the piece is being read
    let avail = total.min(n).min(RING_CAP / 2);
    let pad = n - avail;
    l[..pad].fill(0.0);
    r[..pad].fill(0.0);
    let start = total - avail;
    let mut done = 0;
    while done < avail {
        let m = PIECE.min(avail - done);
        ring.lock().copy_abs(start + done, &mut l[pad + done..pad + done + m], &mut r[pad + done..pad + done + m]);
        done += m;
    }
}

/// Like `Ring::copy_last_shifted`, but the lock is taken for short pieces only (see `copy_last_chunked`) and the per-sample range
/// test is hoisted: each channel is one run of zeros followed by contiguous copies. Samples older than half the ring (as seen at
/// the start) read as zero, as in `copy_last_chunked`; the transfer function never looks that far back (window + delay < 1.5 n).
pub fn copy_last_shifted_chunked(ring: &Mutex<Ring>, l: &mut [f32], r: &mut [f32], sl: usize, sr: usize) {
    const PIECE: usize = 8192;
    let n = l.len();
    let total = ring.lock().total as usize;
    let lo = total.saturating_sub(RING_CAP / 2);
    for (out, back, right) in [(l, sl, false), (r, sr, true)] {
        // absolute index of out[0] (negative: before the first sample ever)
        let start = total as i64 - back as i64 - n as i64;
        // out[..skip] has no data: before the first sample, or too old to be read safely
        let skip = ((lo as i64 - start).clamp(0, n as i64)) as usize;
        out[..skip].fill(0.0);
        let mut done = skip;
        while done < n {
            let m = PIECE.min(n - done);
            ring.lock().copy_abs_one(right, (start + done as i64) as usize, &mut out[done..done + m]);
            done += m;
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Params {
    pub fft_size: usize,
    pub window: Window,
    pub update_hz: f32,
    pub avg_count: f32,
    pub cols_per_sec: f32,
    /// 0 = L, 1 = R, 2 = max(L,R)
    pub spectro_src: u8,
    pub log_freq: bool,
    pub bw_lo: f32,
    pub bw_hi: f32,
    pub harmonics: usize,
    pub manual_fund: bool,
    pub fund_hz: f32,
    pub paused: bool,
    pub reset_gen: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            fft_size: 16384,
            window: Window::BlackmanHarris,
            update_hz: 120.0,
            avg_count: 16.0,
            cols_per_sec: 60.0,
            spectro_src: 2,
            log_freq: true,
            bw_lo: 20.0,
            bw_hi: 20000.0,
            harmonics: 9,
            manual_fund: false,
            fund_hz: 1000.0,
            paused: false,
            reset_gen: 0,
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Metrics {
    pub valid: bool,
    pub fund_hz: f32,
    pub fund_dbfs: f32,
    pub thd_db: f32,
    pub thdn_db: f32,
    pub snr_db: f32,
    pub sinad_db: f32,
    pub enob: f32,
    pub enob_fs: f32,
    pub sfdr_db: f32,
    pub noise_floor_dbfs: f32,
    pub harm_db: [f32; MAXH],
    pub harm_hz: [f32; MAXH],
}

#[derive(Clone, Default)]
pub struct Analysis {
    pub seq: u64,
    pub epoch: u32,
    pub sr: f32,
    pub n: usize,
    pub live: [Vec<f32>; 2],
    pub avg: [Vec<f32>; 2],
    pub max: [Vec<f32>; 2],
    pub min: [Vec<f32>; 2],
    /// L/R correlation per bin of the *measurement* FFT (window independent), `meas_n / 2 + 1` bins
    pub corr: Vec<f32>,
    pub scallop_db: f32,
    /// main-lobe table of the display window, see `dsp::peak_table`
    pub peak_tab: Vec<[f32; 2]>,
    pub metrics: [Metrics; 2],
    pub rms_db: [f32; 2],
    pub peak_db: [f32; 2],
    pub dc: [f32; 2],
    pub meas_n: usize,
    pub enbw_bins: f32,
    pub coherent_gain: f32,
    pub dsp_ms: f32,
    pub dsp_fps: f32,
}

/// Settings of the live transfer function, owned by the Transfer tool and read by the DSP thread.
#[derive(Clone, PartialEq)]
pub struct TfParams {
    pub on: bool,
    /// R carries the reference (default), otherwise L does
    pub ref_r: bool,
    pub auto_delay: bool,
    pub delay_ms: f32,
    pub reset: u32,
}

impl Default for TfParams {
    fn default() -> Self {
        Self { on: false, ref_r: true, auto_delay: true, delay_ms: 0.0, reset: 0 }
    }
}

/// Averaged transfer function H = measured / reference, `n / 2 + 1` bins.
#[derive(Clone, Default)]
pub struct TfResult {
    pub seq: u64,
    pub sr: f32,
    pub n: usize,
    pub mag: Vec<f32>,
    pub phase: Vec<f32>,
    pub coh: Vec<f32>,
    pub delay_ms: f32,
    pub frames: u32,
}

/// Microphone correction handed to the DSP thread (empty `f` = off).
#[derive(Default)]
pub struct MicShared {
    pub rev: u32,
    pub f: Vec<f32>,
    pub db: Vec<f32>,
    pub mask: [bool; 2],
}

pub struct Shared {
    /// live input monitor: the audio callback feeds the queue while this is set
    pub mon_on: AtomicBool,
    pub mon_q: std::sync::Arc<Mutex<crate::tools::monitor::Queue>>,
    pub mic: Mutex<MicShared>,
    pub tf_params: Mutex<TfParams>,
    pub tf_out: Mutex<TfResult>,
    pub ring: Mutex<Ring>,
    pub params: Mutex<Params>,
    pub analysis: Mutex<Analysis>,
    /// `Analysis::seq` the UI took last: the DSP thread need not copy a new spectrum while the UI still has not looked at the old one
    pub an_pulled: AtomicU64,
    /// Spectrogram columns (epoch, ROWS bytes low->high frequency) waiting for the UI.
    pub columns: Mutex<Vec<(u32, Vec<u8>)>>,
    pub status: Mutex<String>,
    pub sr: AtomicU32,
    /// device channels (0-based) analysed as L and R
    pub ch_a: AtomicUsize,
    pub ch_b: AtomicUsize,
    /// per device channel: largest |sample| since the UI last looked (f32 bits, non-negative so bits order like values)
    pub ch_peak: [AtomicU32; MAX_CH],
    pub quit: AtomicBool,
    /// who feeds the ring while a file / the timeline plays (0 = nobody, 1 = file player, 2 = timeline)
    pub ring_owner: AtomicU32,
    pub xruns: AtomicU32,
    /// incremented every time an audio thread is started; streams of older generations go quiet
    pub audio_gen: AtomicU32,
    /// channel count of the open input device
    pub chans: AtomicU32,
    /// diagnostics for the hang watchdog (milliseconds since 	0)
    pub t0: Instant,
    pub ui_beat: AtomicU64,
    pub ui_phase: AtomicU32,
    pub dsp_beat: AtomicU64,
    /// time of the last real audio callback; used to feed silence when a device delivers nothing
    pub last_data_ms: AtomicU64,
    pub feeding_silence: AtomicBool,
    pub cv_lock: Mutex<()>,
    pub cv: Condvar,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            mon_on: AtomicBool::new(false),
            mon_q: std::sync::Arc::new(Mutex::new(Default::default())),
            mic: Mutex::new(MicShared::default()),
            tf_params: Mutex::new(TfParams::default()),
            tf_out: Mutex::new(TfResult::default()),
            ring: Mutex::new(Ring::new()),
            params: Mutex::new(Params::default()),
            analysis: Mutex::new(Analysis::default()),
            an_pulled: AtomicU64::new(0),
            columns: Mutex::new(Vec::new()),
            status: Mutex::new(String::from("No input")),
            sr: AtomicU32::new(0),
            ch_a: AtomicUsize::new(0),
            ch_b: AtomicUsize::new(1),
            ch_peak: std::array::from_fn(|_| AtomicU32::new(0)),
            quit: AtomicBool::new(false),
            ring_owner: AtomicU32::new(0),
            xruns: AtomicU32::new(0),
            audio_gen: AtomicU32::new(0),
            chans: AtomicU32::new(2),
            t0: Instant::now(),
            ui_beat: AtomicU64::new(0),
            ui_phase: AtomicU32::new(0),
            dsp_beat: AtomicU64::new(0),
            last_data_ms: AtomicU64::new(0),
            feeding_silence: AtomicBool::new(false),
            cv_lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }
}

/// Normalised axis position (0..1) -> frequency.
#[inline]
pub fn u_to_freq(u: f32, log: bool, sr: f32) -> f32 {
    let nyq = sr * 0.5;
    if log {
        FMIN_LOG * (nyq / FMIN_LOG).powf(u)
    } else {
        u * nyq
    }
}

#[inline]
pub fn freq_to_u(f: f32, log: bool, sr: f32) -> f32 {
    let nyq = sr * 0.5;
    if log {
        (f.max(1e-3) / FMIN_LOG).ln() / (nyq / FMIN_LOG).ln()
    } else {
        f / nyq
    }
}


/// "Listen to the band between cursor A and B": everything that is played back (the side bar monitor and the output
/// nodes of the routing graph) is band-passed while this is on. Not an analysis setting: it never changes what is measured.
pub struct BandListen {
    pub on: std::sync::atomic::AtomicBool,
    pub lo: AtomicU32,
    pub hi: AtomicU32,
}

pub static BAND: BandListen = BandListen { on: std::sync::atomic::AtomicBool::new(false), lo: AtomicU32::new(0), hi: AtomicU32::new(0) };

impl BandListen {
    pub fn set(&self, on: bool, lo: f32, hi: f32) {
        use std::sync::atomic::Ordering::Relaxed;
        self.lo.store(lo.to_bits(), Relaxed);
        self.hi.store(hi.to_bits(), Relaxed);
        self.on.store(on, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u64) -> u64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *s >> 33
    }

    fn filled(total: usize) -> Mutex<Ring> {
        let mut ring = Ring::new();
        let mut seed = 5u64;
        for _ in 0..total {
            let (a, b) = (lcg(&mut seed) as f32 / 1e9 - 1.0, lcg(&mut seed) as f32 / 1e9 - 1.0);
            ring.push(a, b);
        }
        Mutex::new(ring)
    }

    /// The chunked, branch-free copy gives exactly what the per-sample `copy_last_shifted` gives (also before the first sample
    /// and after the ring has wrapped).
    #[test]
    fn chunked_shifted_copy_matches_the_per_sample_one() {
        let mut seed = 9u64;
        for total in [1000usize, 20_000, 300_000, RING_CAP / 2 + 1234, RING_CAP + 777, 3 * RING_CAP / 2] {
            let ring = filled(total);
            for n in [64usize, 8192, 16384, 40_000] {
                for _ in 0..6 {
                    let (sl, sr) = (lcg(&mut seed) as usize % 60_000, lcg(&mut seed) as usize % 60_000);
                    let (mut a, mut b) = (vec![1.0f32; n], vec![1.0f32; n]);
                    ring.lock().copy_last_shifted(&mut a, &mut b, sl, sr);
                    let (mut c, mut d) = (vec![2.0f32; n], vec![2.0f32; n]);
                    copy_last_shifted_chunked(&ring, &mut c, &mut d, sl, sr);
                    assert!(a == c && b == d, "total {total} n {n} shifts {sl} {sr}");
                }
            }
        }
    }

    /// What lies further back than half the ring reads as zero (it could be overwritten while the pieces are copied).
    #[test]
    fn chunked_shifted_copy_ignores_the_oldest_half_of_the_ring() {
        let ring = filled(RING_CAP + 5);
        let n = 4096;
        let (mut l, mut r) = (vec![1.0f32; n], vec![1.0f32; n]);
        copy_last_shifted_chunked(&ring, &mut l, &mut r, RING_CAP / 2 + 10, 0);
        assert!(l.iter().all(|v| *v == 0.0));
        assert!(r.iter().any(|v| *v != 0.0));
    }

    /// Hold time and total time of the old and the new copy (cargo test --locked shifted_copy_cost -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn shifted_copy_cost() {
        let ring = filled(RING_CAP);
        let n = 262_144;
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let t = std::time::Instant::now();
        for _ in 0..50 {
            ring.lock().copy_last_shifted(&mut l, &mut r, 1000, 0);
        }
        println!("per-sample, one lock: {:?} per copy (the lock is held that long)", t.elapsed() / 50);
        let t = std::time::Instant::now();
        for _ in 0..50 {
            copy_last_shifted_chunked(&ring, &mut l, &mut r, 1000, 0);
        }
        println!("chunked: {:?} per copy in total, the lock is held for pieces of 8192 samples", t.elapsed() / 50);
    }
}
