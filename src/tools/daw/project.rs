//! Multitrack project model: sources (audio at the project rate), clips, tracks, mixing and editing.
//! Everything here is plain data + functions so it can be tested without a UI.

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

/// Frames per overview block (min / max pairs for drawing waveforms).
pub const OV_BLOCK: usize = 256;
/// Length of the fades where a clip lies over another one (seconds).
pub const XFADE_SECS: f32 = 0.012;
pub const MAX_TRACKS: usize = 64;

/// Audio that clips refer to. Immutable once created, shared between the UI, the play thread and undo steps.
pub struct Source {
    pub id: u32,
    pub name: String,
    /// 1 or 2 channels at the project sample rate
    pub ch: Vec<Vec<f32>>,
    /// min / max per `OV_BLOCK` frames, per channel
    pub ov: Vec<Vec<(f32, f32)>>,
    /// file this source was written to (project media folder or a recorded take)
    pub saved: Mutex<Option<PathBuf>>,
    /// the beat, found on a thread when first asked for
    grid: Arc<Mutex<Option<Arc<BeatGrid>>>>,
    grid_started: std::sync::atomic::AtomicBool,
}

/// The beat of a source: where the beats fall (source frames) and the tempo.
pub struct BeatGrid {
    pub beats: Vec<usize>,
    pub bpm: f32,
}

/// Hop of the onset analysis (frames).
const BEAT_HOP: usize = 512;

/// Find the beat of audio: an onset strength from the spectral flux (the rises of the spectrum, below about 6 kHz), the tempo from its
/// autocorrelation (60 to 200 bpm, 120 preferred where it is a toss-up), then a path through the onsets that keeps the tempo (dynamic
/// programming). `None` when nothing in it repeats (noise, a held tone, speech).
pub fn find_beat_grid(ch: &[Vec<f32>], sr: u32) -> Option<BeatGrid> {
    use rustfft::{num_complex::Complex32, FftPlanner};
    const WIN: usize = 1024;
    let len = ch.first().map_or(0, |c| c.len());
    let rate = sr as f32 / BEAT_HOP as f32;
    let n = len.saturating_sub(WIN) / BEAT_HOP;
    if n < (rate * 4.0) as usize {
        return None;
    }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(WIN);
    let win: Vec<f32> = (0..WIN).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WIN as f32).cos()).collect();
    let top = (6000.0 * WIN as f32 / sr as f32) as usize;
    let mut buf = vec![Complex32::new(0.0, 0.0); WIN];
    let mut prev = vec![0f32; top];
    let mut o = vec![0f32; n];
    let mut env = vec![0f32; n];
    for t in 0..n {
        let mut e = 0f32;
        for i in 0..WIN {
            let k = t * BEAT_HOP + i;
            let v = if ch.len() > 1 { 0.5 * (ch[0][k] + ch[1][k]) } else { ch[0][k] };
            e += v * v;
            buf[i] = Complex32::new(v * win[i], 0.0);
        }
        env[t] = (e / WIN as f32).sqrt();
        fft.process(&mut buf);
        let mut flux = 0f32;
        for k in 1..top {
            let m = (1.0 + 400.0 * buf[k].norm() / WIN as f32 * 4.0).ln();
            flux += (m - prev[k]).max(0.0);
            prev[k] = m;
        }
        o[t] = flux;
    }
    let peak = env.iter().copied().fold(0.0, f32::max);
    if peak < 1e-3 {
        return None;
    }
    // what the spectrum rose by, minus the slow trend (the loudness of the passage)
    let slow: Vec<f32> = {
        let w = (rate * 0.5) as usize;
        let mut acc = 0f32;
        (0..n)
            .map(|i| {
                acc += o[i];
                if i >= 2 * w + 1 {
                    acc -= o[i - 2 * w - 1];
                }
                acc / (i + 1).min(2 * w + 1) as f32
            })
            .collect()
    };
    for i in 0..n {
        o[i] = (o[i] - slow[i]).max(0.0);
    }
    let mean = o.iter().sum::<f32>() / n as f32;
    let sd = (o.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n as f32).sqrt();
    if sd < 1e-4 {
        return None;
    }
    for v in o.iter_mut() {
        *v = (*v - mean) / sd;
    }
    let o_pos: Vec<f32> = o.iter().map(|v| v.max(0.0)).collect();
    // tempo: the lag with the strongest repetition, with a mild preference for the usual tempi
    let (lag_lo, lag_hi) = ((rate * 60.0 / 200.0).floor() as usize, (rate * 60.0 / 60.0).ceil() as usize);
    let acn = |lag: usize| -> f32 { (0..n - lag).map(|i| o_pos[i] * o_pos[i + lag]).sum::<f32>() / (n - lag) as f32 };
    let ac0 = acn(0).max(1e-9);
    let mut ac = vec![0f32; lag_hi + 2];
    for lag in lag_lo.saturating_sub(1)..=lag_hi + 1 {
        ac[lag] = acn(lag);
    }
    let weight = |lag: f32| -> f32 {
        let bpm = 60.0 * rate / lag;
        (-0.5 * (bpm / 120.0).log2().powi(2) / 0.8f32.powi(2)).exp()
    };
    let mut best = (0.0f32, 0usize);
    for lag in lag_lo.max(1)..=lag_hi {
        // a beat also repeats one bar (two and four beats) later: count that in
        let s = (ac[lag] + 0.5 * ac.get(lag * 2).copied().unwrap_or(0.0)) * weight(lag as f32);
        if s > best.0 {
            best = (s, lag);
        }
    }
    let lag = best.1;
    // the repetition has to stand out from the lags around it (noise and held tones repeat evenly everywhere)
    let span = &ac[lag_lo.max(1)..=lag_hi];
    let avg = span.iter().sum::<f32>() / span.len() as f32;
    if lag == 0 || ac[lag] / ac0 < 0.04 || ac[lag] < 1.8 * avg {
        return None;
    }
    // a fractional period from the shape of the peak
    let period = {
        let (a, b, c2) = (ac[lag - 1], ac[lag], ac[lag + 1]);
        let d = a - 2.0 * b + c2;
        lag as f32 + if d.abs() > 1e-9 { (0.5 * (a - c2) / d).clamp(-0.5, 0.5) } else { 0.0 }
    };
    // beat path
    const TIGHT: f32 = 40.0;
    let mut score = o.clone();
    let mut from = vec![usize::MAX; n];
    let (lo_off, hi_off) = ((period * 0.5) as usize, (period * 2.0) as usize);
    for t in 0..n {
        let mut best_prev = (f32::MIN, usize::MAX);
        let (a, b) = (t.saturating_sub(hi_off), t.saturating_sub(lo_off.max(1)));
        if t >= lo_off.max(1) {
            for pv in a..=b {
                let d = ((t - pv) as f32 / period).ln();
                let s = score[pv] - TIGHT * d * d;
                if s > best_prev.0 {
                    best_prev = (s, pv);
                }
            }
        }
        if best_prev.1 != usize::MAX && best_prev.0 > 0.0 {
            score[t] = o[t] + best_prev.0;
            from[t] = best_prev.1;
        }
    }
    let tail = n.saturating_sub(period as usize * 2);
    let mut t = (tail..n).max_by(|a, b| score[*a].total_cmp(&score[*b]))?;
    let mut idx = vec![t];
    while from[t] != usize::MAX {
        t = from[t];
        idx.push(t);
    }
    idx.reverse();
    // not where it is quiet (the path runs on through silence)
    let gate = peak * 0.02;
    let near = (period * 0.5) as usize;
    let beats: Vec<usize> = idx
        .into_iter()
        .filter(|&i| env[i.saturating_sub(near)..(i + near + 1).min(n)].iter().copied().fold(0.0, f32::max) > gate)
        .map(|i| i * BEAT_HOP + BEAT_HOP / 2)
        .collect();
    if beats.len() < 4 {
        return None;
    }
    Some(BeatGrid { beats, bpm: 60.0 * rate / period })
}

impl Source {
    pub fn new(id: u32, name: String, mut ch: Vec<Vec<f32>>) -> Self {
        // a source always has a channel (an empty one reads as silence)
        if ch.is_empty() {
            ch.push(Vec::new());
        }
        let ov = ch.iter().map(|c| c.chunks(OV_BLOCK).map(|b| b.iter().fold((0f32, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))).collect()).collect();
        Self { id, name, ch, ov, saved: Mutex::new(None), grid: Arc::new(Mutex::new(None)), grid_started: std::sync::atomic::AtomicBool::new(false) }
    }

    /// The beat of this source, once it is found (the search runs on a thread of its own the first time it is asked for).
    pub fn grid(self: &Arc<Self>, sr: u32) -> Option<Arc<BeatGrid>> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.grid_started.compare_exchange(false, true, Relaxed, Relaxed).is_ok() {
            let (src, slot) = (self.clone(), self.grid.clone());
            let _ = std::thread::Builder::new().name("daw-beats".into()).spawn(move || {
                let g = find_beat_grid(&src.ch, sr);
                crate::log::log(&format!("beat of {}: {}", src.name, g.as_ref().map_or("none found".to_string(), |g| format!("{:.1} bpm, {} beats", g.bpm, g.beats.len()))));
                *slot.lock() = Some(Arc::new(g.unwrap_or(BeatGrid { beats: Vec::new(), bpm: 0.0 })));
            });
        }
        self.grid.lock().clone().filter(|g| !g.beats.is_empty())
    }

    /// False until the search for the beat has finished (for a repaint later).
    pub fn grid_pending(&self) -> bool {
        self.grid_started.load(std::sync::atomic::Ordering::Relaxed) && self.grid.lock().is_none()
    }

    pub fn len(&self) -> usize {
        self.ch.first().map_or(0, |c| c.len())
    }

    pub fn stereo(&self) -> bool {
        self.ch.len() > 1
    }

    #[inline]
    pub fn at(&self, c: usize, i: usize) -> f32 {
        self.ch[c.min(self.ch.len() - 1)].get(i).copied().unwrap_or(0.0)
    }
}

#[derive(Clone)]
pub struct Clip {
    pub id: u32,
    pub src: Arc<Source>,
    /// position on the timeline (frames)
    pub start: i64,
    /// first source frame used and number of frames
    pub off: usize,
    pub len: usize,
    pub gain_db: f32,
    pub fade_in: usize,
    pub fade_out: usize,
    pub reverse: bool,
    pub muted: bool,
    pub name: String,
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub fn db_to_lin(db: f32) -> f32 {
    if db <= -90.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

impl Clip {
    pub fn end(&self) -> i64 {
        self.start + self.len as i64
    }

    /// Linear gain at clip-relative frame `t` (clip gain and fades).
    #[inline]
    pub fn env(&self, t: usize, g: f32) -> f32 {
        let mut e = g;
        if t < self.fade_in {
            e *= smooth((t as f32 + 0.5) / self.fade_in as f32);
        }
        let from_end = self.len - 1 - t.min(self.len - 1);
        if from_end < self.fade_out {
            e *= smooth((from_end as f32 + 0.5) / self.fade_out as f32);
        }
        e
    }

    /// Source frame for clip-relative frame `t`.
    #[inline]
    pub fn src_index(&self, t: usize) -> usize {
        if self.reverse { self.off + self.len - 1 - t } else { self.off + t }
    }

    pub fn clamp_fades(&mut self) {
        self.fade_in = self.fade_in.min(self.len);
        self.fade_out = self.fade_out.min(self.len - self.fade_in.min(self.len));
    }

    /// Move the left edge to timeline frame `t` (trim when `t` is inside, extend as far as the source allows).
    pub fn set_start(&mut self, t: i64) {
        let d = t - self.start;
        if d >= 0 {
            let d = (d as usize).min(self.len - 1);
            if self.reverse {
                self.len -= d;
            } else {
                self.off += d;
                self.len -= d;
            }
            self.start += d as i64;
        } else {
            let want = (-d) as usize;
            let room = if self.reverse { self.src.len() - (self.off + self.len) } else { self.off };
            let e = want.min(room).min(self.start.max(0) as usize);
            if self.reverse {
                self.len += e;
            } else {
                self.off -= e;
                self.len += e;
            }
            self.start -= e as i64;
        }
        self.clamp_fades();
    }

    /// Move the right edge to timeline frame `t`.
    pub fn set_end(&mut self, t: i64) {
        let d = t - self.end();
        if d <= 0 {
            let cut = ((-d) as usize).min(self.len - 1);
            if self.reverse {
                self.off += cut;
            }
            self.len -= cut;
        } else {
            let room = if self.reverse { self.off } else { self.src.len() - (self.off + self.len) };
            let e = (d as usize).min(room);
            if self.reverse {
                self.off -= e;
            }
            self.len += e;
        }
        self.clamp_fades();
    }

    /// Split at timeline frame `t`; `self` keeps the left part, the right part is returned.
    pub fn split_at(&mut self, t: i64, new_id: u32) -> Option<Clip> {
        if t <= self.start || t >= self.end() {
            return None;
        }
        let k = (t - self.start) as usize;
        let mut right = self.clone();
        right.id = new_id;
        right.start = t;
        right.len = self.len - k;
        if self.reverse {
            // timeline order is source order reversed: the left part is the tail of the source range
            right.off = self.off;
            self.off += self.len - k;
        } else {
            right.off = self.off + k;
        }
        self.len = k;
        right.fade_in = 0;
        self.fade_out = 0;
        self.clamp_fades();
        right.clamp_fades();
        Some(right)
    }

    /// Highest absolute sample used by this clip (before gain).
    pub fn peak(&self) -> f32 {
        let mut pk = 0f32;
        for c in &self.src.ch {
            for &v in c.get(self.off..self.off + self.len).unwrap_or(&[]) {
                pk = pk.max(v.abs());
            }
        }
        pk
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// one device channel (0-based)
    Chan(usize),
    /// stereo pair k = channels 2k and 2k+1
    Pair(usize),
    /// all channels mixed down to mono
    Mix,
}

impl Mode {
    pub fn label(self) -> String {
        match self {
            Mode::Chan(c) => format!("Ch {}", c + 1),
            Mode::Pair(k) => format!("Ch {}+{}", 2 * k + 1, 2 * k + 2),
            Mode::Mix => "Mono mix".into(),
        }
    }

    /// Capture lanes (0..ch are the channels, `ch` is the mono mix) this input needs on a device with `ch` channels.
    pub fn lanes(self, ch: usize) -> Vec<usize> {
        let ch = ch.max(1);
        match self {
            Mode::Chan(c) => vec![c.min(ch - 1)],
            Mode::Pair(k) => {
                if 2 * k + 1 < ch {
                    vec![2 * k, 2 * k + 1]
                } else {
                    vec![(2 * k).min(ch - 1)]
                }
            }
            Mode::Mix => vec![ch],
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Input {
    pub dev: String,
    pub mode: Mode,
}

/// Which channels of the clips a track plays.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Chan {
    #[default]
    Stereo,
    /// only the left channel of the source, as a mono signal (then placed by the pan control)
    Left,
    Right,
    /// both channels mixed down to one mono signal
    Mono,
}

impl Chan {
    pub fn index(self) -> u8 {
        self as u8
    }
    pub fn from_index(i: u8) -> Self {
        [Chan::Stereo, Chan::Left, Chan::Right, Chan::Mono][(i as usize).min(3)]
    }
}

#[derive(Clone)]
pub struct Track {
    pub id: u32,
    /// channel routing of stereo clips
    pub chan: Chan,
    /// gain and mute of the left / right channel of stereo clips (sub-tracks)
    pub ch_db: [f32; 2],
    pub ch_mute: [bool; 2],
    /// recording trim: gain of this input in dB and how much earlier (ms, may be negative) the take is placed,
    /// so that inputs of different interfaces line up
    pub in_db: f32,
    pub in_ms: f32,
    pub name: String,
    pub color: usize,
    pub clips: Vec<Clip>,
    pub vol_db: f32,
    /// -1 (left) .. 1 (right)
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    /// polarity: the whole track is turned upside down (null tests, phase checks)
    pub invert: bool,
    /// show the beat of the audio on this track and snap to it (Shift while selecting)
    pub beats: bool,
    /// volume automation: (frame, dB) points, sorted; the level is added to the fader and held beyond the ends
    pub auto: Vec<(i64, f32)>,
    pub input: Input,
    /// 0..=2 (small, normal, large)
    pub h: u8,
    /// a track the user draws a control signal on (a curve between -1 and 1 over the timeline): it is never part of the master
    /// mix, only a Timeline out node of the routing graph can pick it up
    pub draw: bool,
    /// the drawing: (frame, value) points, sorted; the value is held beyond the ends
    pub curve: Vec<(i64, f32)>,
}

pub const MAX_CURVE_POINTS: usize = 20000;

/// Range of the automation lane (dB).
pub const AUTO_RANGE: (f32, f32) = (-40.0, 12.0);
pub const MAX_AUTO_POINTS: usize = 2000;

impl Track {
    /// Automation level (dB) at a frame; `seg` is the index of the first point after the frame.
    fn auto_at(&self, seg: usize, f: i64) -> f32 {
        let a = &self.auto;
        if a.is_empty() {
            0.0
        } else if seg == 0 {
            a[0].1
        } else if seg >= a.len() {
            a[a.len() - 1].1
        } else {
            let ((f0, d0), (f1, d1)) = (a[seg - 1], a[seg]);
            d0 + (d1 - d0) * ((f - f0) as f32 / (f1 - f0).max(1) as f32)
        }
    }

    /// Insert a point (keeping the order; a point on the same frame is replaced).
    pub fn auto_set(&mut self, frame: i64, db: f32) -> Option<usize> {
        let db = db.clamp(AUTO_RANGE.0, AUTO_RANGE.1);
        let i = self.auto.partition_point(|p| p.0 < frame);
        if i < self.auto.len() && self.auto[i].0 == frame {
            self.auto[i].1 = db;
        } else if self.auto.len() < MAX_AUTO_POINTS {
            self.auto.insert(i, (frame.max(0), db));
        } else {
            return None;
        }
        Some(i)
    }

    /// Put a point on the drawing (replaces the point at the same frame).
    pub fn curve_set(&mut self, frame: i64, v: f32) {
        let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        let i = self.curve.partition_point(|p| p.0 < frame);
        if i < self.curve.len() && self.curve[i].0 == frame {
            self.curve[i].1 = v;
        } else if self.curve.len() < MAX_CURVE_POINTS {
            self.curve.insert(i, (frame.max(0), v));
        }
    }

    /// Draw a stroke from (`a`, `va`) to (`b`, `vb`): what was drawn between the two is replaced.
    pub fn curve_stroke(&mut self, a: i64, va: f32, b: i64, vb: f32) {
        let (lo, hi) = (a.min(b), a.max(b));
        self.curve.retain(|p| p.0 < lo || p.0 > hi);
        self.curve_set(a, va);
        if b != a {
            self.curve_set(b, vb);
        }
    }

    /// The drawing at a frame; `seg` is the index of the first point after the frame (kept between calls with rising frames).
    pub fn curve_at(&self, seg: &mut usize, f: i64) -> f32 {
        let c = &self.curve;
        while *seg < c.len() && c[*seg].0 <= f {
            *seg += 1;
        }
        if c.is_empty() {
            0.0
        } else if *seg == 0 {
            c[0].1
        } else if *seg >= c.len() {
            c[c.len() - 1].1
        } else {
            let ((f0, v0), (f1, v1)) = (c[*seg - 1], c[*seg]);
            v0 + (v1 - v0) * ((f - f0) as f32 / (f1 - f0).max(1) as f32)
        }
    }

    pub fn height(&self) -> f32 {
        [58.0, 84.0, 130.0][self.h.min(2) as usize]
    }
}

/// A named point (`len` = 0) or range of the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub id: u32,
    pub at: i64,
    pub len: i64,
    pub name: String,
}

#[derive(Clone)]
pub struct Project {
    pub sr: u32,
    pub tracks: Vec<Track>,
    pub master_db: f32,
    pub marks: Vec<Mark>,
    next_id: u32,
}

/// Pan law: balance for stereo material, constant power (-3 dB at the centre) for mono.
fn pan_gains(pan: f32, stereo: bool) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    if stereo {
        ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
    } else {
        let a = (p + 1.0) * std::f32::consts::FRAC_PI_4;
        (a.cos(), a.sin())
    }
}

impl Project {
    pub fn new(sr: u32) -> Self {
        Self { sr, tracks: Vec::new(), master_db: 0.0, marks: Vec::new(), next_id: 1 }
    }

    pub fn next(&mut self) -> u32 {
        let i = self.next_id;
        self.next_id += 1;
        i
    }

    /// Make sure ids handed out later do not collide with `id` (after loading).
    pub fn reserve_id(&mut self, id: u32) {
        self.next_id = self.next_id.max(id.saturating_add(1));
    }

    pub const MAX_MARKS: usize = 500;

    /// Add a marker (`len` 0) or region; the name defaults to the next free "Marker n" / "Region n".
    pub fn add_mark(&mut self, at: i64, len: i64, name: Option<String>) -> Option<u32> {
        // the same place twice (a held key repeats) is one marker
        if let Some(m) = self.marks.iter().find(|m| m.at == at.max(0) && m.len == len.max(0)) {
            return Some(m.id);
        }
        if self.marks.len() >= Self::MAX_MARKS {
            return None;
        }
        let id = self.next();
        let kind = if len > 0 { "Region" } else { "Marker" };
        let n = self.marks.iter().filter(|m| (m.len > 0) == (len > 0)).count() + 1;
        self.marks.push(Mark { id, at: at.max(0), len: len.max(0), name: name.unwrap_or_else(|| format!("{kind} {n}")) });
        self.marks.sort_by_key(|m| m.at);
        Some(id)
    }

    /// The marker before / after a frame (for jumping around).
    pub fn mark_near(&self, frame: i64, forward: bool) -> Option<&Mark> {
        if forward { self.marks.iter().filter(|m| m.at > frame).min_by_key(|m| m.at) } else { self.marks.iter().filter(|m| m.at < frame).max_by_key(|m| m.at) }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.clips.is_empty())
    }

    /// Frame after the last clip.
    pub fn end_frame(&self) -> i64 {
        self.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.end()).max().unwrap_or(0)
    }

    pub fn add_track(&mut self, name: Option<String>, input: Input) -> usize {
        let id = self.next();
        let n = self.tracks.len();
        self.tracks.push(Track { id, chan: Chan::Stereo, ch_db: [0.0; 2], ch_mute: [false; 2], in_db: 0.0, in_ms: 0.0, name: name.unwrap_or_else(|| format!("Track {}", id)), color: n % COLOR_COUNT, clips: Vec::new(), vol_db: 0.0, pan: 0.0, mute: false, solo: false, arm: false, invert: false, beats: false, auto: Vec::new(), input, h: 1, draw: false, curve: Vec::new() });
        n
    }

    /// A track to draw a control signal on.
    pub fn add_draw_track(&mut self) -> usize {
        let n = self.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let id = self.tracks[n].id;
        let draws = self.tracks.iter().filter(|t| t.draw).count() + 1;
        let t = &mut self.tracks[n];
        t.draw = true;
        t.name = format!("Draw {draws} (id {id})");
        t.h = 1;
        n
    }

    /// Turn a stereo track into two mono tracks (left panned hard left, right hard right). The new right
    /// track is inserted below; returns its index.
    pub fn split_channels(&mut self, ti: usize) -> Option<usize> {
        if self.tracks.len() >= MAX_TRACKS || !self.tracks[ti].clips.iter().any(|c| c.src.stereo()) {
            return None;
        }
        let mut r = self.tracks[ti].clone();
        r.id = self.next();
        for c in r.clips.iter_mut() {
            c.id = self.next();
        }
        r.arm = false;
        let l = &mut self.tracks[ti];
        let base = l.name.clone();
        l.name = format!("{base} L");
        r.name = format!("{base} R");
        for (t, chan, pan) in [(&mut *l, Chan::Left, -1.0), (&mut r, Chan::Right, 1.0)] {
            t.chan = chan;
            t.pan = pan;
            t.ch_db = [0.0; 2];
            t.ch_mute = [false; 2];
            t.h = t.h.min(1);
        }
        self.tracks.insert(ti + 1, r);
        Some(ti + 1)
    }

    pub fn track_index(&self, id: u32) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// (track index, clip index) of a clip id.
    pub fn find_clip(&self, id: u32) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|c| c.id == id).map(|ci| (ti, ci)))
    }

    /// Add a source as a new clip on track `ti` at `start`.
    pub fn add_clip(&mut self, ti: usize, src: Arc<Source>, start: i64) -> u32 {
        let id = self.next();
        let len = src.len().max(1);
        let name = src.name.clone();
        self.tracks[ti].clips.push(Clip { id, src, start: start.max(0), off: 0, len, gain_db: 0.0, fade_in: 0, fade_out: 0, reverse: false, muted: false, name });
        id
    }

    /// Mix `l.len()` frames starting at timeline frame `start`. With `only` just that track (post fader,
    /// ignoring mute / solo) is rendered. `peaks[i]` receives the post-fader peak (L, R) of track `i`.
    pub fn mix(&self, start: i64, l: &mut [f32], r: &mut [f32], only: Option<u32>, peaks: &mut [[f32; 2]]) {
        let n = l.len();
        l.fill(0.0);
        r.fill(0.0);
        let any_solo = self.tracks.iter().any(|t| t.solo);
        // the track buffers are kept between calls (the playback thread mixes every few milliseconds)
        thread_local! {
            static BUFS: std::cell::RefCell<(Vec<f32>, Vec<f32>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
        }
        let (mut tl, mut tr) = BUFS.with(|b| std::mem::take(&mut *b.borrow_mut()));
        tl.clear();
        tl.resize(n, 0.0);
        tr.clear();
        tr.resize(n, 0.0);
        for (ti, t) in self.tracks.iter().enumerate() {
            if t.draw {
                // a drawing is muted for the master mix: only a request for this very track (the routing graph) gets it
                if only == Some(t.id) {
                    let mut seg = t.curve.partition_point(|p| p.0 <= start);
                    for i in 0..n {
                        let v = t.curve_at(&mut seg, start + i as i64);
                        l[i] += v;
                        r[i] += v;
                    }
                }
                if let Some(p) = peaks.get_mut(ti) {
                    *p = [0.0; 2];
                }
                continue;
            }
            let audible = match only {
                Some(id) => t.id == id,
                None => !t.mute && (!any_solo || t.solo),
            };
            if !audible || t.clips.is_empty() {
                if let Some(p) = peaks.get_mut(ti) {
                    *p = [0.0; 2];
                }
                continue;
            }
            tl.fill(0.0);
            tr.fill(0.0);
            let mut any = false;
            let xf = ((XFADE_SECS * self.sr as f32) as i64).max(8);
            for (ci, c) in t.clips.iter().enumerate().filter(|(_, c)| !c.muted) {
                let a = start.max(c.start);
                let b = (start + n as i64).min(c.end());
                if a >= b {
                    continue;
                }
                any = true;
                // overlapping clips: the one that comes later in the list (moved last, drawn on top) replaces what is
                // under it, with short fades at the edges of the overlap
                let uppers: Vec<(i64, i64)> = t.clips.iter().enumerate().filter(|(j, o)| *j > ci && !o.muted && o.start < b && o.end() > a).map(|(_, o)| (o.start, o.end())).collect();
                let low_start = t.clips.iter().enumerate().any(|(j, o)| j < ci && !o.muted && o.start <= c.start && o.end() > c.start);
                let low_end = t.clips.iter().enumerate().any(|(j, o)| j < ci && !o.muted && o.start < c.end() && o.end() >= c.end());
                let crossing = !uppers.is_empty() || low_start || low_end;
                let xlen = xf.min((c.len as i64 / 2).max(1));
                let g = db_to_lin(c.gain_db);
                let two = c.src.stereo();
                let (pl, pr) = pan_gains(t.pan, two && t.chan == Chan::Stereo);
                let plain = c.fade_in == 0 && c.fade_out == 0;
                let cg = [if t.ch_mute[0] { 0.0 } else { db_to_lin(t.ch_db[0]) }, if t.ch_mute[1] { 0.0 } else { db_to_lin(t.ch_db[1]) }];
                for f in a..b {
                    let rel = (f - c.start) as usize;
                    let si = c.src_index(rel);
                    let mut e = if plain { g } else { c.env(rel, g) };
                    if crossing {
                        if low_start && f - c.start < xlen {
                            e *= smooth((f - c.start) as f32 / xlen as f32);
                        }
                        if low_end && c.end() - f <= xlen {
                            e *= smooth((c.end() - f) as f32 / xlen as f32);
                        }
                        for &(us, ue) in &uppers {
                            if f >= us && f < ue {
                                let xu = xf.min((ue - us) / 2).max(1);
                                let cover = smooth((f - us) as f32 / xu as f32).min(smooth((ue - f) as f32 / xu as f32));
                                e *= 1.0 - cover;
                            }
                        }
                    }
                    let (mut sl, mut sr) = (c.src.at(0, si), c.src.at(1, si));
                    if two {
                        sl *= cg[0];
                        sr *= cg[1];
                        match t.chan {
                            Chan::Stereo => {}
                            Chan::Left => sr = sl,
                            Chan::Right => sl = sr,
                            Chan::Mono => {
                                let m = 0.5 * (sl + sr);
                                sl = m;
                                sr = m;
                            }
                        }
                    }
                    let o = (f - start) as usize;
                    tl[o] += sl * e * pl;
                    tr[o] += sr * e * pr;
                }
            }
            let vg = db_to_lin(t.vol_db) * if t.invert { -1.0 } else { 1.0 };
            let (mut pl, mut pr) = (0f32, 0f32);
            if any {
                let mut seg = t.auto.partition_point(|p| p.0 <= start);
                for i in 0..n {
                    let g = if t.auto.is_empty() {
                        vg
                    } else {
                        let f = start + i as i64;
                        while seg < t.auto.len() && t.auto[seg].0 <= f {
                            seg += 1;
                        }
                        vg * db_to_lin(t.auto_at(seg, f))
                    };
                    let (a, b) = (tl[i] * g, tr[i] * g);
                    pl = pl.max(a.abs());
                    pr = pr.max(b.abs());
                    l[i] += a;
                    r[i] += b;
                }
            }
            if let Some(p) = peaks.get_mut(ti) {
                *p = [pl, pr];
            }
        }
        BUFS.with(|b| *b.borrow_mut() = (tl, tr));
        if only.is_none() {
            let mg = db_to_lin(self.master_db);
            if (mg - 1.0).abs() > 1e-6 {
                l.iter_mut().for_each(|v| *v *= mg);
                r.iter_mut().for_each(|v| *v *= mg);
            }
        }
    }

    /// Render `[a, b)` (or a single track) to stereo.
    pub fn render(&self, a: i64, b: i64, only: Option<u32>) -> (Vec<f32>, Vec<f32>) {
        let n = (b - a).max(0) as usize;
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        let mut pk = vec![[0f32; 2]; self.tracks.len()];
        const BLK: usize = 1 << 14;
        let mut o = 0;
        while o < n {
            let k = BLK.min(n - o);
            self.mix(a + o as i64, &mut l[o..o + k], &mut r[o..o + k], only, &mut pk);
            o += k;
        }
        (l, r)
    }

    // ------------------------------------------------------------------ editing

    /// Split every clip of `tracks` (all when empty) that spans `t`; only clips in `only` when given.
    pub fn split_at(&mut self, t: i64, only: Option<&[u32]>) -> usize {
        let mut made = 0;
        for ti in 0..self.tracks.len() {
            let ids: Vec<u32> = self.tracks[ti].clips.iter().filter(|c| c.start < t && c.end() > t && only.map_or(true, |o| o.contains(&c.id))).map(|c| c.id).collect();
            for id in ids {
                let nid = self.next();
                let ci = self.tracks[ti].clips.iter().position(|c| c.id == id).unwrap();
                if let Some(right) = self.tracks[ti].clips[ci].split_at(t, nid) {
                    self.tracks[ti].clips.push(right);
                    made += 1;
                }
            }
        }
        made
    }

    /// Remove the time range `[a, b)` from the given tracks (all when `tracks` is empty), leaving a gap.
    pub fn delete_range(&mut self, a: i64, b: i64, tracks: &[u32]) {
        for ti in 0..self.tracks.len() {
            if !tracks.is_empty() && !tracks.contains(&self.tracks[ti].id) {
                continue;
            }
            for t in [a, b] {
                let ids: Vec<u32> = self.tracks[ti].clips.iter().filter(|c| c.start < t && c.end() > t).map(|c| c.id).collect();
                for id in ids {
                    let nid = self.next();
                    let ci = self.tracks[ti].clips.iter().position(|c| c.id == id).unwrap();
                    if let Some(right) = self.tracks[ti].clips[ci].split_at(t, nid) {
                        self.tracks[ti].clips.push(right);
                    }
                }
            }
            self.tracks[ti].clips.retain(|c| !(c.start >= a && c.end() <= b));
        }
    }

    /// Crop a clip to `[a, b)`; false when they do not overlap.
    pub fn trim_clip_to(&mut self, id: u32, a: i64, b: i64) -> bool {
        let Some((ti, ci)) = self.find_clip(id) else { return false };
        let c = &mut self.tracks[ti].clips[ci];
        let (s, e) = (a.max(c.start), b.min(c.end()));
        if s >= e {
            return false;
        }
        c.set_end(e);
        c.set_start(s);
        true
    }

    pub fn duplicate_clip(&mut self, id: u32, at: Option<i64>) -> Option<u32> {
        let (ti, ci) = self.find_clip(id)?;
        let mut c = self.tracks[ti].clips[ci].clone();
        c.id = self.next();
        c.start = at.unwrap_or(c.end());
        let nid = c.id;
        self.tracks[ti].clips.push(c);
        Some(nid)
    }

    pub fn clip_mut(&mut self, id: u32) -> Option<&mut Clip> {
        let (ti, ci) = self.find_clip(id)?;
        Some(&mut self.tracks[ti].clips[ci])
    }

    /// Set the clip gain so that its peak lands on `target_db`.
    pub fn normalize_clip(&mut self, id: u32, target_db: f32) -> bool {
        let Some(c) = self.clip_mut(id) else { return false };
        let pk = c.peak();
        if pk < 1e-6 {
            return false;
        }
        c.gain_db = target_db - 20.0 * pk.log10();
        true
    }

    /// Bake a time range of the mix (or one track) into a new source.
    pub fn bounce(&self, a: i64, b: i64, only: Option<u32>, id: u32, name: String) -> Arc<Source> {
        let (l, r) = self.render(a, b, only);
        Arc::new(Source::new(id, name, vec![l, r]))
    }
}

pub const COLOR_COUNT: usize = 8;
pub const TRACK_COLORS: [(u8, u8, u8); COLOR_COUNT] = [(96, 165, 250), (74, 222, 128), (250, 204, 21), (248, 113, 113), (192, 132, 252), (45, 212, 191), (251, 146, 60), (244, 114, 182)];

// ---------------------------------------------------------------------- resampling

/// Windowed-sinc resampler (Kaiser-like window, 32 taps) for sample rate conversion at import and record.
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let ratio = to as f64 / from as f64;
    // cut-off relative to the input rate: lower the band limit when going down
    let fc = if ratio < 1.0 { ratio * 0.97 * 0.5 } else { 0.5 * 0.99 };
    const HALF: isize = 16;
    const PHASES: usize = 256;
    let taps = (2 * HALF) as usize;
    let bessel = |x: f64| {
        let (mut s, mut t, mut k) = (1.0f64, 1.0f64, 1.0f64);
        while t > 1e-12 * s {
            t *= (x / 2.0 / k) * (x / 2.0 / k);
            s += t;
            k += 1.0;
        }
        s
    };
    let beta = 7.0f64;
    let i0b = bessel(beta);
    let mut table = vec![0f32; (PHASES + 1) * taps];
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        let mut sum = 0.0;
        for k in 0..taps {
            let d = (k as isize - HALF + 1) as f64 - frac;
            let w = {
                let u = d / HALF as f64;
                if u.abs() >= 1.0 { 0.0 } else { bessel(beta * (1.0 - u * u).sqrt()) / i0b }
            };
            let s = if d.abs() < 1e-12 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * d).sin() / (std::f64::consts::PI * d) };
            let v = s * w;
            table[p * taps + k] = v as f32;
            sum += v;
        }
        for k in 0..taps {
            table[p * taps + k] = (table[p * taps + k] as f64 / sum) as f32;
        }
    }
    let n_out = ((x.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(n_out);
    let n = x.len() as isize;
    for o in 0..n_out {
        let pos = o as f64 / ratio;
        let i = pos.floor() as isize;
        let frac = pos - i as f64;
        let ph = (frac * PHASES as f64).round() as usize;
        let tab = &table[ph * taps..(ph + 1) * taps];
        let mut acc = 0f32;
        let base = i - HALF + 1;
        if base >= 0 && base + taps as isize <= n {
            for (k, &w) in tab.iter().enumerate() {
                acc += x[(base + k as isize) as usize] * w;
            }
        } else {
            for (k, &w) in tab.iter().enumerate() {
                let j = base + k as isize;
                if j >= 0 && j < n {
                    acc += x[j as usize] * w;
                }
            }
        }
        out.push(acc);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_beat_of_dense_music_is_found() {
        use super::*;
        let sr = 44100u32;
        // 100 bpm: a kick on every beat, a hat on the off-beats, over a loud steady pad and some noise
        let mut x = 777u32;
        let mut rnd = move || { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 8) as f32 / 8388608.0 - 1.0 };
        let beat = (sr as f32 * 0.6) as usize;
        let mut v = vec![0f32; sr as usize * 30];
        for (i, s) in v.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            *s = 0.25 * (2.0 * std::f32::consts::PI * 220.0 * t).sin() + 0.2 * (2.0 * std::f32::consts::PI * 330.0 * t).sin() + 0.1 * rnd();
        }
        for k in 0..48 {
            let at = k * beat + 5000;
            for i in 0..6000 {
                if at + i < v.len() {
                    v[at + i] += 0.6 * (-(i as f32) / 1500.0).exp() * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / sr as f32).sin();
                }
            }
            let h = at + beat / 2;
            for i in 0..1500 {
                if h + i < v.len() {
                    v[h + i] += 0.25 * (-(i as f32) / 300.0).exp() * rnd();
                }
            }
        }
        let g = find_beat_grid(&[v], sr).expect("music with a kick has a beat");
        assert!((g.bpm - 100.0).abs() < 3.0 || (g.bpm - 200.0).abs() < 4.0, "{}", g.bpm);
        assert!(g.beats.len() > 25);
    }

    #[test]
    fn the_beat_of_a_click_track_is_found_and_noise_has_none() {
        use super::*;
        let sr = 48000u32;
        // 120 bpm: a short decaying burst every half second, 20 s long
        let mut v = vec![0f32; sr as usize * 20];
        let beat = sr as usize / 2;
        for k in 0..40 {
            for i in 0..2400 {
                v[k * beat + 3000 + i] = 0.7 * (-(i as f32) / 500.0).exp() * (i as f32 * 0.4).sin();
            }
        }
        let s = Source::new(1, "t".into(), vec![v]);
        let g = find_beat_grid(&s.ch, sr).expect("a click track has a beat");
        assert!((g.bpm - 120.0).abs() < 2.0, "{}", g.bpm);
        let hit = g.beats.iter().filter(|&&b| (0..40).any(|k| (b as i64 - (k * beat + 3000) as i64).abs() < 1500)).count();
        assert!(hit as f32 >= 0.9 * g.beats.len() as f32 && g.beats.len() >= 30, "{} of {}", hit, g.beats.len());
        // a steady noise has no beat
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..sr as usize * 20).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 8) as f32 / 8388608.0 - 1.0 }).map(|v| v * 0.3).collect();
        assert!(find_beat_grid(&[noise], sr).is_none());
    }

    use super::*;

    fn src(n: usize) -> Arc<Source> {
        let l: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        Arc::new(Source::new(1, "ramp".into(), vec![l]))
    }

    fn proj_with_clip(n: usize) -> (Project, u32) {
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let id = p.add_clip(ti, src(n), 100);
        (p, id)
    }

    #[test]
    fn an_inverted_track_cancels_its_twin_and_markers_stay_ordered() {
        let mut p = Project::new(48000);
        let src = Arc::new(Source::new(1, "s".into(), vec![(0..2000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect()]));
        for _ in 0..2 {
            let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
            p.add_clip(ti, src.clone(), 0);
        }
        let (l, _) = p.render(0, 2000, None);
        assert!(l.iter().any(|v| v.abs() > 0.3));
        p.tracks[1].invert = true;
        let (l, r) = p.render(0, 2000, None);
        assert!(l.iter().chain(r.iter()).all(|v| v.abs() < 1e-6), "two identical tracks, one inverted, are silent");
        p.add_mark(900, 0, None);
        p.add_mark(100, 50, None);
        assert_eq!(p.marks.iter().map(|m| m.at).collect::<Vec<_>>(), vec![100, 900]);
        assert_eq!(p.mark_near(500, true).map(|m| m.at), Some(900));
        assert_eq!(p.mark_near(500, false).map(|m| m.at), Some(100));
        assert!(p.mark_near(900, true).is_none());
        // a held key adds the same marker again and again: it stays one
        let n = p.marks.len();
        let a = p.add_mark(900, 0, None);
        assert_eq!(p.marks.len(), n);
        assert_eq!(a, p.marks.iter().find(|m| m.at == 900).map(|m| m.id));
    }

    #[test]
    fn volume_automation_follows_its_points_across_block_boundaries() {
        let mut p = Project::new(48000);
        let src = Arc::new(Source::new(1, "dc".into(), vec![vec![0.5; 4000]]));
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        p.add_clip(ti, src, 0);
        p.tracks[0].pan = -1.0;
        let flat = p.render(0, 4000, None).0;
        p.tracks[0].auto_set(1000, 0.0);
        p.tracks[0].auto_set(2000, -20.0);
        p.tracks[0].auto_set(1000, 0.0);
        assert_eq!(p.tracks[0].auto.len(), 2);
        let a = p.render(0, 4000, None).0;
        assert!((a[500] - flat[500]).abs() < 1e-6, "held before the first point");
        assert!((a[1500] / flat[1500] - 10f32.powf(-10.0 / 20.0)).abs() < 1e-3, "halfway: -10 dB");
        assert!((a[3500] / flat[3500] - 0.1).abs() < 1e-3, "held after the last point");
        // the same result when rendered in small blocks
        let mut l = vec![0.0f32; 4000];
        let mut at = 0;
        for n in [300usize, 777, 1, 1500, 1422] {
            let (mut bl, mut br) = (vec![0.0f32; n], vec![0.0f32; n]);
            p.mix(at as i64, &mut bl, &mut br, None, &mut [[0.0; 2]]);
            l[at..at + n].copy_from_slice(&bl);
            at += n;
        }
        assert!(l.iter().zip(&a).all(|(x, y)| (x - y).abs() < 1e-5));
        assert!(p.tracks[0].auto_set(5, 99.0).is_some() && p.tracks[0].auto[0].1 == AUTO_RANGE.1);
    }

    #[test]
    fn a_drawn_track_is_not_in_the_master_mix_but_a_node_can_take_it() {
        let mut p = Project::new(48000);
        let n = p.add_draw_track();
        let id = p.tracks[n].id;
        // a ramp from -1 at frame 100 to 1 at frame 300, drawn in two strokes (the second replaces part of the first)
        p.tracks[n].curve_stroke(100, -1.0, 200, 0.0);
        p.tracks[n].curve_stroke(150, 0.5, 300, 1.0);
        assert!(p.tracks[n].curve.windows(2).all(|w| w[0].0 < w[1].0));
        let (mut l, mut r, mut pk) = (vec![9.0f32; 400], vec![9.0f32; 400], vec![[0.0f32; 2]; 4]);
        p.mix(0, &mut l, &mut r, None, &mut pk);
        assert!(l.iter().chain(r.iter()).all(|v| *v == 0.0), "muted for the master");
        p.mix(0, &mut l, &mut r, Some(id), &mut pk);
        assert_eq!(l, r);
        assert_eq!((l[0], l[100], l[300], l[399]), (-1.0, -1.0, 1.0, 1.0), "held beyond the ends");
        assert!((l[150] - 0.5).abs() < 1e-5 && l[125] > -1.0 && l[125] < 0.5, "{} {}", l[150], l[125]);
    }

    #[test]
    fn mix_places_clip_and_applies_gain_and_pan() {
        let (mut p, _) = proj_with_clip(1000);
        p.tracks[0].vol_db = -6.0206;
        p.tracks[0].pan = -1.0;
        let (mut l, mut r) = (vec![0f32; 1200], vec![0f32; 1200]);
        let mut pk = vec![[0f32; 2]; 1];
        p.mix(0, &mut l, &mut r, None, &mut pk);
        assert_eq!(l[50], 0.0);
        // frame 100 + 500 = ramp 0.5, -6 dB, hard left
        assert!((l[600] - 0.25).abs() < 1e-3, "{}", l[600]);
        assert!(r[600].abs() < 1e-6);
        assert!(pk[0][0] > 0.4);
        assert_eq!(l[1150], 0.0);
    }

    #[test]
    fn channel_routing_and_split() {
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let s = Arc::new(Source::new(2, "st".into(), vec![vec![0.5; 200], vec![0.25; 200]]));
        p.add_clip(ti, s, 0);
        let at = |p: &Project| {
            let (l, r) = p.render(10, 20, None);
            (l[0], r[0])
        };
        assert_eq!(at(&p), (0.5, 0.25));
        p.tracks[0].ch_mute[1] = true;
        assert_eq!(at(&p), (0.5, 0.0));
        p.tracks[0].ch_mute[1] = false;
        p.tracks[0].chan = Chan::Mono;
        let (l, r) = at(&p);
        assert!((l - r).abs() < 1e-6 && l > 0.2);
        p.tracks[0].chan = Chan::Stereo;
        let (a, b) = (p.tracks[0].id, p.split_channels(0).unwrap());
        assert_eq!(p.tracks.len(), 2);
        assert_eq!((p.tracks[0].chan, p.tracks[b].chan), (Chan::Left, Chan::Right));
        assert_ne!(a, p.tracks[b].id);
        // left stays hard left at full level, right hard right
        let (l, r) = at(&p);
        assert!((l - 0.5).abs() < 1e-5 && (r - 0.25).abs() < 1e-5, "{l} {r}");
    }

    #[test]
    fn overlapping_clips_crossfade_instead_of_clicking() {
        let mut p = Project::new(48000);
        let ti = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let flat = |v: f32, n: usize| Arc::new(Source::new(9, "c".into(), vec![vec![v; n]]));
        p.add_clip(ti, flat(1.0, 4800), 0);
        // a quieter clip lies on top of the middle of the first one (added last = on top)
        p.add_clip(ti, flat(0.5, 960), 2000);
        let (l, _) = p.render(0, 4800, None);
        let pan = 1.0 / std::f32::consts::SQRT_2; // mono material is panned with -3 dB at the centre
        // untouched parts are as before, the top clip replaces what is under it in the middle
        assert!((l[500] - pan).abs() < 1e-3);
        assert!((l[2480] - 0.5 * pan).abs() < 1e-3, "{}", l[2480]);
        assert!((l[4000] - pan).abs() < 1e-3);
        // around the edges the level moves smoothly: no jump bigger than a small step between samples
        let worst = (1990..3000).map(|i| (l[i + 1] - l[i]).abs()).fold(0f32, f32::max);
        assert!(worst < 0.05, "step {worst}");
        // a clip without anything under it keeps its hard edge
        let mut q = Project::new(48000);
        let t2 = q.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        q.add_clip(t2, flat(1.0, 1000), 100);
        let (m, _) = q.render(0, 1200, None);
        assert!((m[100] - pan).abs() < 1e-3 && (m[1099] - pan).abs() < 1e-3);
    }

    #[test]
    fn mute_and_solo() {
        let (mut p, _) = proj_with_clip(500);
        let t2 = p.add_track(None, Input { dev: String::new(), mode: Mode::Mix });
        let s = src(500);
        p.add_clip(t2, s, 100);
        p.tracks[0].mute = true;
        let (l, _) = p.render(100, 600, None);
        let (l1, _) = p.render(100, 600, Some(p.tracks[1].id));
        assert!((l[250] - l1[250]).abs() < 1e-6);
        p.tracks[0].mute = false;
        p.tracks[1].solo = true;
        let (l2, _) = p.render(100, 600, None);
        assert!((l2[250] - l1[250]).abs() < 1e-6);
        // `only` ignores mute
        p.tracks[1].mute = true;
        assert!(p.render(100, 600, Some(p.tracks[1].id)).0[250] > 0.0);
    }

    #[test]
    fn split_and_reverse_split_cover_the_same_audio() {
        for rev in [false, true] {
            let (mut p, id) = proj_with_clip(1000);
            p.clip_mut(id).unwrap().reverse = rev;
            let before = p.render(0, 1200, None).0;
            assert_eq!(p.split_at(400, None), 1);
            assert_eq!(p.tracks[0].clips.len(), 2);
            let after = p.render(0, 1200, None).0;
            for i in 0..1200 {
                assert!((before[i] - after[i]).abs() < 1e-6, "rev {rev} frame {i}");
            }
            let total: usize = p.tracks[0].clips.iter().map(|c| c.len).sum();
            assert_eq!(total, 1000);
        }
    }

    #[test]
    fn trim_keeps_alignment_of_the_audio() {
        for rev in [false, true] {
            let (mut p, id) = proj_with_clip(1000);
            p.clip_mut(id).unwrap().reverse = rev;
            let before = p.render(0, 1200, None).0;
            {
                let c = p.clip_mut(id).unwrap();
                c.set_start(300);
                c.set_end(900);
            }
            let c = &p.tracks[0].clips[0];
            assert_eq!((c.start, c.end()), (300, 900));
            let after = p.render(0, 1200, None).0;
            for i in 300..900 {
                assert!((before[i] - after[i]).abs() < 1e-6, "rev {rev} frame {i}");
            }
            assert_eq!(after[200], 0.0);
            // extending again restores the original audio
            let c = p.clip_mut(id).unwrap();
            c.set_start(100);
            c.set_end(1100);
            assert_eq!((c.start, c.len), (100, 1000));
            let again = p.render(0, 1200, None).0;
            for i in 0..1200 {
                assert!((before[i] - again[i]).abs() < 1e-6, "rev {rev} re-extend frame {i}");
            }
        }
    }

    #[test]
    fn delete_range_leaves_gap() {
        let (mut p, _) = proj_with_clip(1000);
        let before = p.render(0, 1200, None).0;
        p.delete_range(300, 500, &[]);
        let after = p.render(0, 1200, None).0;
        for i in 0..1200 {
            let want = if (300..500).contains(&i) { 0.0 } else { before[i] };
            assert!((after[i] - want).abs() < 1e-6, "frame {i}");
        }
    }

    #[test]
    fn fades_and_normalize() {
        let (mut p, id) = proj_with_clip(1000);
        {
            let c = p.clip_mut(id).unwrap();
            c.fade_in = 100;
            c.fade_out = 100;
        }
        let (l, _) = p.render(0, 1200, None);
        assert!(l[100] < 0.01);
        assert!(l[1099] < 0.2);
        assert!(p.normalize_clip(id, -6.0));
        let c = p.clip_mut(id).unwrap();
        assert!((c.peak() * db_to_lin(c.gain_db) - 0.5012).abs() < 1e-3);
    }

    #[test]
    fn resampler_keeps_a_tone() {
        let sr = 44100.0f32;
        let x: Vec<f32> = (0..44100).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin() * 0.8).collect();
        let y = resample(&x, 44100, 48000);
        assert!((y.len() as i64 - 48000).abs() <= 1);
        let (mut num, mut den) = (0f64, 0f64);
        for (i, &v) in y.iter().enumerate().skip(2000).take(40000) {
            let want = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin() * 0.8;
            num += ((v - want) as f64).powi(2);
            den += (want as f64).powi(2);
        }
        assert!(num / den < 1e-4, "error {}", num / den);
        let z = resample(&y, 48000, 22050);
        assert!((z.len() as i64 - 22050).abs() <= 1);
    }

    #[test]
    fn input_lanes() {
        assert_eq!(Mode::Chan(3).lanes(8), vec![3]);
        assert_eq!(Mode::Pair(1).lanes(8), vec![2, 3]);
        assert_eq!(Mode::Pair(1).lanes(3), vec![2]);
        assert_eq!(Mode::Mix.lanes(2), vec![2]);
        assert_eq!(Mode::Chan(9).lanes(2), vec![1]);
    }
}
