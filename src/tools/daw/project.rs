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
}

impl Source {
    pub fn new(id: u32, name: String, ch: Vec<Vec<f32>>) -> Self {
        let ov = ch.iter().map(|c| c.chunks(OV_BLOCK).map(|b| b.iter().fold((0f32, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))).collect()).collect();
        Self { id, name, ch, ov, saved: Mutex::new(None) }
    }

    pub fn len(&self) -> usize {
        self.ch.first().map_or(0, |c| c.len())
    }

    pub fn stereo(&self) -> bool {
        self.ch.len() > 1
    }

    #[inline]
    pub fn at(&self, c: usize, i: usize) -> f32 {
        self.ch[c.min(self.ch.len() - 1)][i]
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
            for &v in &c[self.off..self.off + self.len] {
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
    pub input: Input,
    /// 0..=2 (small, normal, large)
    pub h: u8,
}

impl Track {
    pub fn height(&self) -> f32 {
        [58.0, 84.0, 130.0][self.h.min(2) as usize]
    }
}

#[derive(Clone)]
pub struct Project {
    pub sr: u32,
    pub tracks: Vec<Track>,
    pub master_db: f32,
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
        Self { sr, tracks: Vec::new(), master_db: 0.0, next_id: 1 }
    }

    pub fn next(&mut self) -> u32 {
        let i = self.next_id;
        self.next_id += 1;
        i
    }

    /// Make sure ids handed out later do not collide with `id` (after loading).
    pub fn reserve_id(&mut self, id: u32) {
        self.next_id = self.next_id.max(id + 1);
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
        self.tracks.push(Track { id, chan: Chan::Stereo, ch_db: [0.0; 2], ch_mute: [false; 2], in_db: 0.0, in_ms: 0.0, name: name.unwrap_or_else(|| format!("Track {}", id)), color: n % COLOR_COUNT, clips: Vec::new(), vol_db: 0.0, pan: 0.0, mute: false, solo: false, arm: false, input, h: 1 });
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
        let (mut tl, mut tr) = (vec![0f32; n], vec![0f32; n]);
        for (ti, t) in self.tracks.iter().enumerate() {
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
            let vg = db_to_lin(t.vol_db);
            let (mut pl, mut pr) = (0f32, 0f32);
            if any {
                for i in 0..n {
                    let (a, b) = (tl[i] * vg, tr[i] * vg);
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
