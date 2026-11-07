//! Impedance measurement: a log sweep through a known series resistor into the device under test (a speaker, a headphone, a
//! microphone capsule, a filter ...). The voltage divider gives the impedance at every frequency:
//!
//! ```text
//!   generator out ---[ R ref ]---+---- device under test ---- ground
//!                                |
//!                      input(s) measure here
//! ```
//!
//! Two channels: R input on the generator side of the resistor (the reference, it cancels the sound card), L input across the
//! device. One channel: only L across the device, with the voltages of the output and the input known (calibration). A known
//! resistor in place of the device calibrates the level.

use super::plot::{canvas_h, inset, Plot};
use super::sweep::{State, SweepResult};
use super::traces::Library;
use super::ts::{self, FreeAir, VasMethod};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const SMOOTH: [(f32, &str); 4] = [(0.0, "off"), (12.0, "1/12 oct"), (6.0, "1/6 oct"), (3.0, "1/3 oct")];
const LOG_N: usize = 900;

/// The result of one measurement.
#[derive(Clone)]
pub struct ZCurve {
    pub f: Vec<f32>,
    /// |Z| in dB re 1 ohm, unsmoothed
    pub db: Vec<f32>,
    pub phase_deg: Vec<f32>,
}

impl ZCurve {
    pub fn ohm(&self, i: usize) -> f32 {
        10f32.powf(self.db[i] / 20.0)
    }
}

/// How the sweep result becomes an impedance.
#[derive(Clone, Copy)]
pub struct Setup {
    /// resistor in series with the device (ohm), and the output impedance of the interface in front of it
    pub r_ref: f32,
    pub r_out: f32,
    /// a factor on the measured voltage ratio (calibration: the volts of the output and the input, or a known resistor)
    pub gain: f32,
}

/// What was measured on one DATS V3 (USB AUDIO  CODEC) at 44.1 kHz: with the terminals open the L input reads 0.9989 of the R input
/// (the gain of the two inputs differs by 0.1 %), and with its built-in 1 kΩ resistor L / R is 0.9129 at all frequencies from 100 Hz
/// to 10 kHz, which makes the series resistor 94.2 Ω. Other units differ by their tolerances: the calibration finds their own.
pub const DATS_RS: f32 = 94.2;
pub const DATS_GAIN: f32 = 1.0 / 0.9989;

/// The impedance of the device from the transfer function `H = V_device / V_source` (complex): `Z = Rs * H / (1 - H)`.
pub fn z_from_h(h: (f32, f32), rs: f32) -> (f32, f32) {
    let (a, b) = (1.0 - h.0, -h.1);
    let den = (a * a + b * b).max(1e-18);
    // h / (1 - h)
    let (re, im) = ((h.0 * a + h.1 * b) / den, (h.1 * a - h.0 * b) / den);
    (rs * re, rs * im)
}

/// What a short circuit of the leads measures: their resistance and inductance as complex values over frequency. It is taken off every
/// measurement afterwards (the series impedance of the leads and the terminals).
#[derive(Clone, Debug, PartialEq)]
pub struct ZShort {
    pub f: Vec<f32>,
    pub re: Vec<f32>,
    pub im: Vec<f32>,
}

impl ZShort {
    /// The value at `f` Hz (straight on a log axis, the end values beyond the ends).
    pub fn at(&self, f: f32) -> (f32, f32) {
        if self.f.len() < 2 {
            return (0.0, 0.0);
        }
        if f <= self.f[0] {
            return (self.re[0], self.im[0]);
        }
        let n = self.f.len();
        if f >= self.f[n - 1] {
            return (self.re[n - 1], self.im[n - 1]);
        }
        let j = self.f.partition_point(|&x| x < f).clamp(1, n - 1);
        let t = (f.ln() - self.f[j - 1].ln()) / (self.f[j].ln() - self.f[j - 1].ln()).max(1e-9);
        (self.re[j - 1] + t * (self.re[j] - self.re[j - 1]), self.im[j - 1] + t * (self.im[j] - self.im[j - 1]))
    }

    /// Fewer points, for the settings file.
    fn thinned(&self, n: usize) -> ZShort {
        let idx: Vec<usize> = (0..n).map(|k| k * (self.f.len() - 1) / (n - 1)).collect();
        ZShort { f: idx.iter().map(|&i| self.f[i]).collect(), re: idx.iter().map(|&i| self.re[i]).collect(), im: idx.iter().map(|&i| self.im[i]).collect() }
    }

    fn encode(&self) -> String {
        let t = self.thinned(60.min(self.f.len()).max(2));
        (0..t.f.len()).map(|i| format!("{:.2}:{:.5}:{:.5}", t.f[i], t.re[i], t.im[i])).collect::<Vec<_>>().join(",")
    }

    fn decode(s: &str) -> Option<ZShort> {
        let mut z = ZShort { f: Vec::new(), re: Vec::new(), im: Vec::new() };
        for p in s.split(',') {
            let v: Vec<f32> = p.split(':').filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 && v.iter().all(|x| x.is_finite()) && v[0] > 0.0 {
                z.f.push(v[0]);
                z.re.push(v[1]);
                z.im.push(v[2]);
            }
        }
        (z.f.len() >= 2 && z.f.windows(2).all(|w| w[1] > w[0])).then_some(z)
    }
}

/// The impedance of every point of a log grid, as complex numbers (before any short compensation).
pub fn z_points(r: &SweepResult, s: Setup) -> (Vec<f32>, Vec<(f32, f32)>) {
    let h = &r.h;
    let nyq = r.sr * 0.5;
    let f0 = r.params.f0.max(h.df * 2.0).max(1.0);
    let f1 = nyq.min(r.params.f1).max(f0 * 2.0);
    let at = |data: &[f32], f: f32| -> f32 {
        let b = f / h.df;
        let k = (b.floor() as usize).min(data.len().saturating_sub(2));
        let t = b - k as f32;
        data[k] * (1.0 - t) + data[k + 1] * t
    };
    let f: Vec<f32> = (0..LOG_N).map(|i| f0 * (f1 / f0).powf(i as f32 / (LOG_N - 1) as f32)).collect();
    let rs = (s.r_ref + s.r_out).max(1e-6);
    let z = f
        .iter()
        .map(|&x| {
            let mag = 10f32.powf(at(&h.mag_db, x) / 20.0) * s.gain;
            let phase = at(&h.phase, x);
            z_from_h((mag * phase.cos(), mag * phase.sin()), rs)
        })
        .collect();
    (f, z)
}

/// The curve from a sweep result, with the short circuit of the leads taken off when one was measured.
pub fn curve_from_sweep(r: &SweepResult, s: Setup, short: Option<&ZShort>) -> ZCurve {
    let (f, z) = z_points(r, s);
    let (mut db, mut ph) = (Vec::with_capacity(f.len()), Vec::with_capacity(f.len()));
    for (x, z) in f.iter().zip(z) {
        let sz = short.map_or((0.0, 0.0), |sh| sh.at(*x));
        let (re, im) = (z.0 - sz.0, z.1 - sz.1);
        db.push(20.0 * (re * re + im * im).sqrt().max(1e-9).log10());
        ph.push(im.atan2(re).to_degrees());
    }
    ZCurve { f, db, phase_deg: ph }
}

/// The voltage ratio of a measurement (full scale units) as one number: the median between 300 Hz and 3 kHz.
pub fn median_ratio(r: &SweepResult) -> Option<f32> {
    let h = &r.h;
    let mut v: Vec<f32> = (0..h.mag_db.len()).filter(|&k| (300.0..=3000.0).contains(&(k as f32 * h.df))).map(|k| 10f32.powf(h.mag_db[k] / 20.0)).collect();
    if v.len() < 3 {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let med = v[v.len() / 2];
    (med > 1e-9).then_some(med)
}

/// The gain that makes the measured level read `known` ohms (a known resistor in place of the device), from the median of the voltage
/// ratio between 300 Hz and 3 kHz: `1.0` when the measurement is already right.
pub fn calibration_gain(r: &SweepResult, r_ref_total: f32, known: f32) -> Option<f32> {
    let target = known / (r_ref_total + known);
    median_ratio(r).map(|m| target / m)
}

/// Calibration without knowing the volts or the series resistor: a measurement with the terminals open (the voltage ratio is 1 there,
/// so it gives the gain) and one with a known resistor (that gives the series resistance). Returns (gain, series resistance).
pub fn two_point_calibration(open: f32, with_r: f32, known: f32) -> Option<(f32, f32)> {
    if open < 1e-9 || with_r < 1e-9 {
        return None;
    }
    let gain = 1.0 / open;
    let h1 = gain * with_r;
    // H = known / (rs + known)
    (h1 > 1e-6 && h1 < 0.99999).then(|| (gain, known * (1.0 / h1 - 1.0)))
}

fn cal_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("impedance_cal.txt")
}

fn fmt_ohm(z: f32) -> String {
    if z >= 1000.0 {
        format!("{:.2} kΩ", z / 1000.0)
    } else if z >= 100.0 {
        format!("{z:.0} Ω")
    } else if z >= 10.0 {
        format!("{z:.1} Ω")
    } else {
        format!("{z:.2} Ω")
    }
}

pub struct Impedance {
    /// 0: two channels (R = generator side, L = across the device), 1: one channel (L), calibrated
    mode: usize,
    r_ref: f32,
    r_out: f32,
    out_v: f32,
    in_v: f32,
    /// a calibration with a known resistor (replaces the volts)
    cal_gain: Option<f32>,
    /// the series resistance found by the two point calibration (replaces the resistor field), and the measurements it comes from
    cal_rs: Option<f32>,
    cal_open: Option<f32>,
    cal_pt: Option<f32>,
    /// the input channel of the device under test with the DATS V3 (0 = L)
    dats_ch: usize,
    dats_seen: bool,
    /// the driver measured and the conditions, for the report
    name: String,
    cond: String,
    /// the short circuit of the leads, taken off every measurement
    short: Option<ZShort>,
    // Thiele-Small parameters: the piston diameter (mm), the method for Vas and its numbers
    d_mm: f32,
    method: usize,
    box_l: f32,
    add_g: f32,
    spl: f32,
    mmd_g: f32,
    /// what the next result is for: 0 nothing, 1 the free-air measurement, 2 the measurement with the box or the mass
    intent: u8,
    free: Option<FreeAir>,
    fs2: Option<f32>,
    ts_note: String,
    cal_r: f32,
    smooth: usize,
    show_phase: bool,
    auto: bool,
    y_lo: f32,
    y_hi: f32,
    curve: Option<ZCurve>,
    /// the sweep result this window asked for
    armed: bool,
    start_id: u64,
    seen_id: u64,
    note: String,
    save: Option<dialogs::Pending>,
    lib: Library,
}

impl Impedance {
    pub fn new() -> Self {
        let mut lib = Library::default();
        lib.file = "impedance.txt".into();
        lib.load_auto();
        // test hook (screenshots): VISUSCOPE_IMP_DEMO=1 shows a modelled loudspeaker and two stored variants
        let mut demo = None;
        if std::env::var_os("VISUSCOPE_IMP_DEMO").is_some() {
            let model = |f0: f32, re: f32| -> ZCurve {
                let f: Vec<f32> = (0..LOG_N).map(|i| 20.0 * 1000f32.powf(i as f32 / (LOG_N - 1) as f32)).collect();
                let (mut db, mut ph) = (Vec::new(), Vec::new());
                for &x in &f {
                    let w = 2.0 * std::f32::consts::PI * x;
                    let w0 = 2.0 * std::f32::consts::PI * f0;
                    let (rr, qm) = (28.0f32, 3.0f32);
                    let c = qm / (rr * w0);
                    let l = 1.0 / (w0 * w0 * c);
                    // the mechanical branch (a parallel resonance), the voice coil
                    let y = (1.0 / rr, w * c - 1.0 / (w * l));
                    let d = y.0 * y.0 + y.1 * y.1;
                    let zm = (y.0 / d, -y.1 / d);
                    let z = (re + zm.0, w * 0.0004 + zm.1);
                    db.push(20.0 * (z.0 * z.0 + z.1 * z.1).sqrt().log10());
                    ph.push(z.1.atan2(z.0).to_degrees());
                }
                ZCurve { f, db, phase_deg: ph }
            };
            let c = model(62.0, 6.2);
            for (name, f0, re) in [("Woofer, closed box", 78.0, 6.2), ("Woofer, open", 48.0, 6.2)] {
                let m = model(f0, re);
                lib.add(Some(name.into()), m.f.clone(), m.db.clone(), m.phase_deg.clone());
            }
            lib.dirty = false;
            demo = Some(c);
        }
        // test hook (screenshots): VISUSCOPE_IMP_CSV=file loads a curve ("freq_hz,db,phase_deg" lines)
        if let Some(t) = std::env::var_os("VISUSCOPE_IMP_CSV").and_then(|p| std::fs::read_to_string(p).ok()) {
            let (mut f, mut db, mut ph) = (Vec::new(), Vec::new(), Vec::new());
            for l in t.lines().skip(1) {
                let v: Vec<f32> = l.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() == 3 {
                    f.push(v[0]);
                    db.push(v[1]);
                    ph.push(v[2]);
                }
            }
            if f.len() > 10 {
                demo = Some(ZCurve { f, db, phase_deg: ph });
            }
        }
        let mut me = Self { curve: demo, mode: 0, r_ref: 100.0, r_out: 0.0, out_v: 1.0, in_v: 1.0, cal_gain: None, cal_rs: None, cal_open: None, cal_pt: None, dats_ch: 0, dats_seen: false, name: String::new(), cond: String::new(), short: None, d_mm: 76.2, method: 0, box_l: 10.0, add_g: 10.0, spl: 85.0, mmd_g: 10.0, intent: 0, free: None, fs2: None, ts_note: String::new(), cal_r: 100.0, smooth: 1, show_phase: true, auto: true, y_lo: -10.0, y_hi: 50.0, armed: false, start_id: 0, seen_id: 0, note: String::new(), save: None, lib };
        me.load_cal();
        me
    }

    fn setup(&self) -> Setup {
        // the voltage ratio of the sweep is in full scale units: the volts of the output and of the input turn it into volts per volt
        let gain = self.cal_gain.unwrap_or(if self.mode == 1 { self.in_v / self.out_v.max(1e-9) } else { 1.0 });
        match self.cal_rs {
            // (the two point calibration found the whole series resistance, the interface's output impedance included)
            Some(rs) => Setup { r_ref: rs, r_out: 0.0, gain },
            // the DATS V3 as it was measured, until its own calibration is made
            None if self.mode == 2 => Setup { r_ref: DATS_RS, r_out: 0.0, gain: DATS_GAIN },
            None => Setup { r_ref: self.r_ref, r_out: self.r_out, gain },
        }
    }

    /// Start a sweep (the DATS and the two channel set-up have the reference on R).
    fn start_measure(&mut self, cx: &mut Ctx) {
        cx.sweep.p.loopback = self.mode != 1;
        self.start_id = cx.sweep.result.as_ref().map_or(0, |r| r.id);
        self.armed = true;
        self.note.clear();
        cx.sweep.start();
    }

    /// The impedance of the shown curve as the free-air measurement, or (`second`) the one with the box or the mass.
    fn take_ts(&mut self, second: bool) {
        let Some(c) = &self.curve else { return };
        let ohm: Vec<f32> = (0..c.f.len()).map(|i| c.ohm(i)).collect();
        match ts::free_air(&c.f, &ohm, &c.phase_deg) {
            Ok(a) => {
                if second {
                    self.fs2 = Some(a.fs);
                    self.ts_note = format!("Resonance of the second measurement: {:.1} Hz", a.fs);
                } else {
                    self.free = Some(a);
                    self.fs2 = None;
                    self.ts_note.clear();
                }
            }
            Err(e) => self.ts_note = e,
        }
    }

    fn vas_method(&self) -> VasMethod {
        match self.method {
            0 => VasMethod::TestBox(self.box_l),
            1 => VasMethod::AddedMass(self.add_g),
            2 => VasMethod::Spl(self.spl),
            _ => VasMethod::Mmd(self.mmd_g),
        }
    }

    /// The parameters as lines of text (for the clipboard and the report).
    fn ts_text(&self) -> Option<String> {
        let a = self.free?;
        let mut s = if self.name.trim().is_empty() { String::new() } else { format!("Driver: {}\n", self.name.trim()) };
        if !self.cond.trim().is_empty() {
            s += &format!("Conditions: {}\n", self.cond.trim());
        }
        s += &format!("Re = {:.3} ohm\nFs = {:.2} Hz\nQms = {:.3}\nQes = {:.3}\nQts = {:.3}\nLe (1 kHz) = {:.3} mH\nLe (10 kHz) = {:.3} mH\nZmax = {:.2} ohm\n", a.re, a.fs, a.qms, a.qes, a.qts, a.le_1k * 1e3, a.le_10k * 1e3, a.zmax);
        if let Some(rt) = self.short.as_ref().map(|sh| sh.at(100.0).0) {
            s += &format!("Rt (test leads) = {rt:.3} ohm\n");
        }
        if let Ok(m) = ts::mechanics(&a, self.d_mm * 1e-3, self.vas_method(), self.fs2) {
            s += &format!("Vas = {:.2} l ({:.3} cu ft)\nMms = {:.2} g\nCms = {:.3} mm/N\nBl = {:.2} T m\nRms = {:.3} kg/s\nEta0 = {:.3} %\nSPL (1 W / 1 m) = {:.1} dB\nSd = {:.1} cm2\n", m.vas * 1e3, m.vas * 35.3147, m.mms * 1e3, m.cms * 1e3, m.bl, m.rms, m.eta0 * 100.0, m.spl, ts::piston_area(self.d_mm * 1e-3) * 1e4);
        }
        Some(s)
    }

    /// The two point calibration from the open and the 1 kΩ measurement (when both are there), then saved.
    fn apply_cal(&mut self) {
        self.cal_rs = None;
        self.cal_gain = None;
        if let (Some(o), Some(p)) = (self.cal_open, self.cal_pt) {
            if let Some((g, rs)) = two_point_calibration(o, p, 1000.0) {
                self.cal_gain = Some(g);
                self.cal_rs = Some(rs);
            }
        }
        self.save_cal();
    }

    /// The calibration lives in a file of its own, written when it changes: it survives restarts whatever happens to the settings.
    fn save_cal(&self) {
        if cfg!(test) || crate::prof::test_mode() {
            return;
        }
        let mut t = String::new();
        for (k, v) in [("open", self.cal_open), ("pt", self.cal_pt), ("rs", self.cal_rs), ("gain", self.cal_gain)] {
            if let Some(v) = v {
                t += &format!("{k}={v}\n");
            }
        }
        if let Some(sh) = &self.short {
            t += &format!("short={}\n", sh.encode());
        }
        let path = cal_path();
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(path, t);
    }

    fn load_cal(&mut self) {
        let Ok(t) = std::fs::read_to_string(cal_path()) else { return };
        for l in t.lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            let f = v.trim().parse::<f32>().ok().filter(|x| x.is_finite() && *x > 0.0 && *x < 1e7);
            match k {
                "open" => self.cal_open = f,
                "pt" => self.cal_pt = f,
                "rs" => self.cal_rs = f,
                "gain" => self.cal_gain = f,
                "short" => self.short = ZShort::decode(v.trim()),
                _ => {}
            }
        }
    }

    fn rebuild(&mut self, cx: &Ctx) {
        if let Some(r) = &cx.sweep.result {
            self.curve = Some(curve_from_sweep(r, self.setup(), self.short.as_ref()));
        }
    }
}

impl Tool for Impedance {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        // test hook (screenshots): VISUSCOPE_IMP_TS=1 takes the loaded curve as the free-air measurement (with a mass of 10 g and a second resonance from VISUSCOPE_IMP_FS2)
        if self.free.is_none() && self.curve.is_some() && std::env::var_os("VISUSCOPE_IMP_TS").is_some() {
            self.take_ts(false);
            if let Some(f) = std::env::var("VISUSCOPE_IMP_FS2").ok().and_then(|v| v.parse::<f32>().ok()) {
                self.method = 1;
                self.fs2 = Some(f);
            }
        }
        // a DATS V3 was plugged in: the window switches to its set-up
        if std::mem::take(cx.dats_new) && !self.dats_seen {
            self.dats_seen = true;
            self.mode = 2;
            self.cal_r = 1000.0;
        }
        // a result of the sweep that was started here
        let id = cx.sweep.result.as_ref().map_or(0, |r| r.id);
        if self.armed && id != self.start_id && !cx.sweep.busy() {
            self.armed = false;
            self.seen_id = id;
            self.rebuild(cx);
            self.note = "Measured".into();
            // a measurement for the driver parameters
            match std::mem::take(&mut self.intent) {
                1 => self.take_ts(false),
                2 => self.take_ts(true),
                3 => {
                    self.cal_open = cx.sweep.result.as_ref().and_then(median_ratio);
                    self.apply_cal();
                    self.note = "Open kept".into();
                }
                4 => {
                    self.cal_r = 1000.0;
                    self.cal_pt = cx.sweep.result.as_ref().and_then(median_ratio);
                    self.apply_cal();
                    self.note = "1 kΩ kept".into();
                }
                5 => {
                    if let Some(r) = cx.sweep.result.as_ref() {
                        // (what the short reads with the calibration as it is now and nothing taken off)
                        let (f, z) = z_points(r, self.setup());
                        self.short = Some(ZShort { f, re: z.iter().map(|v| v.0).collect(), im: z.iter().map(|v| v.1).collect() });
                        self.save_cal();
                        self.rebuild(cx);
                        self.note = "Leads kept".into();
                    }
                }
                _ => {}
            }
        }
        if let Some(d) = &self.save {
            if let Some(res) = d.take() {
                self.save = None;
                if let (Some(path), Some(c)) = (res, &self.curve) {
                    let mut s = String::from("freq_hz,impedance_ohm,phase_deg,resistance_ohm,reactance_ohm\n");
                    for i in 0..c.f.len() {
                        let (z, p) = (c.ohm(i), c.phase_deg[i].to_radians());
                        s += &format!("{:.3},{:.4},{:.2},{:.4},{:.4}\n", c.f[i], z, c.phase_deg[i], z * p.cos(), z * p.sin());
                    }
                    cx.req.note = Some(match std::fs::write(&path, s) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
        if let Some(n) = self.lib.poll() {
            cx.req.note = Some(n);
        }
        self.lib.save_auto();
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        // (the window scrolls; the plot gets a height of its own)
        let h = (ui.available_height() * 0.6).clamp(240.0, 560.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.body(ui, cx, h));
    }

    fn report(&self, _cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        let Some(c) = &self.curve else { return Vec::new() };
        let ppo = LOG_N as f32 / (c.f[c.f.len() - 1] / c.f[0]).log2();
        let db = super::response::smooth_log(&c.db, ppo, SMOOTH[self.smooth].0);
        let x = (c.f[0], *c.f.last().unwrap());
        let mag = vec![Series { name: "impedance".into(), color: COLORS[0], pts: c.f.iter().copied().zip(db.iter().copied()).collect() }];
        let y = auto_range(&mag, 3.0, 6.0);
        let ph = vec![Series { name: String::new(), color: COLORS[1], pts: c.f.iter().copied().zip(c.phase_deg.iter().copied()).collect() }];
        vec![Section {
            id: "Impedance",
            title: "Impedance".into(),
            blocks: vec![
                Block::Text(format!("Series resistor {} (+ {} output impedance). Magnitude in dB re 1 ohm: 0 dB is 1 ohm, 20 dB is 10 ohm.", fmt_ohm(self.r_ref), fmt_ohm(self.r_out))),
                Block::Plot(PlotSpec { title: "Impedance magnitude".into(), xlog: true, xlabel: "Hz".into(), ylabel: "dB re 1 ohm".into(), x, y, series: mag }),
                Block::Plot(PlotSpec { title: "Impedance phase".into(), xlog: true, xlabel: "Hz".into(), ylabel: "degrees".into(), x, y: (-90.0, 90.0), series: ph }),
            ]
            .into_iter()
            .chain(self.ts_text().map(|t| Block::Text(format!("Thiele-Small parameters\n{t}"))))
            .collect(),
        }]
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("mode", self.mode);
        o.put("r_ref", self.r_ref);
        o.put("r_out", self.r_out);
        o.put("out_v", self.out_v);
        o.put("in_v", self.in_v);
        o.put("cal_r", self.cal_r);
        o.put("dats_ch", self.dats_ch);
        o.put("d_mm", self.d_mm);
        o.put("name", self.name.chars().filter(|c| !c.is_control()).collect::<String>());
        o.put("cond", self.cond.chars().filter(|c| !c.is_control()).collect::<String>());
        o.put("method", self.method);
        o.put("box_l", self.box_l);
        o.put("add_g", self.add_g);
        o.put("spl", self.spl);
        o.put("mmd_g", self.mmd_g);
        if let Some(sh) = &self.short {
            o.put("short", sh.encode());
        }
        if let Some(v) = self.cal_rs {
            o.put("cal_rs", v);
        }
        if let Some(v) = self.cal_open {
            o.put("cal_open", v);
        }
        if let Some(v) = self.cal_pt {
            o.put("cal_pt", v);
        }
        if let Some(g) = self.cal_gain {
            o.put("cal_gain", g);
        }
        o.put("smooth", self.smooth);
        o.put("phase", self.show_phase);
        o.put("auto", self.auto);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("mode") {
            self.mode = v.min(2);
        }
        for (k, dst, lo, hi) in [("r_ref", &mut self.r_ref, 0.1, 100_000.0), ("r_out", &mut self.r_out, 0.0, 1000.0), ("out_v", &mut self.out_v, 0.001, 100.0), ("in_v", &mut self.in_v, 0.001, 100.0), ("cal_r", &mut self.cal_r, 0.1, 100_000.0)] {
            if let Some(v) = i.get::<f32>(k) {
                *dst = v.clamp(lo, hi);
            }
        }
        self.dats_ch = i.get::<usize>("dats_ch").unwrap_or(0).min(1);
        for (k, dst, lo, hi) in [("d_mm", &mut self.d_mm, 10.0, 800.0), ("box_l", &mut self.box_l, 0.1, 1000.0), ("add_g", &mut self.add_g, 0.1, 1000.0), ("spl", &mut self.spl, 50.0, 130.0), ("mmd_g", &mut self.mmd_g, 0.1, 2000.0)] {
            if let Some(v) = i.get::<f32>(k) {
                *dst = v.clamp(lo, hi);
            }
        }
        self.name = i.str("name").unwrap_or_default();
        self.cond = i.str("cond").unwrap_or_default();
        self.method = i.get::<usize>("method").unwrap_or(0).min(3);
        if let Some(v) = i.str("short").and_then(|v| ZShort::decode(&v)) {
            self.short = Some(v);
        }
        for (k, dst) in [("cal_rs", &mut self.cal_rs), ("cal_open", &mut self.cal_open), ("cal_pt", &mut self.cal_pt), ("cal_gain", &mut self.cal_gain)] {
            if let Some(v) = i.get::<f32>(k).filter(|g| *g > 0.0 && *g < 1e7) {
                *dst = Some(v);
            }
        }
        if let Some(v) = i.get::<usize>("smooth") {
            self.smooth = v.min(SMOOTH.len() - 1);
        }
        i.set("phase", &mut self.show_phase);
        i.set("auto", &mut self.auto);
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        self.y_hi = self.y_hi.clamp(-50.0, 120.0);
        self.y_lo = self.y_lo.clamp(-60.0, self.y_hi - 5.0);
    }
}

impl Impedance {
    fn body(&mut self, ui: &mut Ui, cx: &mut Ctx, plot_h: f32) {
        let busy = cx.sweep.busy();
        ui.horizontal_wrapped(|ui| {
            let b = egui::Button::new(RichText::new(if busy { "Stop" } else { "Measure" }).strong()).min_size(egui::vec2(96.0, 26.0));
            if ui.add(b).clicked() {
                if busy {
                    cx.sweep.cancel();
                } else {
                    // (the DATS V3 has the generator side of its series resistor on R and the terminals on L, like the two channel set-up)
                    self.intent = 0;
                    self.start_measure(cx);
                }
            }
            if busy {
                ui.add(egui::ProgressBar::new(cx.sweep.progress()).desired_width(120.0).show_percentage());
            }
            match &cx.sweep.state {
                State::Arming(_) => {
                    ui.label(RichText::new("starting output...").small().weak());
                }
                State::Computing { .. } => {
                    ui.label(RichText::new("deconvolving...").small().weak());
                }
                State::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(255, 130, 100), e);
                }
                _ => {}
            }
            if !self.note.is_empty() {
                ui.label(RichText::new(&self.note).small().weak());
            }
        });
        egui::CollapsingHeader::new("Set-up").default_open(self.curve.is_none()).show(ui, |ui| {
                        if let Some(d) = cx.dats.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("DATS V3: {}", d.label)).color(Color32::from_rgb(120, 220, 140)));
                    if crate::mapping::btn(ui, "Set up nodes").on_hover_text("Routing graph: generator and output node on the DATS output, input node on its input, wired to the analyzer. Nothing is deleted; other output nodes on the DATS output are muted.").clicked() {
                        cx.req.dats_nodes = true;
                        self.mode = 2;
                        if self.cal_r == 100.0 {
                            self.cal_r = 1000.0;
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mode, 2, "DATS V3").on_hover_text("The Dayton DATS V3: R input = reference, L input = test terminals; calibrated with its built-in 1 kΩ resistor");
                ui.radio_value(&mut self.mode, 0, "Two channels").on_hover_text("R input at the generator side of the resistor (the reference), L input across the device. The sound card cancels; no volts need to be known.");
                ui.radio_value(&mut self.mode, 1, "One channel, calibrated").on_hover_text("Only L across the device. The voltages of the output and of the input have to be known, or calibrated with a known resistor.");
            });
            egui::Grid::new("imp_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                if self.mode != 2 {
                    ui.label("Series resistor");
                    ui.add(egui::DragValue::new(&mut self.r_ref).range(0.1..=100_000.0).speed(0.5).suffix(" Ω"));
                    ui.end_row();
                    ui.label("Output impedance");
                    ui.add(egui::DragValue::new(&mut self.r_out).range(0.0..=1000.0).speed(0.05).suffix(" Ω")).on_hover_text("Adds to the series resistor (a line output is often 50 to 100 Ω, a headphone output a few ohm)");
                    ui.end_row();
                }
                if self.mode == 1 {
                    ui.label("Output: 0 dBFS is");
                    ui.add(egui::DragValue::new(&mut self.out_v).range(0.001..=100.0).speed(0.01).suffix(" V rms")).on_hover_text("The voltage at the generator output (no load) for a full scale sine, from a multimeter");
                    ui.end_row();
                    ui.label("Input: 0 dBFS is");
                    ui.add(egui::DragValue::new(&mut self.in_v).range(0.001..=100.0).speed(0.01).suffix(" V rms")).on_hover_text("The voltage at the input that reads full scale");
                    ui.end_row();
                }
                if self.mode != 2 {
                ui.label("Known R");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.cal_r).range(0.1..=100_000.0).speed(0.5).suffix(" Ω"));
                    let ok = cx.sweep.result.is_some() && !busy;
                    if ui.add_enabled(ok, egui::Button::new("Calibrate")).on_hover_text("Connect a resistor of this value in place of the device, press Measure, then Calibrate: the level is set so that it reads this value").clicked() {
                        if let Some(g) = cx.sweep.result.as_ref().and_then(|r| calibration_gain(r, self.r_ref + self.r_out, self.cal_r)) {
                            self.cal_gain = Some(g);
                            self.save_cal();
                            self.rebuild(cx);
                            self.note = format!("Calibrated with {}", fmt_ohm(self.cal_r));
                        }
                    }
                    if self.cal_gain.is_some() && crate::mapping::small_btn(ui, "Reset").clicked() {
                        self.cal_gain = None;
                        self.save_cal();
                        self.rebuild(cx);
                    }
                });
                ui.end_row();
                }
            });
            if self.mode == 2 {
                ui.separator();
                let ok = !busy;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Calibration").strong()).on_hover_text("Once is enough: it is saved. Without it the values of a DATS V3 that was measured are used (series resistance 94.2 Ω, gain 0.9989). R input = generator side of the series resistor, L input = test terminals.");
                    if (self.cal_open.is_some() || self.cal_pt.is_some() || self.short.is_some()) && crate::mapping::small_btn(ui, "Reset").clicked() {
                        self.cal_open = None;
                        self.cal_pt = None;
                        self.cal_rs = None;
                        self.cal_gain = None;
                        self.short = None;
                        self.save_cal();
                    }
                });
                for (k, name, what, hint) in [(3, "Open", "terminals empty", "Nothing on the test terminals: gives the input gain"), (4, "1 kΩ", "built-in resistor", "The built-in 1 kΩ resistor on the terminals: gives the series resistance"), (5, "Leads", "clamped together", "The two test leads you measure with, clamped together: their resistance and inductance are taken off every measurement")] {
                    let done = match k {
                        3 => self.cal_open.is_some(),
                        4 => self.cal_pt.is_some(),
                        _ => self.short.is_some(),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("•").color(if done { Color32::from_rgb(120, 220, 140) } else { Color32::GRAY }));
                        ui.label(RichText::new(name).strong()).on_hover_text(hint);
                        ui.label(RichText::new(what).small().weak());
                        if ui.add_enabled(ok, egui::Button::new(if done { "Redo" } else { "Measure" })).on_hover_text(hint).clicked() {
                            self.intent = k;
                            self.start_measure(cx);
                        }
                    });
                }
                if let (Some(g), Some(rs)) = (self.cal_gain, self.cal_rs) {
                    let leads = self.short.as_ref().map(|sh| format!("   leads {:.2} Ω, {:.1} µH", sh.at(100.0).0, sh.at(10000.0).1 / (2.0 * std::f32::consts::PI * 10000.0) * 1e6)).unwrap_or_default();
                    ui.label(RichText::new(format!("{}  gain {g:.4}{leads}", fmt_ohm(rs))).small().color(Color32::from_rgb(120, 220, 140)));
                }
            }
            if let Some(g) = self.cal_gain.filter(|_| self.mode != 2) {
                ui.label(RichText::new(format!("Calibrated: the measured voltage ratio is multiplied by {g:.4}")).small().color(Color32::from_rgb(120, 220, 140)));
            }
            ui.label(RichText::new("Sweep settings: Frequency response window (-30 to -20 dBFS for a speaker).").small().weak());
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Smooth").small().weak());
            let before = self.smooth;
            egui::ComboBox::from_id_salt("imp_sm").width(78.0).selected_text(SMOOTH[self.smooth].1).show_ui(ui, |ui| {
                for (i, s) in SMOOTH.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth, i, s.1);
                }
            });
            let _ = before;
            ui.checkbox(&mut self.show_phase, "Phase").on_hover_text("The phase of the impedance under the magnitude (0 for a resistor, positive for a coil, negative for a capacitor)");
            ui.checkbox(&mut self.auto, "Auto scale");
            ui.add_enabled_ui(!self.auto, |ui| {
                ui.add(egui::DragValue::new(&mut self.y_lo).range(-60.0..=self.y_hi - 5.0).speed(0.5).prefix("min ").suffix(" dBΩ"));
                ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 5.0..=120.0).speed(0.5).prefix("max ").suffix(" dBΩ"));
            });
            if ui.add_enabled(self.curve.is_some() && self.save.is_none(), egui::Button::new("CSV")).on_hover_text("Export the impedance, phase, resistance and reactance").clicked() {
                self.save = Some(dialogs::save("Export impedance", "CSV", &["csv"], "impedance.csv".into()));
            }
            if ui.add_enabled(self.curve.is_some(), egui::Button::new("Report")).on_hover_text("Save an HTML report with the plots and the parameters, and open it").clicked() {
                cx.req.report = Some(ReportReq { title: if self.name.trim().is_empty() { "Impedance measurement".to_string() } else { format!("Impedance: {}", self.name.trim()) }, notes: self.cond.trim().to_string(), include: vec!["Impedance".to_string()], open: true, path: None });
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Driver").small().weak());
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(150.0).hint_text("name of the driver"));
            ui.label(RichText::new("Conditions").small().weak());
            ui.add(egui::TextEdit::singleline(&mut self.cond).desired_width(220.0).hint_text("free air, 20 °C, -25 dBFS ...")).on_hover_text("Both go into the report and the copied parameters");
        });
        let store = self.curve.as_ref().map(|c| (c.f.clone(), c.db.clone(), c.phase_deg.clone()));
        self.lib.ui(ui, store);
        egui::CollapsingHeader::new("Driver (Thiele-Small)").default_open(self.free.is_some()).id_salt("imp_ts").show(ui, |ui| {
            let busy = cx.sweep.busy();
            ui.label(RichText::new("Free air: the driver hanging on its leads, from below a quarter of Fs up to 20 kHz.").small().weak());
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("Measure")).on_hover_text("Runs the sweep and takes Re, Fs, Qms, Qes, Qts and Le from it").clicked() {
                    self.intent = 1;
                    self.start_measure(cx);
                }
                if ui.add_enabled(self.curve.is_some(), egui::Button::new("Use previous")).on_hover_text("The impedance below as the free-air measurement").clicked() {
                    self.take_ts(false);
                }
            });
            ui.separator();
            ui.label(RichText::new("V(as) and M(ms)").strong()).on_hover_text("One more piece of information is needed: choose how");
            ui.horizontal_wrapped(|ui| {
                for (k, name) in ["Test box", "Added mass", "Specified SPL", "Specified Mmd"].into_iter().enumerate() {
                    ui.radio_value(&mut self.method, k, name);
                }
            });
            egui::Grid::new("imp_ts_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Piston diameter");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.d_mm).range(10.0..=800.0).speed(0.5).suffix(" mm")).on_hover_text("The effective diameter of the cone: from the middle of the surround on one side to the middle on the other");
                    ui.label(RichText::new(format!("{:.2} in", self.d_mm / 25.4)).small().weak());
                });
                ui.end_row();
                match self.method {
                    0 => {
                        ui.label("Box volume");
                        ui.add(egui::DragValue::new(&mut self.box_l).range(0.1..=1000.0).speed(0.1).suffix(" l")).on_hover_text("A sealed box, well filled with damping, about the volume of the driver's Vas");
                        ui.end_row();
                    }
                    1 => {
                        ui.label("Added mass");
                        ui.add(egui::DragValue::new(&mut self.add_g).range(0.1..=1000.0).speed(0.1).suffix(" g")).on_hover_text("Modelling clay stuck evenly around the dust cap; weigh it. About the mass of the cone or less.");
                        ui.end_row();
                    }
                    2 => {
                        ui.label("Sensitivity");
                        ui.add(egui::DragValue::new(&mut self.spl).range(50.0..=130.0).speed(0.1).suffix(" dB 1W/1m")).on_hover_text("From the data sheet");
                        ui.end_row();
                    }
                    _ => {
                        ui.label("M(md)");
                        ui.add(egui::DragValue::new(&mut self.mmd_g).range(0.1..=2000.0).speed(0.1).suffix(" g")).on_hover_text("The cone, the coil and half of the surround without the air load: from the data sheet");
                        ui.end_row();
                    }
                }
            });
            if self.method < 2 {
                ui.horizontal_wrapped(|ui| {
                    let what = if self.method == 0 { "the driver in the test box" } else { "the driver with the mass on the cone" };
                    if ui.add_enabled(!busy, egui::Button::new("Measure")).on_hover_text(format!("Runs the sweep with {what} and takes its resonance")).clicked() {
                        self.intent = 2;
                        self.start_measure(cx);
                    }
                    if ui.add_enabled(self.curve.is_some(), egui::Button::new("Use as V(as)")).on_hover_text(format!("The impedance below is {what}")).clicked() {
                        self.take_ts(true);
                    }
                    if let Some(f) = self.fs2 {
                        ui.label(RichText::new(format!("F(s2) {f:.1} Hz")).small().weak());
                    }
                });
            }
            if !self.ts_note.is_empty() {
                ui.colored_label(Color32::from_rgb(255, 170, 90), &self.ts_note);
            }
            if let Some(a) = self.free {
                ui.separator();
                let mech = ts::mechanics(&a, self.d_mm * 1e-3, self.vas_method(), self.fs2);
                egui::Grid::new("imp_ts_res").num_columns(4).spacing([14.0, 3.0]).show(ui, |ui| {
                    let mut row = |ui: &mut Ui, k1: &str, v1: String, k2: &str, v2: String| {
                        ui.label(RichText::new(k1).small().weak());
                        ui.label(RichText::new(v1).monospace());
                        ui.label(RichText::new(k2).small().weak());
                        ui.label(RichText::new(v2).monospace());
                        ui.end_row();
                    };
                    let m = |f: &dyn Fn(&ts::Mech) -> String| mech.as_ref().map(f).unwrap_or_else(|_| "-".into());
                    row(ui, "R(e)", format!("{:.3} Ω", a.re), "V(as)", m(&|m| format!("{:.2} l  {:.3} cu ft", m.vas * 1e3, m.vas * 35.3147)));
                    row(ui, "F(s)", format!("{:.2} Hz", a.fs), "M(ms)", m(&|m| format!("{:.2} g", m.mms * 1e3)));
                    row(ui, "Q(ms)", format!("{:.3}", a.qms), "C(ms)", m(&|m| format!("{:.3} mm/N", m.cms * 1e3)));
                    row(ui, "Q(es)", format!("{:.3}", a.qes), "Bl", m(&|m| format!("{:.2} T·m", m.bl)));
                    row(ui, "Q(ts)", format!("{:.3}", a.qts), "R(ms)", m(&|m| format!("{:.3} kg/s", m.rms)));
                    row(ui, "L(e) 1 kHz", format!("{:.3} mH", a.le_1k * 1e3), "η0", m(&|m| format!("{:.3} %", m.eta0 * 100.0)));
                    row(ui, "L(e) 10 kHz", format!("{:.3} mH", a.le_10k * 1e3), "SPL 1W/1m", m(&|m| format!("{:.1} dB", m.spl)));
                    row(ui, "Z(max)", format!("{:.2} Ω", a.zmax), "R(t) leads", self.short.as_ref().map_or("-".to_string(), |sh| format!("{:.3} Ω", sh.at(100.0).0)));
                });
                if let Err(e) = &mech {
                    ui.label(RichText::new(e.as_str()).small().color(Color32::from_rgb(255, 170, 90)));
                }
                ui.horizontal(|ui| {
                    if crate::mapping::small_btn(ui, "Copy").on_hover_text("The parameters as text").clicked() {
                        if let Some(t) = self.ts_text() {
                            if crate::clip::copy(&t) {
                                self.note = "Copied".into();
                            }
                        }
                    }
                    if crate::mapping::small_btn(ui, "Clear").clicked() {
                        self.free = None;
                        self.fs2 = None;
                    }
                });
            }
        });
        let Some(c) = &self.curve else {
            ui.add_space(8.0);
            ui.label(RichText::new("Press Measure. Store keeps a curve to overlay.").weak());
            return;
        };
        let ppo = LOG_N as f32 / (c.f[c.f.len() - 1] / c.f[0]).log2();
        let sm = SMOOTH[self.smooth].0;
        let cur_db = super::response::smooth_log(&c.db, ppo, sm);
        let (layers, _) = self.lib.layers(&c.f, ppo, sm);
        let (lo, hi) = layers.iter().flat_map(|(_, _, v)| v.iter()).chain(cur_db.iter()).filter(|x| x.is_finite()).fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
        let (y0, y1) = if self.auto && lo < hi { ((lo - 3.0).floor(), (hi + 3.0).ceil()) } else { (self.y_lo, self.y_hi) };

        let (resp, painter, rect) = canvas_h(ui, plot_h);
        let split = if self.show_phase { rect.height() * 0.70 } else { rect.height() };
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split));
        let pr = inset(top);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (fa, fb) = (c.f[0], *c.f.last().unwrap());
        let plot = Plot::new(pr, fa.max(1.0), fb, y0, y1.max(y0 + 6.0), true);
        // the horizontal grid in ohms (1, 2, 5 times a power of ten)
        {
            let clip = painter.with_clip_rect(pr.expand(1.0));
            let (zlo, zhi) = (10f32.powf(plot.y0 / 20.0), 10f32.powf(plot.y1 / 20.0));
            for e in (zlo.log10().floor() as i32)..=(zhi.log10().ceil() as i32) {
                for m in [1.0f32, 2.0, 5.0] {
                    let z = m * 10f32.powi(e);
                    if z < zlo || z > zhi {
                        continue;
                    }
                    let y = plot.ty(20.0 * z.log10());
                    clip.line_segment([Pos2::new(pr.left(), y), Pos2::new(pr.right(), y)], Stroke::new(1.0, if m == 1.0 { crate::views::GRID_MAJOR } else { crate::views::GRID }));
                    if m != 5.0 || (zhi / zlo) < 30.0 {
                        painter.text(Pos2::new(pr.left() - 4.0, y), Align2::RIGHT_CENTER, fmt_ohm(z), FontId::proportional(10.0), crate::views::TXT);
                    }
                }
            }
        }
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        for (_, col, v) in &layers {
            plot.line(&painter, c.f.iter().copied().zip(v.iter().copied()), Stroke::new(1.2, *col));
        }
        plot.line(&painter, c.f.iter().copied().zip(cur_db.iter().copied()), Stroke::new(1.8, Color32::from_rgb(90, 200, 255)));
        for (i, (name, col, _)) in layers.iter().enumerate() {
            painter.text(Pos2::new(pr.right() - 6.0, pr.bottom() - 6.0 - i as f32 * 12.0), Align2::RIGHT_BOTTOM, name, FontId::proportional(10.0), *col);
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "Ω", FontId::proportional(10.0), crate::views::TXT);
        if self.show_phase {
            let low = egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max);
            let lp = inset(low);
            let p2 = Plot::new(lp, plot.x0, plot.x1, -90.0, 90.0, true);
            p2.grid_y(&painter, &|v| format!("{v:.0}"));
            p2.grid_x(&painter, &|_| String::new());
            p2.frame(&painter);
            p2.line(&painter, c.f.iter().copied().zip(c.phase_deg.iter().copied()), Stroke::new(1.4, Color32::from_rgb(255, 170, 70)));
            painter.text(Pos2::new(rect.left() + 2.0, lp.top()), Align2::LEFT_TOP, "deg", FontId::proportional(9.0), crate::views::TXT);
        }
        // the numbers: the extremes and the value at 1 kHz
        let (mut imax, mut imin) = (0usize, 0usize);
        for i in 0..c.f.len() {
            if cur_db[i] > cur_db[imax] {
                imax = i;
            }
            if cur_db[i] < cur_db[imin] {
                imin = i;
            }
        }
        let i1k = c.f.partition_point(|&x| x < 1000.0).min(c.f.len() - 1);
        painter.text(
            Pos2::new(pr.left() + 6.0, pr.top() + 4.0),
            Align2::LEFT_TOP,
            format!("max {} at {}    min {} at {}    1 kHz: {} ∠ {:.0}°", fmt_ohm(10f32.powf(cur_db[imax] / 20.0)), crate::app::fmt_hz(c.f[imax]), fmt_ohm(10f32.powf(cur_db[imin] / 20.0)), crate::app::fmt_hz(c.f[imin]), fmt_ohm(10f32.powf(cur_db[i1k] / 20.0)), c.phase_deg[i1k]),
            FontId::proportional(10.5),
            Color32::from_white_alpha(190),
        );
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right() && p.y >= rect.top()) {
            let f = plot.fx(h.x);
            let i = c.f.partition_point(|&x| x < f).min(c.f.len() - 1);
            let z = 10f32.powf(cur_db[i] / 20.0);
            let ph = c.phase_deg[i].to_radians();
            let mut lines: Vec<(String, Color32)> = vec![(crate::app::fmt_hz(f), Color32::WHITE), (format!("{} ∠ {:.0}°", fmt_ohm(z), c.phase_deg[i]), Color32::from_rgb(90, 200, 255)), (format!("R {}   X {}", fmt_ohm(z * ph.cos()), fmt_ohm(z * ph.sin())), Color32::from_gray(170))];
            for m in self.lib.items.iter().filter(|m| m.visible) {
                let v = m.at(f);
                if v.is_finite() {
                    lines.push((format!("{}  {}", m.name, fmt_ohm(10f32.powf(v / 20.0))), m.color));
                }
            }
            let g: Vec<_> = lines.iter().map(|(t, c)| painter.layout_no_wrap(t.clone(), FontId::proportional(11.0), *c)).collect();
            let w = g.iter().map(|g| g.size().x).fold(0.0, f32::max) + 8.0;
            let hh: f32 = g.iter().map(|g| g.size().y).sum::<f32>() + 6.0;
            let mut p = Pos2::new(pr.right() - w - 4.0, pr.top() + 22.0);
            if h.x > p.x - 10.0 {
                p.x = pr.left() + 4.0;
            }
            painter.rect_filled(egui::Rect::from_min_size(p, egui::vec2(w, hh)), 3.0, Color32::from_black_alpha(190));
            let mut y = p.y + 3.0;
            for (g, (_, col)) in g.into_iter().zip(&lines) {
                let hgt = g.size().y;
                painter.galley(Pos2::new(p.x + 4.0, y), g, *col);
                y += hgt;
            }
            painter.line_segment([Pos2::new(h.x, pr.top()), Pos2::new(h.x, rect.bottom() - 18.0)], Stroke::new(1.0, Color32::from_white_alpha(50)));
        }
        let _ = (self.seen_id, cx.gen_running);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voltage_divider_gives_the_impedance() {
        // a pure resistor of 8 ohm behind 100 ohm: H = 8 / 108
        let h = (8.0 / 108.0, 0.0);
        let z = z_from_h(h, 100.0);
        assert!((z.0 - 8.0).abs() < 1e-3 && z.1.abs() < 1e-4, "{z:?}");
        // a coil: Z = 8 + j 6 ohm behind 100: H = Z / (100 + Z)
        let (zr, zx) = (8.0f32, 6.0f32);
        let (a, b) = (100.0 + zr, zx);
        let d = a * a + b * b;
        let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
        let z = z_from_h(h, 100.0);
        assert!((z.0 - 8.0).abs() < 1e-2 && (z.1 - 6.0).abs() < 1e-2, "{z:?}");
        // a capacitor (negative reactance) is negative
        let (zr, zx) = (1.0f32, -20.0f32);
        let (a, b) = (100.0 + zr, zx);
        let d = a * a + b * b;
        let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
        let z = z_from_h(h, 100.0);
        assert!(z.1 < -19.0, "{z:?}");
    }

    #[test]
    fn the_two_point_calibration_finds_the_gain_and_the_series_resistance() {
        // a box whose input reads 0.8 of the true ratio, with 47 ohm in series: open gives 0.8, 1 kΩ gives 0.8 * 1000 / 1047
        let (gain_true, rs_true) = (0.8f32, 47.0f32);
        let open = gain_true * 1.0;
        let with_r = gain_true * 1000.0 / (rs_true + 1000.0);
        let (g, rs) = two_point_calibration(open, with_r, 1000.0).unwrap();
        assert!((g - 1.0 / gain_true).abs() < 1e-4, "{g}");
        assert!((rs - rs_true).abs() < 0.05, "{rs}");
        // nonsense (the resistor reads higher than open) is refused
        assert!(two_point_calibration(0.5, 0.7, 1000.0).is_none());
        // the impedance of a speaker with the found values
        let h = (6.0 / (rs + 6.0), 0.0);
        let z = z_from_h(h, rs);
        assert!((z.0 - 6.0).abs() < 0.05, "{z:?}");
    }

    /// A sweep result whose transfer function is the one of the impedance `z(f)` behind the DATS series resistor.
    fn fake_result(z: impl Fn(f32) -> (f32, f32)) -> SweepResult {
        let df = 5.0f32;
        let n = 4800;
        let (mut mag, mut phase) = (Vec::new(), Vec::new());
        for k in 0..n {
            let f = (k as f32 * df).max(1.0);
            let (zr, zx) = z(f);
            // H = Z / (Rs + Z), divided by the gain of the box
            let (a, b) = (DATS_RS + zr, zx);
            let d = a * a + b * b;
            let h = ((zr * a + zx * b) / d, (zx * a - zr * b) / d);
            let h = (h.0 / DATS_GAIN, h.1 / DATS_GAIN);
            mag.push(20.0 * (h.0 * h.0 + h.1 * h.1).sqrt().max(1e-9).log10());
            phase.push(h.1.atan2(h.0));
        }
        let t = super::super::sweep::Transfer { df, mag_db: mag, phase, coh: None };
        SweepResult { harm: None, sr: 48000.0, ir: Vec::new(), pre: 0, h: t, runs: 1, capture_peak_db: -20.0, params: super::super::sweep::SweepParams { f0: 20.0, f1: 20000.0, ..Default::default() }, id: 1 }
    }

    #[test]
    fn the_short_of_the_leads_is_taken_off_the_measurements() {
        // leads: 0.48 ohm and 5 microhenry; a coil of 8 ohm and 0.5 mH behind them
        let leads = |f: f32| (0.48f32, 2.0 * std::f32::consts::PI * f * 5e-6);
        let dut = |f: f32| (8.0f32, 2.0 * std::f32::consts::PI * f * 0.5e-3);
        let total = |f: f32| {
            let (a, b) = (leads(f), dut(f));
            (a.0 + b.0, a.1 + b.1)
        };
        let setup = Setup { r_ref: DATS_RS, r_out: 0.0, gain: DATS_GAIN };
        let sh = fake_result(leads);
        let (f, z) = z_points(&sh, setup);
        let short = ZShort { f, re: z.iter().map(|v| v.0).collect(), im: z.iter().map(|v| v.1).collect() };
        assert!((short.at(1000.0).0 - 0.48).abs() < 0.01, "{:?}", short.at(1000.0));
        let m = fake_result(total);
        let plain = curve_from_sweep(&m, setup, None);
        let fixed = curve_from_sweep(&m, setup, Some(&short));
        let k = fixed.f.partition_point(|&x| x < 1000.0);
        // without the compensation the leads are in the number; with it the coil of 8 ohm and 0.5 mH is left
        let want = ((8.0f32).powi(2) + (2.0 * std::f32::consts::PI * fixed.f[k] * 0.5e-3).powi(2)).sqrt();
        assert!((fixed.ohm(k) - want).abs() < 0.03, "{} vs {want}", fixed.ohm(k));
        assert!((plain.ohm(k) - want).abs() > 0.2, "the leads are visible without it");
        // the settings file keeps it
        let back = ZShort::decode(&short.encode()).unwrap();
        assert!((back.at(1000.0).0 - short.at(1000.0).0).abs() < 0.01 && (back.at(10000.0).1 - short.at(10000.0).1).abs() < 0.01);
    }

    #[test]
    fn the_dats_values_read_its_built_in_resistor_as_1k() {
        // what the box showed with its 1 kΩ resistor on the terminals: L / R = 0.9129
        let h = (0.9129 * DATS_GAIN, 0.0);
        let z = z_from_h(h, DATS_RS);
        assert!((z.0 - 1000.0).abs() < 3.0 && z.1.abs() < 1e-3, "{z:?}");
    }

    #[test]
    fn calibration_with_a_known_resistor_corrects_the_level() {
        // a measurement that reads 3 dB too high: 100 ohm behind 100 ohm should give 0.5, it reads 0.7071
        let h = super::super::sweep::Transfer { df: 5.0, mag_db: vec![-3.0103; 2000], phase: vec![0.0; 2000], coh: None };
        let res = SweepResult { harm: None, sr: 48000.0, ir: Vec::new(), pre: 0, h, runs: 1, capture_peak_db: -20.0, params: Default::default(), id: 1 };
        let g = calibration_gain(&res, 100.0, 100.0).unwrap();
        assert!((g - 0.7071).abs() < 0.01, "{g}");
    }

    /// With a DATS V3 connected (leads on the built-in 1 kΩ resistor): plays tones through it and prints what comes back on each input
    /// channel (cargo test --release probe_the_dats -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn probe_the_dats_hardware() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        use std::time::Duration;
        let ins = crate::audio::list_devices();
        let outs = crate::tonegen::list_output_devices();
        for d in ins.iter().chain(outs.iter()) {
            println!("device {:>3} | {}", d.id.chars().take(8).collect::<String>(), d.label);
        }
        let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 (USB AUDIO  CODEC) is connected");
        println!("found: {d:?}");
        let sh = Arc::new(crate::shared::Shared::new());
        let _a = crate::audio::start(sh.clone(), d.in_id.clone().expect("an input"));
        let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
        let status = Arc::new(Mutex::new(String::new()));
        let _g = crate::tonegen::start(params.clone(), d.out_id.clone().expect("an output"), status.clone());
        std::thread::sleep(Duration::from_millis(1500));
        println!("capture: {}   generator: {}", sh.status.lock(), status.lock());
        let sr = sh.sr.load(std::sync::atomic::Ordering::Relaxed) as f32;
        let dft = |x: &[f32], f: f32| -> (f32, f32) {
            let (mut re, mut im) = (0f64, 0f64);
            for (i, v) in x.iter().enumerate() {
                let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
                re += *v as f64 * a.cos();
                im -= *v as f64 * a.sin();
            }
            ((2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32, im.atan2(re) as f32)
        };
        for f in [0.0f32, 100.0, 300.0, 1000.0, 3000.0, 10000.0] {
            {
                let mut g = params.lock();
                g.on = f > 0.0;
                g.wave = crate::tonegen::Wave::Sine;
                g.freq = f.max(1.0);
                g.level_db = [-20.0, -20.0];
            }
            std::thread::sleep(Duration::from_millis(1200));
            let n = (sr * 0.5) as usize;
            let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
            sh.ring.lock().copy_last(&mut l, &mut r);
            if f == 0.0 {
                let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
                let db = |v: f32| 20.0 * v.max(1e-9).log10();
                println!("silence: L {:.1} dBFS rms, R {:.1} dBFS rms (peak L {:.4}, R {:.4})", db(rms(&l)), db(rms(&r)), l.iter().fold(0f32, |m, v| m.max(v.abs())), r.iter().fold(0f32, |m, v| m.max(v.abs())));
                continue;
            }
            let ((al, pl), (ar, pr)) = (dft(&l, f), dft(&r, f));
            let wrap = |x: f32| { let t = std::f32::consts::TAU; (x + std::f32::consts::PI).rem_euclid(t) - std::f32::consts::PI };
            // the impedance this reads with the values of the DATS: H = L / R
            let (mag, ang) = (al / ar.max(1e-9) * DATS_GAIN, wrap(pl - pr));
            let z = z_from_h((mag * ang.cos(), mag * ang.sin()), DATS_RS);
            println!("{f:>7.0} Hz: L {:.2} dBFS ({:.4})  R {:.2} dBFS ({:.4})   R-L phase {:.2} deg   L-R ratio {:.5}   Z = {:.4} + j {:.4} ohm", 20.0 * al.max(1e-9).log10(), al, 20.0 * ar.max(1e-9).log10(), ar, wrap(pr - pl).to_degrees(), al / ar.max(1e-9), z.0, z.1);
        }
        params.lock().on = false;
        std::thread::sleep(Duration::from_millis(300));
    }

    /// With a DATS V3 and a speaker on its terminals: a stepped sine over 20 Hz to 20 kHz, the impedance with the DATS values and the short of
    /// the leads taken off (cargo test --release measure_a_speaker -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn measure_a_speaker_with_the_dats() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        use std::time::Duration;
        let ins = crate::audio::list_devices();
        let outs = crate::tonegen::list_output_devices();
        let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 is connected");
        let sh = Arc::new(crate::shared::Shared::new());
        let _a = crate::audio::start(sh.clone(), d.in_id.clone().unwrap());
        let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
        let _g = crate::tonegen::start(params.clone(), d.out_id.clone().unwrap(), Arc::new(Mutex::new(String::new())));
        std::thread::sleep(Duration::from_millis(1500));
        let sr = sh.sr.load(std::sync::atomic::Ordering::Relaxed) as f32;
        let dft = |x: &[f32], f: f32| -> (f32, f32) {
            let (mut re, mut im) = (0f64, 0f64);
            for (i, v) in x.iter().enumerate() {
                let a = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
                re += *v as f64 * a.cos();
                im -= *v as f64 * a.sin();
            }
            ((2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32, im.atan2(re) as f32)
        };
        // the leads as measured in the short test
        let lead = |f: f32| (0.476f32, 2.0 * std::f32::consts::PI * f * 4.9e-6);
        let mut rows: Vec<(f32, f32, f32)> = Vec::new();
        let n_pts = 40;
        println!("   f (Hz)    |Z| ohm   phase deg    L level dBFS");
        for k in 0..n_pts {
            let f = 20.0 * 1000f32.powf(k as f32 / (n_pts - 1) as f32);
            {
                let mut g = params.lock();
                g.on = true;
                g.wave = crate::tonegen::Wave::Sine;
                g.freq = f;
                g.level_db = [-10.0, -10.0];
            }
            std::thread::sleep(Duration::from_millis(if f < 60.0 { 1400 } else { 800 }));
            let n = (sr * if f < 60.0 { 0.8 } else { 0.4 }) as usize;
            let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
            sh.ring.lock().copy_last(&mut l, &mut r);
            let ((al, pl), (ar, pr)) = (dft(&l, f), dft(&r, f));
            let mag = al / ar.max(1e-9) * DATS_GAIN;
            let ang = pl - pr;
            let z = z_from_h((mag * ang.cos(), mag * ang.sin()), DATS_RS);
            let ld = lead(f);
            let (zr, zx) = (z.0 - ld.0, z.1 - ld.1);
            let (m, ph) = ((zr * zr + zx * zx).sqrt(), zx.atan2(zr).to_degrees());
            println!("{f:>9.1}  {m:>8.3}  {ph:>9.1}    {:.1}", 20.0 * al.max(1e-9).log10());
            rows.push((f, m, ph));
        }
        params.lock().on = false;
        // the usual numbers: the peak (free air resonance), the minimum above it, the value at 1 kHz
        let (mut ip, mut im) = (0usize, 0usize);
        for i in 0..rows.len() {
            if rows[i].1 > rows[ip].1 {
                ip = i;
            }
        }
        for i in ip..rows.len() {
            if rows[i].1 < rows[im.max(ip)].1 || im < ip {
                im = i;
            }
        }
        println!("resonance peak {:.2} ohm at {:.0} Hz; minimum after it {:.2} ohm at {:.0} Hz; lowest value {:.2} ohm", rows[ip].1, rows[ip].0, rows[im].1, rows[im].0, rows.iter().map(|r| r.1).fold(f32::MAX, f32::min));
        std::thread::sleep(Duration::from_millis(300));
    }

    /// With a DATS V3 and a speaker: a real (smooth, logarithmic) sweep through the sweep engine of the program, then the impedance with the DATS
    /// values and the short of the leads taken off; written to a CSV for the screenshot hook (cargo test --release sweep_a_speaker -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn sweep_a_speaker_with_the_dats() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        use std::time::Duration;
        let ins = crate::audio::list_devices();
        let outs = crate::tonegen::list_output_devices();
        let d = crate::app::find_dats(&ins, &outs).expect("a DATS V3 is connected");
        let sh = Arc::new(crate::shared::Shared::new());
        let _a = crate::audio::start(sh.clone(), d.in_id.clone().unwrap());
        let params = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
        let _g = crate::tonegen::start(params.clone(), d.out_id.clone().unwrap(), Arc::new(Mutex::new(String::new())));
        std::thread::sleep(Duration::from_millis(1500));
        let mut sw = crate::tools::sweep::Sweep::new();
        sw.p.loopback = true;
        sw.p.amp_db = -10.0;
        sw.p.secs = 8.0;
        sw.p.tail = 1.5;
        sw.p.averages = 2;
        sw.p.f0 = 20.0;
        sw.p.f1 = 20000.0;
        sw.start();
        let mut req = crate::tools::Requests::default();
        let t0 = std::time::Instant::now();
        while sw.busy() && t0.elapsed() < Duration::from_secs(90) {
            sw.tick(&sh, true, &mut req);
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("sweep: {} after {:.1} s", match &sw.state { crate::tools::sweep::State::Done => "done".to_string(), crate::tools::sweep::State::Failed(e) => format!("failed: {e}"), _ => "not finished".to_string() }, t0.elapsed().as_secs_f32());
        let r = sw.result.as_ref().expect("a result");
        println!("{} run(s), capture peak {:.1} dBFS", r.runs, r.capture_peak_db);
        let setup = Setup { r_ref: DATS_RS, r_out: 0.0, gain: DATS_GAIN };
        // the leads as measured with the short (0.476 ohm, 4.9 microhenry)
        let f: Vec<f32> = (0..200).map(|i| 5.0 * 4000f32.powf(i as f32 / 199.0)).collect();
        let short = ZShort { re: vec![0.476; f.len()], im: f.iter().map(|x| 2.0 * std::f32::consts::PI * x * 4.9e-6).collect(), f };
        let c = curve_from_sweep(r, setup, Some(&short));
        let mut csv = String::from("freq_hz,db,phase_deg
");
        for i in 0..c.f.len() {
            csv += &format!("{:.3},{:.4},{:.3}
", c.f[i], c.db[i], c.phase_deg[i]);
        }
        let path = std::env::var("IMP_CSV_OUT").unwrap_or_else(|_| "impedance_sweep.csv".into());
        std::fs::write(&path, csv).unwrap();
        let (mut ip, mut lo) = (0usize, f32::MAX);
        for i in 0..c.f.len() {
            if c.db[i] > c.db[ip] {
                ip = i;
            }
            lo = lo.min(c.db[i]);
        }
        println!("peak {:.2} ohm at {:.1} Hz; lowest {:.2} ohm; written to {path}", c.ohm(ip), c.f[ip], 10f32.powf(lo / 20.0));
    }
}
