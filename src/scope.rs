//! Triggered oscilloscope.

use crate::app::*;
use crate::cal::*;
use crate::shared::*;
use crate::views::{fclamp, BG, GRID, GRID_MAJOR, TXT};
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
pub struct Meas {
    pub vmax: f32,
    pub vmin: f32,
    pub vrms: f32,
    pub vmean: f32,
    pub freq: f32,
}

pub struct ScopeState {
    /// seconds per horizontal division (10 divisions across)
    pub t_div: f32,
    /// display units (V, or FS when uncalibrated) per vertical division (8 divisions)
    pub v_div: f32,
    pub v_off: f32,
    pub show: [bool; 2],
    pub trig_src: usize,
    /// 0 = auto, 1 = normal, 2 = single
    pub mode: u8,
    pub rising: bool,
    /// trigger level in display units of the trigger source
    pub level: f32,
    /// horizontal position of the trigger point (0..1 of the screen)
    pub pos: f32,
    /// trigger hysteresis, % of full scale
    pub hyst: f32,
    /// low-pass the trigger signal (not the displayed trace) so HF noise cannot cause false triggers
    pub hf_reject: bool,
    pub hf_hz: f32,
    pub running: bool,
    pub armed: bool,
    frame: [Vec<f32>; 2],
    frame_sr: f32,
    triggered: bool,
    tbuf: [Vec<f32>; 2],
    /// ring position `tbuf` was read at (u64::MAX: nothing valid), so a frame without new samples does not copy again
    tbuf_total: u64,
    /// the low-passed trigger source and what it was made from (position, window, channel, cut-off, rate)
    zp: ZeroPhase,
    zp_key: Option<(u64, usize, usize, u32, u32)>,
    last_trig: Instant,
    meas: [Meas; 2],
    meas_t: Instant,
    /// the colours of the two traces, and whether they add up where they overlap (the colours mix into a third one)
    pub col: [Color32; 2],
    pub blend: bool,
    /// roll mode over minutes: min / max per millisecond of the last ten minutes (L min, L max, R min, R max)
    roll: Vec<[f32; 4]>,
    roll_n: u64,
    roll_total: u64,
    roll_acc: [f32; 4],
    roll_cnt: f64,
    /// width of the plot in pixels (what the long roll is reduced to)
    plot_w: usize,
}

/// Columns of the long roll history and their rate.
const ROLL_CAP: usize = 600_000;
const ROLL_RATE: f64 = 1000.0;

/// 4th-order Butterworth-style low-pass applied forwards and backwards (zero phase, so the trigger
/// point does not move relative to the displayed signal). The trigger search starts at the newest end and
/// normally stops within a period or two, so the filter works lazily: the backward pass starts at the end of
/// the buffer (exactly as a whole-buffer filter does) and runs in segments towards the start, only as far as the
/// search reaches. The forward pass of a segment is warmed up over `margin` samples first (the start-up
/// transient of a zero state has died away by then), so the result matches the whole-buffer filter to rounding.
struct ZeroPhase {
    /// filtered samples, valid from `done` to the end
    y: Vec<f32>,
    done: usize,
    c: [f64; 4],
    margin: usize,
    seg: usize,
    /// state of the backward pass at the start of the part that is done
    bz: (f64, f64),
}

impl ZeroPhase {
    fn new() -> Self {
        Self { y: Vec::new(), done: 0, c: [0.0; 4], margin: 0, seg: 8192, bz: (0.0, 0.0) }
    }

    /// Start over for a buffer of `len` samples.
    fn reset(&mut self, len: usize, fc: f32, sr: f32) {
        let fc = fc.clamp(10.0, sr * 0.45) as f64;
        let w0 = std::f64::consts::TAU * fc / sr as f64;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        self.c = [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, -2.0 * c / a0, (1.0 - alpha) / a0];
        // the poles lie on a circle of radius sqrt(a2): the transient is down to 1e-9 after ln(1e-9) / ln(r) samples
        let r = self.c[3].max(1e-12).sqrt();
        self.margin = ((-20.7 / r.ln().min(-1e-9)) as usize + 8).min(len);
        self.seg = self.margin.max(8192);
        self.y.clear();
        self.y.resize(len, 0.0);
        self.done = len;
        self.bz = (0.0, 0.0);
    }

    /// Make `y[need..]` valid (`x` is the unfiltered buffer).
    fn ensure(&mut self, x: &[f32], need: usize) {
        let [b0, b1, a1, a2] = self.c;
        while self.done > need {
            let b = self.done;
            let a = b.saturating_sub(self.seg);
            let w = a.saturating_sub(self.margin);
            let (mut z1, mut z2) = (0.0f64, 0.0f64);
            for j in w..b {
                let xi = x[j] as f64;
                let out = b0 * xi + z1;
                z1 = b1 * xi - a1 * out + z2;
                z2 = b0 * xi - a2 * out;
                self.y[j] = out as f32;
            }
            let (mut z1, mut z2) = self.bz;
            for j in (a..b).rev() {
                let xi = self.y[j] as f64;
                let out = b0 * xi + z1;
                z1 = b1 * xi - a1 * out + z2;
                z2 = b0 * xi - a2 * out;
                self.y[j] = out as f32;
            }
            self.bz = (z1, z2);
            self.done = a;
        }
    }
}

/// Newest crossing of `lvl` in `raw` (or in its low-passed version `zp`) between `lo` and `hi`, as the scope looks for it.
fn find_trigger(raw: &[f32], mut zp: Option<&mut ZeroPhase>, lvl: f32, hy: f32, rising: bool, lo: usize, hi: usize) -> Option<usize> {
    let mut i = hi;
    while i > lo {
        let x: &[f32] = match zp.as_deref_mut() {
            Some(z) => {
                if i < z.done + 12 {
                    z.ensure(raw, i - 12);
                }
                &z.y
            }
            None => raw,
        };
        let (a, b) = (x[i - 1], x[i]);
        let hit = if rising { a < lvl && b >= lvl && x[i - 12] < lvl - hy } else { a > lvl && b <= lvl && x[i - 12] > lvl + hy };
        if hit {
            return Some(i);
        }
        i -= 1;
    }
    None
}

/// Copy the newest `l.len()` samples like `Ring::copy_last`, but with the lock taken for short pieces (the audio callback
/// waits for it; a window of up to a million samples in one go would hold it up). Oldest piece first and in the same
/// lock as the position, so what is read last is the newest and cannot have been overwritten. The first lock is a
/// `try_lock` (None: busy, skip this frame). With `have == total` the buffers already hold this window and are left alone.
fn snapshot_last(ring: &parking_lot::Mutex<Ring>, l: &mut [f32], r: &mut [f32], have: u64) -> Option<u64> {
    const PIECE: usize = 8192;
    let g = ring.try_lock()?;
    let total = g.total;
    if total == have {
        return Some(total);
    }
    let n = l.len();
    // (callers keep a margin of a few pieces to the size of the ring)
    let avail = (total as usize).min(n).min(RING_CAP - PIECE);
    let pad = n - avail;
    l[..pad].fill(0.0);
    r[..pad].fill(0.0);
    let start = total as usize - avail;
    let first = PIECE.min(avail);
    g.copy_abs(start, &mut l[pad..pad + first], &mut r[pad..pad + first]);
    drop(g);
    let mut done = first;
    while done < avail {
        let m = PIECE.min(avail - done);
        ring.lock().copy_abs(start + done, &mut l[pad + done..pad + done + m], &mut r[pad + done..pad + done + m]);
        done += m;
    }
    Some(total)
}

impl ScopeState {
    pub fn new() -> Self {
        let long_ago = Instant::now().checked_sub(Duration::from_secs(5)).unwrap_or_else(Instant::now);
        Self {
            t_div: 1e-3,
            v_div: 0.25,
            v_off: 0.0,
            show: [true, true],
            trig_src: 0,
            mode: 0,
            rising: true,
            level: 0.0,
            pos: 0.5,
            hyst: 0.2,
            hf_reject: false,
            hf_hz: 5000.0,
            running: true,
            armed: true,
            frame: Default::default(),
            frame_sr: 48000.0,
            triggered: false,
            tbuf: Default::default(),
            tbuf_total: u64::MAX,
            zp: ZeroPhase::new(),
            zp_key: None,
            last_trig: long_ago,
            meas: Default::default(),
            meas_t: long_ago,
            col: CH_COLORS,
            blend: false,
            roll: Vec::new(),
            roll_n: 0,
            roll_total: 0,
            roll_acc: [f32::MAX, f32::MIN, f32::MAX, f32::MIN],
            roll_cnt: 0.0,
            plot_w: 800,
        }
    }

    fn n_win(&self, sr: f32) -> usize {
        if self.mode == 3 {
            // roll mode goes on to minutes (the history of the last ten)
            return ((10.0 * self.t_div * sr).round() as usize).clamp(64, (ROLL_CAP as f64 / ROLL_RATE * sr as f64) as usize);
        }
        ((10.0 * self.t_div * sr).round() as usize).clamp(64, RING_CAP / 2 - 16)
    }

    /// The window of the roll is longer than the ring of samples holds: it is drawn from the history.
    fn long_roll(&self, sr: f32) -> bool {
        self.mode == 3 && self.n_win(sr) > RING_CAP / 2 - 16
    }

    /// The longest time per division of the current mode.
    pub fn max_t_div_for(&self, sr: f32) -> f32 {
        if self.mode == 3 { (ROLL_CAP as f64 / ROLL_RATE / 10.0) as f32 } else { Self::max_t_div(sr) }
    }

    /// Fold the samples that came in since the last call into the history (one min / max pair per millisecond).
    fn roll_feed(&mut self, sh: &Shared, sr: f32) {
        if self.roll.is_empty() {
            self.roll = vec![[0.0; 4]; ROLL_CAP];
        }
        let (a, b);
        let total;
        {
            let Some(ring) = sh.ring.try_lock() else { return };
            total = ring.total;
            if self.roll_total == 0 || total < self.roll_total {
                self.roll_total = total;
                return;
            }
            let new = ((total - self.roll_total).min(RING_CAP as u64)) as usize;
            if new == 0 {
                return;
            }
            let (mut x, mut y) = (vec![0f32; new], vec![0f32; new]);
            ring.copy_last(&mut x, &mut y);
            (a, b) = (x, y);
        }
        self.roll_total = total;
        let spc = sr as f64 / ROLL_RATE;
        for i in 0..a.len() {
            let acc = &mut self.roll_acc;
            acc[0] = acc[0].min(a[i]);
            acc[1] = acc[1].max(a[i]);
            acc[2] = acc[2].min(b[i]);
            acc[3] = acc[3].max(b[i]);
            self.roll_cnt += 1.0;
            if self.roll_cnt >= spc {
                self.roll_cnt -= spc;
                let slot = (self.roll_n % ROLL_CAP as u64) as usize;
                self.roll[slot] = *acc;
                self.roll_n += 1;
                *acc = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
            }
        }
    }

    /// The window of a long roll reduced to one min / max pair per pixel of the plot.
    fn build_long(&mut self, sr: f32) {
        let w = self.plot_w.clamp(100, 4000);
        let ncols = ((self.n_win(sr) as f64 / sr as f64) * ROLL_RATE).round().max(1.0);
        let newest = self.roll_n as f64;
        let oldest = (self.roll_n.saturating_sub(ROLL_CAP as u64)) as f64;
        for c in 0..2 {
            self.frame[c].clear();
            self.frame[c].reserve(2 * w);
        }
        for px in 0..w {
            let (r0, r1) = (newest - ncols + px as f64 * ncols / w as f64, newest - ncols + (px + 1) as f64 * ncols / w as f64);
            // (before the first recorded column there is nothing)
            let c0 = r0.floor().max(oldest);
            let c1 = if r1 <= oldest { c0 } else { r1.ceil().min(newest).max(c0 + 1.0).min(newest) };
            let mut m = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
            let mut c = c0;
            // (a long span is sampled sparsely: a few hundred columns per pixel are plenty)
            let step = ((c1 - c0) / 256.0).max(1.0);
            while c < c1 {
                let col = self.roll[(c as u64 % ROLL_CAP as u64) as usize];
                m[0] = m[0].min(col[0]);
                m[1] = m[1].max(col[1]);
                m[2] = m[2].min(col[2]);
                m[3] = m[3].max(col[3]);
                c += step;
            }
            for k in 0..2 {
                let (lo, hi) = if m[2 * k] > m[2 * k + 1] { (0.0, 0.0) } else { (m[2 * k], m[2 * k + 1]) };
                self.frame[k].push(lo);
                self.frame[k].push(hi);
            }
        }
    }

    pub fn min_t_div(sr: f32) -> f32 {
        64.0 / (10.0 * sr)
    }

    pub fn max_t_div(sr: f32) -> f32 {
        (RING_CAP as f32 / 2.0 - 16.0) / (10.0 * sr)
    }

    fn frame_id_bump(&mut self) {}

    /// Grab the newest samples and align them to a trigger crossing.
    fn acquire(&mut self, sh: &Shared, scale: [f32; 2]) {
        let sr = sh.sr.load(Relaxed) as f32;
        if sr <= 0.0 || !self.running || (self.mode == 2 && !self.armed) {
            return;
        }
        let n_win = self.n_win(sr);
        if self.mode == 3 {
            self.roll_feed(sh, sr);
            if self.long_roll(sr) {
                self.build_long(sr);
                self.frame_sr = sr;
                self.triggered = false;
                self.frame_id_bump();
                return;
            }
            // roll mode: no trigger, always show the newest n_win samples scrolling in from the right
            if self.tbuf[0].len() != n_win {
                self.tbuf_total = u64::MAX;
            }
            for c in 0..2 {
                self.tbuf[c].resize(n_win, 0.0);
            }
            {
                let (a, b) = self.tbuf.split_at_mut(1);
                let Some(t) = snapshot_last(&sh.ring, &mut a[0], &mut b[0], self.tbuf_total) else { return };
                self.tbuf_total = t;
            }
            for c in 0..2 {
                let (f, t) = (&mut self.frame[c], &self.tbuf[c]);
                f.clear();
                f.extend_from_slice(t);
            }
            self.frame_sr = sr;
            self.triggered = false;
            return;
        }
        let pre = ((self.pos * n_win as f32) as usize).min(n_win - 1);
        let post = n_win - pre;
        // (a couple of pieces short of the ring, see snapshot_last)
        let m = (n_win * 2).min(RING_CAP - 2 * 8192);
        if self.tbuf[0].len() != m {
            self.tbuf_total = u64::MAX;
        }
        for c in 0..2 {
            self.tbuf[c].resize(m, 0.0);
        }
        {
            let (a, b) = self.tbuf.split_at_mut(1);
            let Some(t) = snapshot_last(&sh.ring, &mut a[0], &mut b[0], self.tbuf_total) else { return };
            self.tbuf_total = t;
        }
        let src = self.trig_src;
        let zp = if self.hf_reject {
            let key = (self.tbuf_total, m, src, self.hf_hz.to_bits(), sr.to_bits());
            if self.zp_key != Some(key) {
                self.zp.reset(m, self.hf_hz, sr);
                self.zp_key = Some(key);
            }
            Some(&mut self.zp)
        } else {
            None
        };
        let lvl = self.level / scale[src].max(1e-12);
        let hy = self.hyst * 0.01;
        let hi = m - post - 1;
        let lo = pre.max(16);
        let found = find_trigger(&self.tbuf[src], zp, lvl, hy, self.rising, lo, hi);
        self.frame_sr = sr;
        match found {
            Some(i) => {
                for c in 0..2 {
                    let (f, t) = (&mut self.frame[c], &self.tbuf[c]);
                    f.clear();
                    f.extend_from_slice(&t[i - pre..i - pre + n_win]);
                }
                self.triggered = true;
                self.last_trig = Instant::now();
                if self.mode == 2 {
                    self.armed = false;
                }
            }
            None => {
                if self.mode == 0 && self.last_trig.elapsed().as_secs_f32() > (20.0f32 * self.t_div).max(0.12) {
                    for c in 0..2 {
                        let (f, t) = (&mut self.frame[c], &self.tbuf[c]);
                        f.clear();
                        f.extend_from_slice(&t[m - n_win..]);
                    }
                    self.triggered = false;
                }
            }
        }
    }

    fn update_meas(&mut self, scale: [f32; 2]) {
        if self.meas_t.elapsed().as_secs_f32() < 0.3 {
            return;
        }
        self.meas_t = Instant::now();
        for c in 0..2 {
            let f = &self.frame[c];
            if f.len() < 4 {
                self.meas[c] = Meas::default();
                continue;
            }
            let (mut mx, mut mn, mut sum, mut sq) = (f32::MIN, f32::MAX, 0.0f64, 0.0f64);
            for &v in f {
                mx = mx.max(v);
                mn = mn.min(v);
                sum += v as f64;
                sq += (v as f64) * (v as f64);
            }
            let n = f.len() as f64;
            let mean = (sum / n) as f32;
            // frequency from interpolated rising crossings of the mean
            let (mut first, mut last, mut count) = (0.0f64, 0.0f64, 0usize);
            for i in 1..f.len() {
                if f[i - 1] < mean && f[i] >= mean {
                    let t = (i - 1) as f64 + ((mean - f[i - 1]) / (f[i] - f[i - 1]).max(1e-12)) as f64;
                    if count == 0 {
                        first = t;
                    }
                    last = t;
                    count += 1;
                }
            }
            let freq = if count >= 2 && last > first { ((count - 1) as f64 * self.frame_sr as f64 / (last - first)) as f32 } else { 0.0 };
            let s = scale[c];
            self.meas[c] = Meas { vmax: mx * s, vmin: mn * s, vrms: (sq / n).sqrt() as f32 * s, vmean: mean * s, freq };
        }
    }
}

impl App {
    fn fmt_units(&self, v: f32) -> String {
        if self.cal.on { fmt_volts(v) } else { format!("{v:.3} FS") }
    }

    pub fn panel_scope(&mut self, ui: &mut Ui) {
        let act = self.header(ui, 4, "Scope", ("Time", "Volt"), false);
        let sr = self.sr();
        let scale = [self.cal.scale(0), self.cal.scale(1)];
        {
            let s = &mut self.scope;
            if act.reset {
                s.t_div = 1e-3;
                s.v_div = 0.25 * scale[0];
                s.v_off = 0.0;
                s.pos = 0.5;
                s.level = 0.0;
            }
            if act.zx != 0.0 {
                s.t_div = fclamp(s.t_div * act.zx, ScopeState::min_t_div(sr), s.max_t_div_for(sr));
            }
            if act.zy != 0.0 {
                s.v_div = fclamp(s.v_div * act.zy, 1e-7, 1e4);
            }
            if act.px != 0.0 {
                s.pos = fclamp(s.pos + act.px, 0.02, 0.98);
            }
            if act.py != 0.0 {
                s.v_off += act.py * 2.0 * s.v_div;
            }
        }

        {
            let s = &mut self.scope;
            if s.mode != 3 {
                s.t_div = s.t_div.min(ScopeState::max_t_div(sr));
            }
        }
        // controls
        let cal_on = self.cal.on;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(5.0, 3.0);
            let s = &mut self.scope;
            let run_txt = if s.running { "Stop" } else { "Run" };
            if crate::mapping::btn(ui, run_txt).clicked() {
                s.running = !s.running;
            }
            ui.checkbox(&mut s.show[0], RichText::new("L").color(s.col[0]));
            ui.checkbox(&mut s.show[1], RichText::new("R").color(s.col[1]));
            ui.color_edit_button_srgba(&mut s.col[0]).on_hover_text(crate::lang::tr("Colour of the left trace"));
            ui.color_edit_button_srgba(&mut s.col[1]).on_hover_text(crate::lang::tr("Colour of the right trace"));
            ui.checkbox(&mut s.blend, crate::lang::tr("Blend")).on_hover_text(crate::lang::tr("The traces add up instead of one covering the other: where they overlap the two colours mix into a third one"));
            ui.label(RichText::new(crate::lang::tr("Trig")).small().weak());
            egui::ComboBox::from_id_salt("tsrc").width(36.0).selected_text(["L", "R"][s.trig_src]).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.trig_src, 0, "L");
                ui.selectable_value(&mut s.trig_src, 1, "R");
            });
            egui::ComboBox::from_id_salt("tmode").width(64.0).selected_text(["Auto", "Normal", "Single", "Roll"][s.mode as usize]).show_ui(ui, |ui| {
                for (i, n) in ["Auto", "Normal", "Single", "Roll"].iter().enumerate() {
                    ui.selectable_value(&mut s.mode, i as u8, crate::lang::tr(n));
                }
            });
            egui::ComboBox::from_id_salt("tedge").width(58.0).selected_text(if s.rising { "Rising" } else { "Falling" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.rising, true, crate::lang::tr("Rising"));
                ui.selectable_value(&mut s.rising, false, crate::lang::tr("Falling"));
            });
            if s.mode == 2 && crate::mapping::btn(ui, "Arm").clicked() {
                s.armed = true;
            }
            // The level spans full scale (+-1 FS, or +-volts when calibrated) whatever part of it is on screen.
            let fs = scale[s.trig_src].max(1e-9);
            s.level = s.level.clamp(-fs, fs);
            ui.add_enabled_ui(s.mode != 3, |ui| {
                ui.spacing_mut().slider_width = 130.0;
                ui.add(
                    egui::Slider::new(&mut s.level, -fs..=fs)
                        .text(crate::lang::tr("level"))
                        .custom_formatter(move |v, _| if cal_on { fmt_volts(v as f32) } else { format!("{v:+.3} FS") }),
                )
                .on_hover_text(crate::lang::tr("Trigger level over the full range, also above or below the visible window"));
            });
            let mut pct = s.pos * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(2.0..=98.0).suffix("%").prefix(crate::lang::tr("pos "))).changed() {
                s.pos = pct / 100.0;
            }
            ui.add(egui::DragValue::new(&mut s.hyst).range(0.0..=20.0).speed(0.05).suffix(crate::lang::tr("% hyst")));
            ui.checkbox(&mut s.hf_reject, crate::lang::tr("HF reject")).on_hover_text(crate::lang::tr("Low-pass filter on the trigger signal only. The displayed trace is not filtered."));
            ui.add_enabled(s.hf_reject, egui::DragValue::new(&mut s.hf_hz).range(100.0..=96000.0).speed(20.0).suffix(crate::lang::tr(" Hz")));
        });

        let sh = self.sh.clone();
        self.scope.acquire(&sh, scale);
        self.scope.update_meas(scale);

        // measurement line
        {
            let mut parts = Vec::new();
            for c in 0..2 {
                if !self.scope.show[c] {
                    continue;
                }
                let m = self.scope.meas[c];
                let f = if m.freq > 0.0 { fmt_hz(m.freq) } else { "-".into() };
                parts.push(RichText::new(format!(
                    "{}: Vpp {}  Vrms {}  max {}  min {}  mean {}  f {}",
                    ["L", "R"][c],
                    self.fmt_units(m.vmax - m.vmin),
                    self.fmt_units(m.vrms),
                    self.fmt_units(m.vmax),
                    self.fmt_units(m.vmin),
                    self.fmt_units(m.vmean),
                    f
                )).small().color(self.scope.col[c]));
            }
            ui.horizontal_wrapped(|ui| {
                for p in parts {
                    ui.label(p);
                }
            });
        }

        let size = ui.available_size();
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, BG);
        let plot = Rect::from_min_max(rect.min + vec2(52.0, 4.0), rect.max - vec2(8.0, 16.0));
        if plot.width() < 40.0 || plot.height() < 40.0 {
            return;
        }
        self.scope.plot_w = plot.width() as usize;
        let s = &self.scope;
        let n_win = s.n_win(sr);
        let visible = n_win as f32 / sr;        let t_per_div = visible / 10.0;
        let center_y = plot.center().y;
        let y_of = |v: f32| center_y - (v + s.v_off) / (8.0 * s.v_div) * plot.height();

        // interaction: click / drag sets the trigger level, wheel zooms
        let mut new_level = None;
        if s.mode != 3 && (resp.clicked() || resp.dragged_by(egui::PointerButton::Primary))
            && let Some(p) = resp.interact_pointer_pos() {
                new_level = Some((center_y - p.y) / plot.height() * 8.0 * s.v_div - s.v_off);
            }
        let mut wheel = 0.0;
        if resp.hovered() {
            wheel = ui.input(|i| i.smooth_scroll_delta.y);
        }
        let shift = ui.input(|i| i.modifiers.shift);

        // grid
        let clip = painter.with_clip_rect(plot);
        for i in 0..=10 {
            let x = plot.left() + plot.width() * i as f32 / 10.0;
            clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, if i == 5 { GRID_MAJOR } else { GRID }));
            if i % 2 == 0 {
                // roll mode: time before "now" at the right edge; otherwise time relative to the trigger point
                let t = if s.mode == 3 { (i as f32 / 10.0 - 1.0) * visible } else { (i as f32 / 10.0 - s.pos) * visible };
                painter.text(Pos2::new(x, plot.bottom() + 2.0), Align2::CENTER_TOP, fmt_time(t), FontId::proportional(9.0), TXT);
            }
        }
        for k in -4..=4 {
            let v = k as f32 * s.v_div - s.v_off;
            let y = center_y - k as f32 / 8.0 * plot.height();
            clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, if k == 0 { GRID_MAJOR } else { GRID }));
            if k % 2 == 0 {
                let txt = if self.cal.on { fmt_volts(v) } else { format!("{v:.2}") };
                painter.text(Pos2::new(plot.left() - 3.0, y), Align2::RIGHT_CENTER, txt, FontId::proportional(9.0), TXT);
            }
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, GRID_MAJOR), egui::StrokeKind::Inside);

        // traces
        let w = plot.width().floor().max(1.0) as usize;
        for c in 0..2 {
            let f = &s.frame[c];
            if !s.show[c] || f.len() < 2 {
                continue;
            }
            let sc = scale[c];
            let n = f.len();
            let spp = n as f32 / w as f32;
            // (blended: light that adds up, so the colours mix where the traces meet)
            let col = if s.blend { Color32::from_rgba_premultiplied(s.col[c].r(), s.col[c].g(), s.col[c].b(), 0) } else { s.col[c] };
            let mut pts: Vec<Pos2> = Vec::new();
            if spp <= 1.0 {
                pts.reserve(n);
                for (i, &v) in f.iter().enumerate() {
                    pts.push(Pos2::new(plot.left() + i as f32 / n_win as f32 * plot.width(), y_of(v * sc)));
                }
                if spp < 0.25 {
                    for p in &pts {
                        clip.circle_filled(*p, 2.0, col);
                    }
                }
            } else {
                for px in 0..w {
                    let a = (px as f32 * spp) as usize;
                    let b = (((px + 1) as f32 * spp) as usize).clamp(a + 1, n);
                    let (mut mn, mut mx) = (f32::MAX, f32::MIN);
                    for &v in &f[a.min(n - 1)..b] {
                        mn = mn.min(v);
                        mx = mx.max(v);
                    }
                    let x = plot.left() + px as f32 + 0.5;
                    let (y0, y1) = (y_of(mx * sc), y_of(mn * sc));
                    if px % 2 == 0 {
                        pts.push(Pos2::new(x, y0));
                        pts.push(Pos2::new(x, y1));
                    } else {
                        pts.push(Pos2::new(x, y1));
                        pts.push(Pos2::new(x, y0));
                    }
                }
            }
            clip.add(Shape::line(pts, Stroke::new(1.4, col)));
        }

        // trigger markers (none in roll mode)
        let tc = Color32::from_rgb(255, 210, 60);
        if s.mode != 3 {
            let ty = y_of(s.level);
            if ty >= plot.top() + 2.0 && ty <= plot.bottom() - 2.0 {
                clip.line_segment([Pos2::new(plot.left(), ty), Pos2::new(plot.right(), ty)], Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 210, 60, 110)));
                painter.add(Shape::convex_polygon(vec![Pos2::new(plot.right() - 10.0, ty), Pos2::new(plot.right(), ty - 5.0), Pos2::new(plot.right(), ty + 5.0)], tc, Stroke::NONE));
            } else {
                // level is above or below the visible window: arrow at the edge pointing towards it
                let up = ty < plot.top();
                let (y, dir) = if up { (plot.top() + 14.0, -1.0f32) } else { (plot.bottom() - 14.0, 1.0f32) };
                let x = plot.right() - 12.0;
                painter.add(Shape::convex_polygon(vec![Pos2::new(x - 6.0, y - dir * 3.0), Pos2::new(x + 6.0, y - dir * 3.0), Pos2::new(x, y + dir * 8.0)], tc, Stroke::NONE));
                let txt = if self.cal.on { fmt_volts(s.level) } else { format!("{:+.3} FS", s.level) };
                painter.text(Pos2::new(x - 10.0, y), Align2::RIGHT_CENTER, format!("trigger {txt}"), FontId::proportional(11.0), tc);
            }
            let tx = plot.left() + s.pos * plot.width();
            painter.add(Shape::convex_polygon(vec![Pos2::new(tx - 5.0, plot.top()), Pos2::new(tx + 5.0, plot.top()), Pos2::new(tx, plot.top() + 8.0)], tc, Stroke::NONE));
        }

        let status = if !s.running {
            "STOP"
        } else if s.mode == 3 {
            "ROLL"
        } else if s.mode == 2 && !s.armed {
            "SINGLE - done (press Arm)"
        } else if s.mode == 2 {
            "SINGLE - armed"
        } else if s.triggered && s.last_trig.elapsed().as_secs_f32() < (20.0f32 * s.t_div).max(0.2) {
            "TRIG'D"
        } else if s.mode == 0 {
            "AUTO (no trigger)"
        } else {
            "WAITING for trigger"
        };
        painter.text(plot.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, status, FontId::proportional(11.0), tc);
        let vd = if self.cal.on { fmt_volts(s.v_div) } else { format!("{:.3} FS", s.v_div) };
        painter.text(plot.right_top() + vec2(-6.0, 4.0), Align2::RIGHT_TOP, format!("{}/div   {}/div", fmt_time(t_per_div), vd), FontId::proportional(11.0), TXT);

        // apply deferred changes
        let sr_lo = ScopeState::min_t_div(sr);
        let sr_hi = ScopeState::max_t_div(sr);
        let s = &mut self.scope;
        if let Some(l) = new_level {
            let fs = scale[s.trig_src].max(1e-9);
            s.level = l.clamp(-fs, fs);
        }
        if wheel != 0.0 {
            let f = (-wheel * 0.004).exp();
            if shift {
                s.v_div = fclamp(s.v_div * f, 1e-7, 1e4);
            } else {
                s.t_div = fclamp(s.t_div * f, sr_lo, sr_hi);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole-buffer filter this was before the lazy one (the reference).
    fn lowpass_zero_phase(x: &[f32], fc: f32, sr: f32) -> Vec<f32> {
        let fc = fc.clamp(10.0, sr * 0.45) as f64;
        let w0 = std::f64::consts::TAU * fc / sr as f64;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 - c) / 2.0 / a0;
        let b1 = (1.0 - c) / a0;
        let a1 = -2.0 * c / a0;
        let a2 = (1.0 - alpha) / a0;
        let mut y: Vec<f32> = x.to_vec();
        for pass in 0..2 {
            let (mut z1, mut z2) = (0.0f64, 0.0f64);
            let mut run = |v: &mut f32| {
                let xi = *v as f64;
                let out = b0 * xi + z1;
                z1 = b1 * xi - a1 * out + z2;
                z2 = b0 * xi - a2 * out;
                *v = out as f32;
            };
            if pass == 0 {
                y.iter_mut().for_each(&mut run);
            } else {
                y.iter_mut().rev().for_each(&mut run);
            }
        }
        y
    }

    /// The trigger search as it was, on the whole filtered buffer.
    fn find_trigger_old(x: &[f32], lvl: f32, hy: f32, rising: bool, lo: usize, hi: usize) -> Option<usize> {
        let mut i = hi;
        while i > lo {
            let (a, b) = (x[i - 1], x[i]);
            let hit = if rising { a < lvl && b >= lvl && x[i - 12] < lvl - hy } else { a > lvl && b <= lvl && x[i - 12] > lvl + hy };
            if hit {
                return Some(i);
            }
            i -= 1;
        }
        None
    }

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|i| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let r = ((s >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0;
                0.6 * r + 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() + 0.2
            })
            .collect()
    }

    #[test]
    fn lazy_zero_phase_matches_the_whole_buffer_filter() {
        for (fc, n) in [(5000.0f32, 50_000usize), (100.0, 60_000), (10.0, 70_000), (20000.0, 30_000)] {
            let x = noise(n, 7 + n as u64);
            let want = lowpass_zero_phase(&x, fc, 48000.0);
            let mut z = ZeroPhase::new();
            z.reset(n, fc, 48000.0);
            // in steps from the end, as the search asks for it
            for need in [n - 100, n - 9000, n / 2, 3000, 0] {
                z.ensure(&x, need);
                assert!(z.done <= need);
                let err = (z.done..n).map(|i| (z.y[i] - want[i]).abs()).fold(0.0f32, f32::max);
                assert!(err < 2e-6, "fc {fc} need {need}: {err}");
            }
        }
    }

    #[test]
    fn lazy_trigger_search_finds_what_the_old_one_found() {
        let (n, sr) = (40_000usize, 48000.0);
        let x = noise(n, 99);
        for fc in [500.0f32, 5000.0] {
            let y = lowpass_zero_phase(&x, fc, sr);
            for (k, lvl) in [-0.3f32, 0.0, 0.2, 0.9, 5.0].into_iter().enumerate() {
                for rising in [true, false] {
                    let (lo, hi) = (4000 + k * 100, n - 3000);
                    let want = find_trigger_old(&y, lvl, 0.002, rising, lo, hi);
                    let mut z = ZeroPhase::new();
                    z.reset(n, fc, sr);
                    let got = find_trigger(&x, Some(&mut z), lvl, 0.002, rising, lo, hi);
                    assert_eq!(got, want, "fc {fc} level {lvl} rising {rising}");
                    if fc == 5000.0 && lvl == 0.0 {
                        assert!(want.is_some() && z.done > n - 20_000, "only the end of the buffer is filtered: {}", z.done);
                    }
                    // unfiltered
                    assert_eq!(find_trigger(&x, None, lvl, 0.002, rising, lo, hi), find_trigger_old(&x, lvl, 0.002, rising, lo, hi));
                }
            }
        }
    }

    #[test]
    fn a_snapshot_of_the_ring_equals_copy_last() {
        let ring = parking_lot::Mutex::new(Ring::new());
        let mut s = 5u64;
        let mut next = move || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as usize
        };
        let (mut l, mut r) = (vec![0f32; 70_000], vec![0f32; 70_000]);
        let (mut el, mut er) = (vec![0f32; 70_000], vec![0f32; 70_000]);
        let mut have = u64::MAX;
        for round in 0..40 {
            // sometimes no new samples (the old picture must be kept), sometimes a lot (more than a lap of the ring)
            let add = match round % 5 {
                0 => 0,
                1 => 3,
                2 => 20_000,
                3 => 1_500_000,
                _ => next() % 90_000,
            };
            {
                let mut g = ring.lock();
                for _ in 0..add {
                    let v = (next() % 10_000) as f32;
                    g.push(v, -v);
                }
            }
            let t = snapshot_last(&ring, &mut l, &mut r, have).unwrap();
            have = t;
            ring.lock().copy_last(&mut el, &mut er);
            assert_eq!(l, el, "round {round}");
            assert_eq!(r, er, "round {round}");
        }
        // a window that is longer than what has been recorded is zero padded at the front
        let ring2 = parking_lot::Mutex::new(Ring::new());
        for i in 0..1000 {
            ring2.lock().push(i as f32 + 1.0, 0.0);
        }
        let (mut a, mut b) = (vec![9f32; 3000], vec![9f32; 3000]);
        snapshot_last(&ring2, &mut a, &mut b, u64::MAX).unwrap();
        assert!(a[..2000].iter().all(|&v| v == 0.0) && a[2000] == 1.0 && a[2999] == 1000.0);
    }

    #[test]
    #[ignore]
    fn bench_trigger_hf_reject() {
        let (n, sr) = (2 * 262_144usize, 48000.0);
        let x = noise(n, 3);
        let t = Instant::now();
        let mut found = 0;
        for _ in 0..20 {
            let y = lowpass_zero_phase(&x, 5000.0, sr);
            found += find_trigger_old(&y, 0.1, 0.002, true, 131_072, n - 131_072).unwrap_or(0);
        }
        eprintln!("old: {:?} per frame ({found})", t.elapsed() / 20);
        let t = Instant::now();
        let mut z = ZeroPhase::new();
        for _ in 0..20 {
            z.reset(n, 5000.0, sr);
            found += find_trigger(&x, Some(&mut z), 0.1, 0.002, true, 131_072, n - 131_072).unwrap_or(0);
        }
        eprintln!("new: {:?} per frame ({found})", t.elapsed() / 20);
        // the copy out of the ring: whole lock vs pieces (the time is similar, the lock hold time is what changes)
        let ring = parking_lot::Mutex::new(Ring::new());
        {
            let mut g = ring.lock();
            for i in 0..RING_CAP {
                g.push(i as f32, 0.0);
            }
        }
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        let t = Instant::now();
        for _ in 0..20 {
            ring.lock().copy_last(&mut l, &mut r);
        }
        eprintln!("copy_last whole: {:?}", t.elapsed() / 20);
        let t = Instant::now();
        for _ in 0..20 {
            snapshot_last(&ring, &mut l, &mut r, u64::MAX);
        }
        eprintln!("snapshot_last pieces: {:?}", t.elapsed() / 20);
        let t = Instant::now();
        for _ in 0..20 {
            snapshot_last(&ring, &mut l, &mut r, RING_CAP as u64);
        }
        eprintln!("snapshot_last unchanged: {:?}", t.elapsed() / 20);
    }

    #[test]
    fn a_roll_of_minutes_comes_from_the_history() {
        let sh = Shared::new();
        sh.sr.store(48000, Relaxed);
        let mut s = ScopeState::new();
        s.mode = 3;
        s.t_div = 3.0;
        s.plot_w = 600;
        // 20 s of a 100 Hz tone that is loud in the first ten seconds and quiet after, fed in pieces like the audio thread does
        let mut t = 0usize;
        while t < 48000 * 20 {
            {
                let mut r = sh.ring.lock();
                for i in 0..4800 {
                    let a = if t + i < 48000 * 10 { 0.8 } else { 0.1 };
                    let v = a * (2.0 * std::f32::consts::PI * 100.0 * (t + i) as f32 / 48000.0).sin();
                    r.push(v, -v);
                }
            }
            t += 4800;
            s.roll_feed(&sh, 48000.0);
        }
        assert!(s.long_roll(48000.0), "30 s is longer than the ring of samples");
        s.build_long(48000.0);
        assert_eq!(s.frame[0].len(), 1200);
        // the window is 30 s: it ends now, the last 20 s are the tone (10 s loud, 10 s quiet), before that nothing
        let peak_at = |c: usize, px: usize| s.frame[c][2 * px + 1];
        assert!(peak_at(0, 5) < 0.001, "before the start: {}", peak_at(0, 5));
        assert!(peak_at(0, 250) > 0.7, "the loud part: {}", peak_at(0, 250));
        assert!(peak_at(0, 550) < 0.15 && peak_at(0, 550) > 0.05, "the quiet part: {}", peak_at(0, 550));
        // the right channel is the negated one
        let lowest = s.frame[1][2 * 250];
        assert!(lowest < -0.7, "{lowest}");
        // the longest roll is 60 s per division
        assert!(s.max_t_div_for(48000.0) >= 59.0);
        s.mode = 0;
        assert!(s.max_t_div_for(48000.0) < 5.0);
    }

    #[test]
    fn hf_reject_filters_noise_not_signal() {
        let sr = 48000.0f32;
        let n = 20000;
        let tone = |f: f32, i: usize| (std::f32::consts::TAU * f * i as f32 / sr).sin();
        let x_lo: Vec<f32> = (0..n).map(|i| tone(100.0, i)).collect();
        let x_hi: Vec<f32> = (0..n).map(|i| tone(20000.0, i)).collect();
        let (yl, yh) = (lowpass_zero_phase(&x_lo, 2000.0, sr), lowpass_zero_phase(&x_hi, 2000.0, sr));
        let rms = |v: &[f32]| (v[4000..16000].iter().map(|a| a * a).sum::<f32>() / 12000.0).sqrt();
        let pass_db = 20.0 * (rms(&yl) / rms(&x_lo)).log10();
        let stop_db = 20.0 * (rms(&yh) / rms(&x_hi)).log10();
        // zero phase: output tracks input sample for sample
        let err = (4000..16000).map(|i| (yl[i] - x_lo[i]).abs()).fold(0.0f32, f32::max);
        eprintln!("pass {pass_db:.3} dB, stop {stop_db:.1} dB, max phase error {err:.4}");
        assert!(pass_db.abs() < 0.1);
        assert!(stop_db < -40.0);
        assert!(err < 0.02);
    }
}