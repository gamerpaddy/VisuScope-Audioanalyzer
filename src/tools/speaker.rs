//! Speaker design: what comes after the measurements. Four tabs that work on the Thiele-Small parameters of the Impedance
//! window and on the stored traces: a box simulation (sealed / vented), a crossover from driver traces, the merge of a
//! near-field with a far-field measurement, and polar / directivity plots from traces taken at several angles.

use super::plot::{canvas, canvas_h, inset, lerp_col, Plot};
use super::traces::{Library, Meas};
use super::ts::C_AIR;
use super::*;
use eframe::egui::{self, Align2, FontId, Pos2, RichText, Stroke};
use rustfft::num_complex::Complex32 as C;

const TABS: [&str; 4] = ["Box", "Crossover", "Near / far merge", "Polar"];
pub const XO: [&str; 7] = ["off", "Butterworth 6", "Butterworth 12", "Butterworth 18", "Linkwitz-Riley 12", "Linkwitz-Riley 24", "Linkwitz-Riley 48"];
const RHO: f32 = 1.18;
const WAY_COL: [Color32; 3] = [Color32::from_rgb(255, 150, 70), Color32::from_rgb(110, 220, 140), Color32::from_rgb(110, 170, 255)];

/// Butterworth low-pass of order `n` (1 to 4) at `x` = f / fc.
fn bw(n: usize, x: f32) -> C {
    let s = C::new(0.0, x);
    let one = C::new(1.0, 0.0);
    let d = match n {
        1 => s + one,
        2 => s * s + s * std::f32::consts::SQRT_2 + one,
        3 => (s + one) * (s * s + s + one),
        _ => (s * s + s * 0.765_366_9 + one) * (s * s + s * 1.847_759 + one),
    };
    one / d
}

/// Response of the crossover filter `kind` (index into `XO`) with the corner `fc` at `f`.
pub fn xo_filter(kind: usize, f: f32, fc: f32, hp: bool) -> C {
    let (n, sq) = match kind {
        0 => return C::new(1.0, 0.0),
        1 => (1, false),
        2 => (2, false),
        3 => (3, false),
        4 => (1, true),
        5 => (2, true),
        _ => (4, true),
    };
    let x = f / fc.max(1.0);
    let mut h = bw(n, x);
    if hp {
        h *= C::new(0.0, x).powi(n as i32);
    }
    if sq { h * h } else { h }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Drv {
    pub fs: f32,
    pub qts: f32,
    pub qes: f32,
    pub vas_l: f32,
    pub sd_cm2: f32,
    pub xmax_mm: f32,
    /// DC resistance of the voice coil (ohm), for the impedance curve
    pub re: f32,
}

pub struct BoxOut {
    pub f: Vec<f32>,
    pub db: Vec<f32>,
    pub x_mm: Vec<f32>,
    /// peak air speed in the port (m/s), zero for a sealed box
    pub port: Vec<f32>,
    /// magnitude of the electrical impedance in the box (ohm, without the inductance of the voice coil)
    pub z: Vec<f32>,
    pub f3: f32,
}

/// Small-signal model of a driver in a sealed box, or in a vented one tuned to `fb` with the leakage `ql`: the level at 1 m
/// (half space) with `watts` into the nominal resistance, the peak excursion of the cone and the peak air speed in a port of
/// `port_cm2`.
pub fn box_sim(d: &Drv, vented: bool, vb_l: f32, fb: f32, ql: f32, watts: f32, port_cm2: f32) -> BoxOut {
    let a = d.vas_l / vb_l.max(0.01);
    let eta0 = 4.0 * std::f32::consts::PI.powi(2) / C_AIR.powi(3) * d.fs.powi(3) * d.vas_l * 1e-3 / d.qes.max(0.01);
    let pa = (eta0 * watts).max(1e-12);
    let h = fb / d.fs;
    let one = C::new(1.0, 0.0);
    let n = 360;
    let f: Vec<f32> = (0..n).map(|i| 10.0 * 200f32.powf(i as f32 / (n - 1) as f32)).collect();
    let (mut db, mut x_mm, mut port, mut z) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let qms = if d.qes > d.qts * 1.001 { 1.0 / (1.0 / d.qts - 1.0 / d.qes) } else { 1000.0 };
    let res = d.re * qms / d.qes.max(0.01);
    for &fr in &f {
        let s = C::new(0.0, fr / d.fs);
        let (g, cone, prt) = if vented {
            let sn = s / h.sqrt();
            let (a1, a2, a3) = ((ql + h * d.qts) / (h.sqrt() * ql * d.qts), (h + (a + 1.0 + h * h) * ql * d.qts) / (h * ql * d.qts), (h * ql + d.qts) / (h.sqrt() * ql * d.qts));
            let g = sn.powi(4) / (sn.powi(4) + sn.powi(3) * a1 + sn * sn * a2 + sn * a3 + one);
            let leak = s * (h / ql) + C::new(h * h, 0.0);
            (g, ((s * s + leak) / (s * s)).norm(), (leak / (s * s)).norm())
        } else {
            let fc = (1.0 + a).sqrt();
            (s * s / (s * s + s * (fc / (d.qts * fc)) + C::new(fc * fc, 0.0)), 1.0, 0.0)
        };
        // the motional impedance: the same system with the mechanical Q alone
        let zm = if vented {
            let den = s.powi(4) + s.powi(3) * (h / ql + 1.0 / qms) + s * s * (h * h + 1.0 + a + h / (ql * qms)) + s * (h / ql + h * h / qms) + C::new(h * h, 0.0);
            s / qms * (s * s + s * (h / ql) + C::new(h * h, 0.0)) / den
        } else {
            s / qms / (s * s + s / qms + C::new(1.0 + a, 0.0))
        };
        z.push((C::new(d.re, 0.0) + zm * res).norm());
        let w = std::f32::consts::TAU * fr;
        // the volume velocity that radiates this power into half space
        let u = (std::f32::consts::TAU * C_AIR * pa / RHO).sqrt() / w * g.norm();
        db.push(112.1 + 10.0 * pa.log10() + 20.0 * g.norm().max(1e-9).log10());
        x_mm.push(std::f32::consts::SQRT_2 * u * cone / (w * d.sd_cm2.max(0.1) * 1e-4) * 1000.0);
        port.push(std::f32::consts::SQRT_2 * u * prt / (port_cm2.max(0.1) * 1e-4));
    }
    let top = *db.last().unwrap();
    let f3 = (0..n).rev().find(|&i| db[i] < top - 3.0).map(|i| f[(i + 1).min(n - 1)]).unwrap_or(f[0]);
    BoxOut { f, db, x_mm, port, z, f3 }
}

/// Phase of a trace at `f` in degrees (zero when it has none).
fn phase_at(m: &Meas, f: f32) -> f32 {
    if m.phase.len() != m.f.len() || m.f.len() < 2 {
        return 0.0;
    }
    let j = m.f.partition_point(|&x| x < f).clamp(1, m.f.len() - 1);
    let t = ((f.ln() - m.f[j - 1].ln()) / (m.f[j].ln() - m.f[j - 1].ln()).max(1e-9)).clamp(0.0, 1.0);
    m.phase[j - 1] + t * (m.phase[j] - m.phase[j - 1])
}

fn log_grid(f0: f32, f1: f32, n: usize) -> Vec<f32> {
    (0..n).map(|i| f0 * (f1 / f0).powf(i as f32 / (n - 1) as f32)).collect()
}

/// Near-field and far-field levels joined at `fx`: the near field below, the far field above, cross-faded over `width` octaves.
/// `baffle_cm` > 0 takes the baffle step out of the near field (6 dB less below the step). Returns the merged curve and the
/// level that was added to the near field.
pub fn merge_nf(near: &Meas, far: &Meas, f: &[f32], fx: f32, width: f32, baffle_cm: f32, auto: bool, offset: f32) -> (Vec<f32>, f32) {
    let step = |x: f32| if baffle_cm > 0.0 { let f3 = 115.0 / (baffle_cm * 0.01); -6.02 * f3 * f3 / (x * x + f3 * f3) } else { 0.0 };
    let w = width.max(0.05);
    let band: Vec<f32> = f.iter().filter(|&&x| (x / fx).log2().abs() <= w * 0.5).map(|&x| far.at(x) - near.at(x) - step(x)).filter(|v| v.is_finite()).collect();
    let off = if auto && !band.is_empty() { band.iter().sum::<f32>() / band.len() as f32 } else { offset };
    let out = f
        .iter()
        .map(|&x| {
            let (n, fa) = (near.at(x) + step(x) + off, far.at(x));
            let t = (((x / fx).log2() + w * 0.5) / w).clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t);
            match (n.is_finite(), fa.is_finite()) {
                (true, true) => n * (1.0 - t) + fa * t,
                (true, false) => n,
                _ => fa,
            }
        })
        .collect();
    (out, off)
}

/// Directivity index in dB from levels (dB relative to on-axis) at angles from 0 to 180 degrees, rotational symmetry assumed;
/// beyond the last measured angle the last level is kept.
pub fn directivity_index(pts: &[(f32, f32)]) -> f32 {
    if pts.is_empty() {
        return 0.0;
    }
    let lvl = |a: f32| {
        let j = pts.partition_point(|p| p.0 < a);
        if j == 0 {
            pts[0].1
        } else if j >= pts.len() {
            pts[pts.len() - 1].1
        } else {
            let t = (a - pts[j - 1].0) / (pts[j].0 - pts[j - 1].0).max(1e-6);
            pts[j - 1].1 + t * (pts[j].1 - pts[j - 1].1)
        }
    };
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for k in 0..180 {
        let a = k as f32 + 0.5;
        let s = a.to_radians().sin();
        num += 10f32.powf(lvl(a) / 10.0) * s;
        den += s;
    }
    -10.0 * (num / den).max(1e-9).log10()
}

#[derive(Clone, Copy)]
struct Way {
    on: bool,
    tr: usize,
    hp: usize,
    hp_f: f32,
    lp: usize,
    lp_f: f32,
    gain: f32,
    delay_ms: f32,
    inv: bool,
}

/// What the crossover tab draws: the level of each active way, the sum as complex values, and (when asked for) the level of the
/// sum with the second active way inverted.
pub struct XoOut {
    pub curves: Vec<Vec<f32>>,
    pub sum: Vec<C>,
    pub rev: Option<Vec<f32>>,
}

/// One way at `x` as a complex number: its trace (level and phase) with the filters, gain, delay and polarity; None where the trace
/// has no data.
fn way_response(items: &[Meas], w: &Way, x: f32) -> Option<C> {
    let m = items.get(w.tr.min(items.len().checked_sub(1)?))?;
    let db = m.at(x);
    if !db.is_finite() {
        return None;
    }
    let ph = phase_at(m, x).to_radians() - std::f32::consts::TAU * x * w.delay_ms * 1e-3;
    let h = C::from_polar(10f32.powf((db + w.gain) / 20.0), ph) * xo_filter(w.hp, x, w.hp_f, true) * xo_filter(w.lp, x, w.lp_f, false);
    Some(if w.inv { -h } else { h })
}

/// The crossover over the frequencies `f` for the active ways `act` (indices into `ways`). The reversed sum is the sum with the
/// second active way turned over, which is the sum minus twice that way.
fn xo_response(items: &[Meas], ways: &[Way; 3], act: &[usize], f: &[f32], want_rev: bool) -> XoOut {
    let hs: Vec<Vec<Option<C>>> = act.iter().map(|&k| f.iter().map(|&x| way_response(items, &ways[k], x)).collect()).collect();
    let db = |c: C| 20.0 * c.norm().max(1e-9).log10();
    let curves = hs.iter().map(|h| h.iter().map(|c| c.map(db).unwrap_or(f32::NAN)).collect()).collect();
    let sum: Vec<C> = (0..f.len()).map(|i| hs.iter().filter_map(|h| h[i]).sum::<C>()).collect();
    let rev = (want_rev && hs.len() >= 2).then(|| (0..f.len()).map(|i| db(sum[i] - hs[1][i].map_or(C::new(0.0, 0.0), |c| c * 2.0))).collect());
    XoOut { curves, sum, rev }
}

pub struct Speaker {
    tab: usize,
    drv: Drv,
    vented: bool,
    vb_l: f32,
    fb: f32,
    ql: f32,
    watts: f32,
    port_d_cm: f32,
    /// the lower plot of the box tab shows the impedance instead of the excursion
    box_z: bool,
    ways: [Way; 3],
    reverse_null: bool,
    near: usize,
    far: usize,
    fx: f32,
    width: f32,
    baffle_cm: f32,
    auto_lvl: bool,
    offset: f32,
    /// traces used for the polar plot (by name) and their angles
    pol: Vec<(String, f32)>,
    pol_f: f32,
    pol_range: f32,
    next_angle: f32,
    angle_step: f32,
    /// the sweep that is running is stored under this angle when it is done
    pending: Option<(f32, u64)>,
    /// turn the table by a serial command before each sweep, and wait this long for it
    turn_on: bool,
    turn_cmd: String,
    settle: f32,
    /// the series goes on by itself up to this angle
    auto_max: f32,
    auto: bool,
    /// the table is turning to this angle since then
    turning: Option<(f32, std::time::Instant)>,
}

fn pick(ui: &mut Ui, id: &str, lib: &Library, sel: &mut usize) {
    let name = lib.items.get(*sel).map(|m| m.name.clone()).unwrap_or_else(|| "-".into());
    egui::ComboBox::from_id_salt(id).width(120.0).selected_text(name).show_ui(ui, |ui| {
        for (i, m) in lib.items.iter().enumerate() {
            ui.selectable_value(sel, i, &m.name);
        }
    });
}

fn no_traces(ui: &mut Ui, what: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(format!("{what}\nStore curves with the Store button of the Frequency response window (or load FRD / text files in the Traces window).")).weak());
}

impl Speaker {
    pub fn new() -> Self {
        let w = |on, hp, hp_f, lp, lp_f| Way { on, tr: 0, hp, hp_f, lp, lp_f, gain: 0.0, delay_ms: 0.0, inv: false };
        Self {
            tab: 0,
            drv: Drv { fs: 40.0, qts: 0.4, qes: 0.45, vas_l: 50.0, sd_cm2: 130.0, xmax_mm: 5.0, re: 6.0 },
            vented: false,
            vb_l: 25.0,
            fb: 40.0,
            ql: 7.0,
            watts: 1.0,
            port_d_cm: 5.0,
            box_z: false,
            ways: [w(true, 0, 80.0, 5, 2500.0), w(true, 5, 2500.0, 0, 20000.0), w(false, 5, 300.0, 5, 3000.0)],
            reverse_null: false,
            near: 0,
            far: 1,
            fx: 300.0,
            width: 1.0,
            baffle_cm: 0.0,
            auto_lvl: true,
            offset: 0.0,
            pol: Vec::new(),
            pol_f: 2000.0,
            pol_range: 30.0,
            next_angle: 0.0,
            angle_step: 15.0,
            pending: None,
            turn_on: false,
            turn_cmd: "G{angle}".into(),
            settle: 3.0,
            auto_max: 90.0,
            auto: false,
            turning: None,
        }
    }

    fn box_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let last = *super::ts::LAST.lock();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(last.is_some(), egui::Button::new(crate::lang::tr("Take from Impedance"))).on_hover_text(crate::lang::tr("Use the Thiele-Small parameters the Impedance window worked out (Fs, Qts, Qes, and Vas and the cone area when it has them). Measure the driver there first; this tab only simulates")).clicked()
                && let Some((a, m, sd)) = last {
                    self.drv.fs = a.fs;
                    self.drv.qts = a.qts;
                    self.drv.qes = a.qes;
                    self.drv.re = a.re;
                    self.drv.sd_cm2 = sd * 1e4;
                    if let Some(m) = m {
                        self.drv.vas_l = m.vas * 1e3;
                    }
                }
            let d = &mut self.drv;
            ui.add(egui::DragValue::new(&mut d.fs).range(5.0..=2000.0).speed(0.2).prefix(crate::lang::tr("Fs ")).suffix(crate::lang::tr(" Hz")));
            ui.add(egui::DragValue::new(&mut d.qts).range(0.05..=3.0).speed(0.005).prefix(crate::lang::tr("Qts ")));
            ui.add(egui::DragValue::new(&mut d.qes).range(0.05..=5.0).speed(0.005).prefix(crate::lang::tr("Qes ")));
            ui.add(egui::DragValue::new(&mut d.vas_l).range(0.05..=2000.0).speed(0.2).prefix(crate::lang::tr("Vas ")).suffix(" l"));
            ui.add(egui::DragValue::new(&mut d.sd_cm2).range(1.0..=3000.0).speed(0.5).prefix(crate::lang::tr("Sd ")).suffix(crate::lang::tr(" cm2")));
            ui.add(egui::DragValue::new(&mut d.re).range(0.1..=200.0).speed(0.02).prefix(crate::lang::tr("Re ")).suffix(" Ω"));
            ui.add(egui::DragValue::new(&mut d.xmax_mm).range(0.1..=60.0).speed(0.05).prefix(crate::lang::tr("Xmax ")).suffix(crate::lang::tr(" mm")));
        });
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.vented, false, crate::lang::tr("Sealed"));
            ui.selectable_value(&mut self.vented, true, crate::lang::tr("Vented"));
            ui.add(egui::DragValue::new(&mut self.vb_l).range(0.1..=2000.0).speed(0.1).prefix(crate::lang::tr("box ")).suffix(" l"));
            if self.vented {
                ui.add(egui::DragValue::new(&mut self.fb).range(5.0..=500.0).speed(0.1).prefix(crate::lang::tr("tuned to ")).suffix(crate::lang::tr(" Hz")));
                ui.add(egui::DragValue::new(&mut self.port_d_cm).range(1.0..=40.0).speed(0.05).prefix(crate::lang::tr("port dia ")).suffix(crate::lang::tr(" cm")));
                ui.add(egui::DragValue::new(&mut self.ql).range(2.0..=30.0).speed(0.1).prefix(crate::lang::tr("QL "))).on_hover_text(crate::lang::tr("Leakage losses of the box: 7 is a usual value"));
            }
            ui.add(egui::DragValue::new(&mut self.watts).range(0.01..=5000.0).speed(0.1).prefix(crate::lang::tr("power ")).suffix(" W"));
            ui.selectable_value(&mut self.box_z, false, crate::lang::tr("excursion"));
            ui.selectable_value(&mut self.box_z, true, crate::lang::tr("impedance")).on_hover_text(crate::lang::tr("The impedance the driver will show in this box (without the inductance of the voice coil): compare it with a measurement of the finished box in the Impedance window. A vented box shows two peaks with the tuning in the dip between them"));
            if ui.button(crate::lang::tr("Suggest")).on_hover_text(crate::lang::tr("Sealed: the volume for Qtc 0.71. Vented: a flat alignment (volume 15 Qts^2.87 Vas, tuning 0.42 Fs / Qts^0.9)")).clicked() {
                let d = self.drv;
                if self.vented {
                    self.vb_l = (15.0 * d.qts.powf(2.87) * d.vas_l).clamp(0.1, 2000.0);
                    self.fb = (0.42 * d.fs * d.qts.powf(-0.9)).clamp(5.0, 500.0);
                } else if d.qts < 0.7 {
                    self.vb_l = (d.vas_l / ((0.707 / d.qts).powi(2) - 1.0)).clamp(0.1, 2000.0);
                }
            }
        });
        let port_cm2 = std::f32::consts::PI * (self.port_d_cm * 0.5).powi(2);
        let o = box_sim(&self.drv, self.vented, self.vb_l, self.fb, self.ql, self.watts, port_cm2);
        let xpk = o.x_mm.iter().zip(&o.f).filter(|x| *x.1 >= 20.0).map(|x| *x.0).fold(0.0, f32::max);
        ui.horizontal_wrapped(|ui| {
            let mut txt = format!("-3 dB at {:.1} Hz", o.f3);
            if self.vented {
                let len = 23562.5 * self.port_d_cm.powi(2) / (self.fb.powi(2) * self.vb_l) - 0.732 * self.port_d_cm;
                let v = o.port.iter().copied().fold(0.0, f32::max);
                txt += &if len > 0.0 { format!("   port {len:.1} cm long") } else { "   port: too wide for this tuning".to_string() };
                txt += &format!("   air speed in the port up to {v:.1} m/s{}", if v > 17.0 { " (audible, above 17)" } else { "" });
            } else {
                let k = (1.0 + self.drv.vas_l / self.vb_l).sqrt();
                txt += &format!("   Fc {:.1} Hz   Qtc {:.2}", self.drv.fs * k, self.drv.qts * k);
            }
            txt += &format!("   excursion up to {xpk:.1} mm above 20 Hz");
            ui.label(RichText::new(txt).monospace());
            if xpk > self.drv.xmax_mm {
                ui.colored_label(Color32::from_rgb(255, 120, 100), crate::lang::tr("over Xmax"));
            }
            if ui.button(crate::lang::tr("Store as trace")).on_hover_text(crate::lang::tr("Keep the simulated response as a trace, to compare with a measurement of the finished box")).clicked() {
                cx.lib.add(Some(format!("Box {} {:.0} l", if self.vented { "vented" } else { "sealed" }, self.vb_l)), o.f.clone(), o.db.clone(), Vec::new());
            }
        });
        let (resp, painter, rect) = canvas(ui, 160.0);
        let split = rect.height() * 0.62;
        let pr = inset(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split)));
        let lr = inset(egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max));
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let top = ((o.db.iter().copied().fold(f32::MIN, f32::max) + 6.0) / 6.0).ceil() * 6.0;
        let p1 = Plot::new(pr, 10.0, 2000.0, top - 42.0, top, true);
        p1.grid_y(&painter, &|v| format!("{v:.0}"));
        p1.grid_x(&painter, &|_| String::new());
        p1.frame(&painter);
        p1.line(&painter, o.f.iter().copied().zip(o.db.iter().copied()), Stroke::new(1.6, Color32::from_rgb(90, 200, 255)));
        painter.text(pr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, "dB SPL, 1 m, half space", FontId::proportional(9.5), crate::views::TXT);
        let xm = if self.box_z { o.z.iter().copied().fold(0.0, f32::max) * 1.1 } else { (xpk.max(self.drv.xmax_mm) * 1.2).max(1.0) };
        let p2 = Plot::new(lr, 10.0, 2000.0, 0.0, xm, true);
        p2.grid_y(&painter, &|v| format!("{v:.1}"));
        p2.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        p2.frame(&painter);
        if self.box_z {
            p2.line(&painter, o.f.iter().copied().zip(o.z.iter().copied()), Stroke::new(1.4, Color32::from_rgb(120, 230, 150)));
        } else {
            p2.line(&painter, [(10.0, self.drv.xmax_mm), (2000.0, self.drv.xmax_mm)].into_iter(), Stroke::new(1.0, Color32::from_rgb(255, 110, 90)));
            p2.line(&painter, o.f.iter().copied().zip(o.x_mm.iter().copied()), Stroke::new(1.4, Color32::from_rgb(255, 190, 80)));
        }
        painter.text(lr.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, if self.box_z { "impedance in the box, ohm" } else { "cone excursion, mm peak (red: Xmax)" }, FontId::proportional(9.5), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right()) {
            let f = p1.fx(h.x);
            let i = o.f.partition_point(|&x| x < f).min(o.f.len() - 1);
            p1.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{}   {:.1} dB   {:.2} mm   {:.1} Ω", crate::app::fmt_hz(f), o.db[i], o.x_mm[i], o.z[i]), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn xo_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        if cx.lib.items.is_empty() {
            no_traces(ui, "The crossover works on stored traces: one measured response per driver, with phase (measured from the same microphone position, reference on the R input).");
            return;
        }
        egui::Grid::new("spk_xo").num_columns(8).spacing([6.0, 3.0]).show(ui, |ui| {
            for (k, w) in self.ways.iter_mut().enumerate() {
                ui.checkbox(&mut w.on, RichText::new(format!("Way {}", k + 1)).color(WAY_COL[k]));
                pick(ui, &format!("spk_xo_tr{k}"), cx.lib, &mut w.tr);
                for (hp, kind, fc) in [(true, &mut w.hp, &mut w.hp_f), (false, &mut w.lp, &mut w.lp_f)] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if hp { "high-pass" } else { "low-pass" }).small().weak());
                        egui::ComboBox::from_id_salt(format!("spk_xo_{k}_{hp}")).width(118.0).selected_text(crate::lang::tr(XO[*kind])).show_ui(ui, |ui| {
                            for (i, n) in XO.iter().enumerate() {
                                ui.selectable_value(kind, i, crate::lang::tr(n));
                            }
                        });
                        ui.add_enabled(*kind > 0, egui::DragValue::new(fc).range(10.0..=40000.0).speed(2.0).suffix(crate::lang::tr(" Hz")));
                    });
                }
                ui.add(egui::DragValue::new(&mut w.gain).range(-40.0..=20.0).speed(0.1).suffix(crate::lang::tr(" dB")));
                ui.add(egui::DragValue::new(&mut w.delay_ms).range(0.0..=50.0).speed(0.005).suffix(crate::lang::tr(" ms"))).on_hover_text(crate::lang::tr("Delay of this way (to line up the acoustic centres)"));
                ui.checkbox(&mut w.inv, crate::lang::tr("invert"));
                ui.end_row();
            }
        });
        let f = log_grid(20.0, 20000.0, 480);
        let act: Vec<usize> = (0..3).filter(|&k| self.ways[k].on).collect();
        // (each way is worked out once for the three curves; the reversed sum only when it is shown)
        let XoOut { curves, sum, rev } = xo_response(&cx.lib.items, &self.ways, &act, &f, self.reverse_null);
        let db = |c: C| 20.0 * c.norm().max(1e-9).log10();
        let sum_db: Vec<f32> = sum.iter().map(|c| if c.norm() > 0.0 { db(*c) } else { f32::NAN }).collect();
        let no_phase = act.iter().any(|&k| cx.lib.items.get(self.ways[k].tr).is_some_and(|m| m.phase.len() != m.f.len()));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.reverse_null, crate::lang::tr("Reverse null")).on_hover_text(crate::lang::tr("Also show the sum with the second active way inverted: a deep, even notch at the crossover means the ways are in phase there"));
            if ui.button(crate::lang::tr("Store sum as trace")).clicked() {
                cx.lib.add(Some("Crossover sum".into()), f.clone(), sum_db.clone(), sum.iter().map(|c| c.arg().to_degrees()).collect());
            }
            if cx.graph_on && ui.button(crate::lang::tr("Send to routing")).on_hover_text(crate::lang::tr("Put the filters, gains and delays of the ways into the routing graph: one row of nodes per way, fed from what feeds the first output. Connect the end of each row to the output channel of its driver. A Linkwitz-Riley filter becomes two Filter nodes in a row")).clicked() {
                let chains: Vec<Vec<(u8, [f32; 3])>> = act
                    .iter()
                    .map(|&k| {
                        let w = &self.ways[k];
                        let mut ch = Vec::new();
                        for (kind, fc, ty) in [(w.hp, w.hp_f, 0.0), (w.lp, w.lp_f, 1.0)] {
                            let fc = fc.clamp(20.0, 20000.0);
                            match kind {
                                0 => {}
                                // (slopes of the Filter node: 0 = 12, 1 = 24, 3 = 6, 4 = 18 dB/oct; a Linkwitz-Riley is two Butterworth in a row)
                                1 => ch.push((0, [ty, fc, 3.0])),
                                2 => ch.push((0, [ty, fc, 0.0])),
                                3 => ch.push((0, [ty, fc, 4.0])),
                                4 => ch.extend([(0, [ty, fc, 3.0]); 2]),
                                5 => ch.extend([(0, [ty, fc, 0.0]); 2]),
                                _ => ch.extend([(0, [ty, fc, 1.0]); 2]),
                            }
                        }
                        if w.gain != 0.0 || w.inv {
                            ch.push((1, [w.gain, w.inv as u8 as f32, 0.0]));
                        }
                        if w.delay_ms > 0.0 {
                            ch.push((2, [w.delay_ms, 0.0, 100.0]));
                        }
                        ch
                    })
                    .collect();
                *crate::graph::bus().xo_send.lock() = Some(chains);
            }
            if no_phase {
                ui.colored_label(Color32::from_rgb(255, 170, 90), crate::lang::tr("a trace has no phase: the sum is only right where one way plays alone"));
            }
        });
        let (resp, painter, rect) = canvas(ui, 140.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let top = ((sum_db.iter().copied().filter(|v| v.is_finite()).fold(-200.0, f32::max) + 8.0) / 5.0).ceil() * 5.0;
        let plot = Plot::new(pr, 20.0, 20000.0, top - 50.0, top, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        for (c, &k) in curves.iter().zip(&act) {
            plot.line(&painter, f.iter().copied().zip(c.iter().copied()), Stroke::new(1.2, WAY_COL[k]));
        }
        if let (true, Some(r)) = (self.reverse_null, &rev) {
            plot.line(&painter, f.iter().copied().zip(r.iter().copied()), Stroke::new(1.0, Color32::from_rgb(150, 150, 160)));
        }
        plot.line(&painter, f.iter().copied().zip(sum_db.iter().copied()), Stroke::new(1.8, Color32::WHITE));
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let x = plot.fx(h.x);
            let i = f.partition_point(|&v| v < x).min(f.len() - 1);
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{}   sum {:.1} dB", crate::app::fmt_hz(x), sum_db[i]), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn merge_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        if cx.lib.items.len() < 2 {
            no_traces(ui, "The merge needs two stored traces: the woofer measured with the microphone at the cone (near field), and the speaker measured from a distance with a short gate (far field).");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::lang::tr("Near field"));
            pick(ui, "spk_mg_n", cx.lib, &mut self.near);
            ui.label(crate::lang::tr("far field"));
            pick(ui, "spk_mg_f", cx.lib, &mut self.far);
            ui.add(egui::DragValue::new(&mut self.fx).range(20.0..=5000.0).speed(1.0).prefix(crate::lang::tr("join at ")).suffix(crate::lang::tr(" Hz"))).on_hover_text(crate::lang::tr("Above the lowest frequency the gate of the far field allows, below where the cone stops acting as a piston"));
            ui.add(egui::DragValue::new(&mut self.width).range(0.1..=4.0).speed(0.02).prefix(crate::lang::tr("over ")).suffix(crate::lang::tr(" oct")));
            ui.add(egui::DragValue::new(&mut self.baffle_cm).range(0.0..=300.0).speed(0.2).prefix(crate::lang::tr("baffle ")).suffix(crate::lang::tr(" cm"))).on_hover_text(crate::lang::tr("Width of the baffle: takes the baffle step (6 dB less at low frequencies) out of the near field. 0 = no correction"));
            ui.checkbox(&mut self.auto_lvl, crate::lang::tr("Match level")).on_hover_text(crate::lang::tr("Shift the near field to the level of the far field around the join"));
            ui.add_enabled(!self.auto_lvl, egui::DragValue::new(&mut self.offset).range(-80.0..=80.0).speed(0.1).suffix(crate::lang::tr(" dB")));
        });
        let n = cx.lib.items.len() - 1;
        let (near, far) = (cx.lib.items[self.near.min(n)].clone(), cx.lib.items[self.far.min(n)].clone());
        if near.f.len() < 2 || far.f.len() < 2 {
            return;
        }
        let f = log_grid(near.f[0].min(far.f[0]).max(1.0), near.f.last().unwrap().max(*far.f.last().unwrap()), 600);
        let (out, off) = merge_nf(&near, &far, &f, self.fx, self.width, self.baffle_cm, self.auto_lvl, self.offset);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("near field shifted by {off:+.1} dB")).small().weak());
            if ui.button(crate::lang::tr("Store merged trace")).clicked() {
                cx.lib.add(Some(format!("Merged {} + {}", near.name, far.name)), f.clone(), out.clone(), Vec::new());
            }
        });
        let (_, painter, rect) = canvas(ui, 140.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let top = ((out.iter().copied().filter(|v| v.is_finite()).fold(-200.0, f32::max) + 8.0) / 5.0).ceil() * 5.0;
        let plot = Plot::new(pr, f[0], *f.last().unwrap(), top - 50.0, top, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        plot.line(&painter, f.iter().map(|&x| (x, near.at(x) + off)), Stroke::new(1.0, WAY_COL[0]));
        plot.line(&painter, f.iter().map(|&x| (x, far.at(x))), Stroke::new(1.0, WAY_COL[2]));
        plot.line(&painter, f.iter().copied().zip(out.iter().copied()), Stroke::new(1.8, Color32::WHITE));
        let x = plot.tx(self.fx);
        painter.line_segment([Pos2::new(x, pr.top()), Pos2::new(x, pr.bottom())], Stroke::new(1.0, Color32::from_white_alpha(60)));
    }

    /// Levels relative to the trace at the smallest angle, at `f`: (angle, dB), sorted by angle.
    fn polar_at(&self, lib: &Library, f: f32) -> Vec<(f32, f32)> {
        let mut v: Vec<(f32, f32)> = self.pol.iter().filter_map(|(n, a)| lib.items.iter().find(|m| &m.name == n).map(|m| (a.abs(), m.at(f)))).filter(|x| x.1.is_finite()).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let Some(&(_, on)) = v.first() {
            v.iter_mut().for_each(|x| x.1 -= on);
        }
        v
    }

    /// Measure at this angle: turn the table first when there is one, then sweep (see `update`).
    fn start_angle(&mut self, cx: &mut Ctx, angle: f32) {
        if self.turn_on {
            crate::serial::send(&self.turn_cmd, &[("angle", angle)]);
            self.turning = Some((angle, std::time::Instant::now()));
        } else {
            cx.sweep.start();
            self.pending = Some((angle, cx.sweep.result.as_ref().map(|r| r.id).unwrap_or(0)));
        }
    }

    fn polar_ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            let busy = cx.sweep.busy();
            if ui.add_enabled(!busy && self.pending.is_none() && self.turning.is_none(), egui::Button::new(format!("Measure at {:.0}°", self.next_angle))).on_hover_text(crate::lang::tr("Turn the speaker to this angle and press: runs the sweep of the Frequency response window (its settings, a short response window keeps the room out), stores the result as a trace named after the angle and goes on to the next angle")).clicked() {
                self.start_angle(cx, self.next_angle);
            }
            ui.add(egui::DragValue::new(&mut self.next_angle).range(0.0..=180.0).speed(0.5).suffix("°"));
            ui.add(egui::DragValue::new(&mut self.angle_step).range(1.0..=90.0).speed(0.2).prefix(crate::lang::tr("step ")).suffix("°"));
            ui.add(egui::DragValue::new(&mut self.pol_f).range(20.0..=40000.0).speed(10.0).prefix(crate::lang::tr("polar at ")).suffix(crate::lang::tr(" Hz")));
            ui.add(egui::DragValue::new(&mut self.pol_range).range(10.0..=60.0).speed(0.2).prefix(crate::lang::tr("range ")).suffix(crate::lang::tr(" dB")));
            if self.pending.is_some() {
                ui.label(RichText::new(crate::lang::tr("measuring...")).small().weak());
            }
        });
        ui.horizontal_wrapped(|ui| {
            let idle = !cx.sweep.busy() && self.pending.is_none() && self.turning.is_none();
            ui.checkbox(&mut self.turn_on, "Turntable").on_hover_text("Turn the speaker by a command on the serial port before each sweep (an Arduino with a stepper, for example). The port and its speed are set in Options > Serial port");
            ui.add_enabled(self.turn_on, egui::TextEdit::singleline(&mut self.turn_cmd).desired_width(90.0)).on_hover_text("The text sent to the table. {angle} is replaced by the angle in degrees: G{angle} sends G45 for 45°");
            ui.add_enabled(self.turn_on, egui::DragValue::new(&mut self.settle).range(0.0..=120.0).speed(0.1).prefix("wait ").suffix(" s")).on_hover_text("Time for the table to turn and come to rest before the sweep starts");
            ui.add(egui::DragValue::new(&mut self.auto_max).range(0.0..=180.0).speed(0.5).prefix("up to ").suffix("°"));
            if self.auto {
                if ui.button("Stop").clicked() {
                    (self.auto, self.turning, self.pending) = (false, None, None);
                    cx.sweep.cancel();
                }
            } else if ui.add_enabled(idle, egui::Button::new("Measure all")).on_hover_text("The whole series by itself: from the angle shown, in steps, up to the last angle. With the turntable it turns, waits and sweeps; without it, it sweeps one angle after the other at once (useful only for a test). At the end the table goes back to 0°").clicked() {
                self.auto = true;
                self.start_angle(cx, self.next_angle);
            }
            if let Some((a, _)) = self.turning {
                ui.label(RichText::new(format!("turning to {a:.0}°...")).small().weak());
            }
            if self.turn_on {
                ui.label(RichText::new(crate::serial::status()).small().weak());
            }
        });
        egui::CollapsingHeader::new(format!("Traces and angles ({} used)", self.pol.len())).id_salt("spk_pol_list").default_open(self.pol.is_empty()).show(ui, |ui| {
            ui.label(RichText::new(crate::lang::tr("Tick the stored traces that belong to the set and give each its angle (0 = on axis).")).small().weak());
            for m in &cx.lib.items {
                ui.horizontal(|ui| {
                    let k = self.pol.iter().position(|p| p.0 == m.name);
                    let mut on = k.is_some();
                    if ui.checkbox(&mut on, &m.name).changed() {
                        match k {
                            Some(k) => {
                                self.pol.remove(k);
                            }
                            None => {
                                // an angle in the name is taken over
                                let num: String = m.name.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                                self.pol.push((m.name.clone(), num.parse().unwrap_or(0.0)));
                            }
                        }
                    } else if let Some(k) = k {
                        ui.add(egui::DragValue::new(&mut self.pol[k].1).range(0.0..=180.0).speed(0.5).suffix("°"));
                    }
                });
            }
        });
        self.pol.retain(|p| cx.lib.items.iter().any(|m| m.name == p.0));
        let pts = self.polar_at(cx.lib, self.pol_f);
        if pts.len() < 2 {
            ui.label(RichText::new(crate::lang::tr("At least two angles are needed.")).weak());
            return;
        }
        ui.label(RichText::new(format!("Directivity index at {}: {:.1} dB   (rotational symmetry assumed, {} angles up to {:.0}°)", crate::app::fmt_hz(self.pol_f), directivity_index(&pts), pts.len(), pts.last().unwrap().0)).monospace());
        let (resp, painter, rect) = canvas_h(ui, ui.available_height().max(180.0));
        let side = rect.height().min(rect.width() * 0.42);
        // polar diagram on the left
        let c = Pos2::new(rect.left() + side * 0.5 + 4.0, rect.center().y);
        let rad = side * 0.5 - 16.0;
        let rng = self.pol_range;
        for k in 0..=3 {
            let r = rad * (1.0 - k as f32 / 3.0);
            painter.circle_stroke(c, r, Stroke::new(1.0, crate::views::GRID_MAJOR));
            painter.text(c + egui::vec2(2.0, -r), Align2::LEFT_BOTTOM, format!("{:.0}", -rng * k as f32 / 3.0), FontId::proportional(9.0), crate::views::TXT);
        }
        for k in 0..12 {
            let a = (k as f32 * 30.0).to_radians();
            painter.line_segment([c, c + egui::vec2(a.sin(), -a.cos()) * rad], Stroke::new(1.0, crate::views::GRID));
        }
        let lvl = |a: f32| {
            let j = pts.partition_point(|p| p.0 < a);
            if j == 0 {
                Some(pts[0].1)
            } else if j >= pts.len() {
                (a <= pts[pts.len() - 1].0 + 0.01).then(|| pts[pts.len() - 1].1)
            } else {
                let t = (a - pts[j - 1].0) / (pts[j].0 - pts[j - 1].0).max(1e-6);
                Some(pts[j - 1].1 + t * (pts[j].1 - pts[j - 1].1))
            }
        };
        let amax = pts.last().unwrap().0;
        let line: Vec<Pos2> = (-180..=180)
            .filter_map(|d| {
                let a = d as f32;
                lvl(a.abs()).map(|v| c + egui::vec2(a.to_radians().sin(), -a.to_radians().cos()) * rad * (1.0 + v / rng).clamp(0.0, 1.05))
            })
            .collect();
        painter.add(egui::Shape::line(line, Stroke::new(1.6, Color32::from_rgb(90, 200, 255))));
        // directivity map on the right: frequency across, angle up and down, level relative to on-axis as colour
        let mr = inset(egui::Rect::from_min_max(Pos2::new(rect.left() + side + 8.0, rect.top()), rect.max));
        if mr.width() < 60.0 || mr.height() < 40.0 {
            return;
        }
        let (f0, f1) = (200.0f32, 20000.0f32);
        let plot = Plot::new(mr, f0, f1, -amax, amax, true);
        let (nx, ny) = (72usize, 48usize);
        for ix in 0..nx {
            let fr = f0 * (f1 / f0).powf((ix as f32 + 0.5) / nx as f32);
            let col = self.polar_at(cx.lib, fr);
            if col.len() < 2 {
                continue;
            }
            for iy in 0..ny {
                let a = -amax + (iy as f32 + 0.5) / ny as f32 * 2.0 * amax;
                let j = col.partition_point(|p| p.0 < a.abs()).clamp(1, col.len() - 1);
                let t = ((a.abs() - col[j - 1].0) / (col[j].0 - col[j - 1].0).max(1e-6)).clamp(0.0, 1.0);
                let v = col[j - 1].1 + t * (col[j].1 - col[j - 1].1);
                let u = (1.0 + v / rng).clamp(0.0, 1.0);
                let colr = if u > 0.5 { lerp_col(Color32::from_rgb(40, 110, 200), Color32::from_rgb(255, 235, 120), (u - 0.5) * 2.0) } else { lerp_col(Color32::from_rgb(8, 10, 20), Color32::from_rgb(40, 110, 200), u * 2.0) };
                let r = egui::Rect::from_min_max(Pos2::new(mr.left() + mr.width() * ix as f32 / nx as f32, mr.top() + mr.height() * (1.0 - (iy + 1) as f32 / ny as f32)), Pos2::new(mr.left() + mr.width() * (ix + 1) as f32 / nx as f32 + 0.5, mr.top() + mr.height() * (1.0 - iy as f32 / ny as f32) + 0.5));
                painter.rect_filled(r, 0.0, colr);
            }
        }
        plot.grid_y(&painter, &|v| format!("{v:.0}°"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        let x = plot.tx(self.pol_f);
        painter.line_segment([Pos2::new(x, mr.top()), Pos2::new(x, mr.bottom())], Stroke::new(1.0, Color32::from_white_alpha(120)));
        if let Some(h) = resp.interact_pointer_pos().filter(|p| mr.contains(*p)) {
            self.pol_f = plot.fx(h.x).clamp(f0, f1);
        }
    }
}

impl Tool for Speaker {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        // the table has had its time: sweep
        if let Some((angle, since)) = self.turning
            && since.elapsed().as_secs_f32() >= self.settle && !cx.sweep.busy() {
                self.turning = None;
                cx.sweep.start();
                self.pending = Some((angle, cx.sweep.result.as_ref().map(|r| r.id).unwrap_or(0)));
            }
        if let Some((angle, old)) = self.pending {
            if let Some(r) = cx.sweep.result.as_ref().filter(|r| r.id != old) {
                let (f, mag, ph) = super::response::curve_of(r);
                let name = format!("{angle:.0}°");
                cx.lib.add(Some(name.clone()), f, mag, ph);
                self.pol.retain(|p| p.0 != name);
                self.pol.push((name, angle));
                self.next_angle = (angle + self.angle_step).min(180.0);
                self.pending = None;
                if self.auto {
                    if angle + self.angle_step <= self.auto_max + 0.01 {
                        self.start_angle(cx, self.next_angle);
                    } else {
                        self.auto = false;
                        if self.turn_on {
                            crate::serial::send(&self.turn_cmd, &[("angle", 0.0)]);
                        }
                    }
                }
            } else if !cx.sweep.busy() {
                (self.pending, self.auto) = (None, false);
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal(|ui| {
            for (i, t) in TABS.iter().enumerate() {
                ui.selectable_value(&mut self.tab, i, crate::lang::tr(t));
            }
        });
        ui.separator();
        match self.tab {
            0 => self.box_ui(ui, cx),
            1 => self.xo_ui(ui, cx),
            2 => self.merge_ui(ui, cx),
            _ => self.polar_ui(ui, cx),
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        let d = &self.drv;
        o.put("tab", self.tab);
        o.put("drv", format!("{} {} {} {} {} {} {}", d.fs, d.qts, d.qes, d.vas_l, d.sd_cm2, d.xmax_mm, d.re));
        o.put("box_z", self.box_z);
        o.put("box", format!("{} {} {} {} {} {}", self.vented as u8, self.vb_l, self.fb, self.ql, self.watts, self.port_d_cm));
        for (k, w) in self.ways.iter().enumerate() {
            o.put(&format!("way{k}"), format!("{} {} {} {} {} {} {} {}", w.on as u8, w.hp, w.hp_f, w.lp, w.lp_f, w.gain, w.delay_ms, w.inv as u8));
        }
        o.put("turn", format!("{} {} {}", self.turn_on as u8, self.settle, self.auto_max));
        o.put("turn_cmd", self.turn_cmd.replace(['\n', '\r'], " "));
        o.put("merge", format!("{} {} {} {}", self.fx, self.width, self.baffle_cm, self.auto_lvl as u8));
    }

    fn load(&mut self, i: &SettingsIn) {
        let nums = |k: &str| -> Vec<f32> { i.str(k).map(|s| s.split_whitespace().filter_map(|x| x.parse::<f32>().ok()).filter(|x| x.is_finite()).collect()).unwrap_or_default() };
        if let Some(t) = i.get::<usize>("tab") {
            self.tab = t.min(TABS.len() - 1);
        }
        i.set("box_z", &mut self.box_z);
        // (files of the first version have no Re)
        if let [fs, qts, qes, vas, sd, xmax, ref rest @ ..] = nums("drv")[..] {
            self.drv = Drv { re: rest.first().copied().unwrap_or(6.0).clamp(0.1, 200.0), fs: fs.clamp(5.0, 2000.0), qts: qts.clamp(0.05, 3.0), qes: qes.clamp(0.05, 5.0), vas_l: vas.clamp(0.05, 2000.0), sd_cm2: sd.clamp(1.0, 3000.0), xmax_mm: xmax.clamp(0.1, 60.0) };
        }
        if let [v, vb, fb, ql, w, pd] = nums("box")[..] {
            (self.vented, self.vb_l, self.fb, self.ql, self.watts, self.port_d_cm) = (v > 0.5, vb.clamp(0.1, 2000.0), fb.clamp(5.0, 500.0), ql.clamp(2.0, 30.0), w.clamp(0.01, 5000.0), pd.clamp(1.0, 40.0));
        }
        for k in 0..3 {
            if let [on, hp, hp_f, lp, lp_f, gain, delay, inv] = nums(&format!("way{k}"))[..] {
                let w = &mut self.ways[k];
                (w.on, w.hp, w.hp_f, w.lp, w.lp_f) = (on > 0.5, (hp as usize).min(XO.len() - 1), hp_f.clamp(10.0, 40000.0), (lp as usize).min(XO.len() - 1), lp_f.clamp(10.0, 40000.0));
                (w.gain, w.delay_ms, w.inv) = (gain.clamp(-40.0, 20.0), delay.clamp(0.0, 50.0), inv > 0.5);
            }
        }
        if let [on, settle, max] = nums("turn")[..] {
            (self.turn_on, self.settle, self.auto_max) = (on > 0.5, settle.clamp(0.0, 120.0), max.clamp(0.0, 180.0));
        }
        if let Some(c) = i.str("turn_cmd").filter(|c| c.len() <= 200) {
            self.turn_cmd = c;
        }
        if let [fx, width, baffle, auto] = nums("merge")[..] {
            (self.fx, self.width, self.baffle_cm, self.auto_lvl) = (fx.clamp(20.0, 5000.0), width.clamp(0.1, 4.0), baffle.clamp(0.0, 300.0), auto > 0.5);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_box_is_a_second_order_high_pass() {
        // Qtc 0.707: -3 dB at Fc
        let d = Drv { fs: 40.0, qts: 0.35, qes: 0.4, vas_l: 60.0, sd_cm2: 130.0, xmax_mm: 5.0, re: 6.0 };
        let vb = d.vas_l / ((0.707f32 / d.qts).powi(2) - 1.0);
        let o = box_sim(&d, false, vb, 0.0, 7.0, 1.0, 20.0);
        let fc = d.fs * (1.0 + d.vas_l / vb).sqrt();
        assert!((o.f3 / fc - 1.0).abs() < 0.04, "{} {}", o.f3, fc);
        // vented: more output at the tuning than the sealed box, and the cone hardly moves there
        let v = box_sim(&d, true, 50.0, 40.0, 7.0, 1.0, 20.0);
        let i = v.f.partition_point(|&x| x < 40.0);
        assert!(v.db[i] > o.db[i] + 2.0);
        assert!(v.x_mm[i] < v.x_mm[i + 30].max(v.x_mm[i - 30]));
        // the impedance: one peak at Fc in the sealed box, a dip near the tuning between two peaks in the vented one
        let k = o.z.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!((o.f[k] / fc - 1.0).abs() < 0.04, "{} {}", o.f[k], fc);
        assert!(v.z[i] < v.z[i + 40] && v.z[i] < v.z[i - 40] && v.z[i] < d.re * 1.6);
    }

    #[test]
    fn linkwitz_riley_sums_flat_and_the_index_of_a_point_source_is_zero() {
        for f in [100.0f32, 1000.0, 2500.0, 9000.0] {
            let s = xo_filter(5, f, 2500.0, true) + xo_filter(5, f, 2500.0, false);
            assert!((s.norm() - 1.0).abs() < 1e-3, "{f}: {}", s.norm());
        }
        assert!(directivity_index(&[(0.0, 0.0), (90.0, 0.0), (180.0, 0.0)]).abs() < 0.01);
        assert!(directivity_index(&[(0.0, 0.0), (30.0, -6.0), (60.0, -20.0), (90.0, -40.0)]) > 6.0);
    }

    /// The version that worked every way out for each of the curves, the sum and the reversed sum, as the reference.
    fn xo_old(items: &[Meas], ways: &[Way; 3], act: &[usize], f: &[f32]) -> (Vec<Vec<f32>>, Vec<C>, Option<Vec<f32>>) {
        let n_items = items.len();
        let way_h = |w: &Way, x: f32, flip: bool| -> Option<C> {
            let m = items.get(w.tr.min(n_items - 1))?;
            let db = m.at(x);
            if !db.is_finite() {
                return None;
            }
            let ph = phase_at(m, x).to_radians() - std::f32::consts::TAU * x * w.delay_ms * 1e-3;
            let h = C::from_polar(10f32.powf((db + w.gain) / 20.0), ph) * xo_filter(w.hp, x, w.hp_f, true) * xo_filter(w.lp, x, w.lp_f, false);
            Some(if w.inv != flip { -h } else { h })
        };
        let db = |c: C| 20.0 * c.norm().max(1e-9).log10();
        let curves = act.iter().map(|&k| f.iter().map(|&x| way_h(&ways[k], x, false).map(db).unwrap_or(f32::NAN)).collect()).collect();
        let sum_of = |flip_k: Option<usize>| -> Vec<C> { f.iter().map(|&x| act.iter().filter_map(|&k| way_h(&ways[k], x, flip_k == Some(k))).sum::<C>()).collect() };
        let sum = sum_of(None);
        let rev = (act.len() >= 2).then(|| sum_of(Some(act[1])).iter().map(|c| db(*c)).collect());
        (curves, sum, rev)
    }

    fn traces() -> Vec<Meas> {
        let mut s = 17u32;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s as f32 / u32::MAX as f32
        };
        // three traces over different ranges (so that some frequencies have no data in some of them), the last one without phase
        [(30.0f32, 5000.0f32, true), (400.0, 20000.0, true), (20.0, 20000.0, false)]
            .iter()
            .map(|&(a, b, ph)| {
                let f = log_grid(a, b, 120);
                Meas { name: String::new(), color: Color32::WHITE, visible: true, mag: f.iter().map(|_| 80.0 + 10.0 * rnd()).collect(), phase: if ph { f.iter().map(|_| 360.0 * rnd() - 180.0).collect() } else { Vec::new() }, f }
            })
            .collect()
    }

    fn test_ways() -> [Way; 3] {
        let w = |tr, hp, hp_f, lp, lp_f, gain, delay_ms, inv| Way { on: true, tr, hp, hp_f, lp, lp_f, gain, delay_ms, inv };
        [w(0, 0, 80.0, 5, 2500.0, 0.0, 0.0, false), w(1, 5, 2500.0, 0, 20000.0, -2.0, 0.3, true), w(2, 2, 300.0, 3, 3000.0, 1.0, 0.1, false)]
    }

    #[test]
    fn the_crossover_worked_out_once_per_way_gives_the_same_curves_as_the_one_worked_out_for_each_curve() {
        let items = traces();
        let ways = test_ways();
        let f = log_grid(20.0, 20000.0, 480);
        for act in [vec![0usize, 1], vec![0, 1, 2], vec![1], vec![0, 2], vec![]] {
            let (c0, s0, r0) = xo_old(&items, &ways, &act, &f);
            let new = xo_response(&items, &ways, &act, &f, true);
            assert_eq!(new.curves.len(), c0.len());
            for (a, b) in new.curves.iter().zip(&c0) {
                for (x, y) in a.iter().zip(b) {
                    assert!((x.is_nan() && y.is_nan()) || (x - y).abs() < 1e-3, "{x} {y}");
                }
            }
            for (a, b) in new.sum.iter().zip(&s0) {
                assert!((a - b).norm() < 1e-4 * (1.0 + b.norm()), "{a} {b}");
            }
            assert_eq!(new.rev.is_some(), r0.is_some());
            if let (Some(a), Some(b)) = (&new.rev, &r0) {
                // (the notch of a reversed sum is where the ways cancel: away from a deep null the levels agree)
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    assert!((x - y).abs() < 0.05 || (*x < 20.0 && *y < 20.0), "{i}: {x} {y}");
                }
            }
            // not asked for: not made
            assert!(xo_response(&items, &ways, &act, &f, false).rev.is_none());
        }
    }

    #[test]
    #[ignore]
    fn bench_crossover_curves() {
        let items = traces();
        let ways = test_ways();
        let f = log_grid(20.0, 20000.0, 480);
        let t = std::time::Instant::now();
        let mut acc = 0.0f32;
        for _ in 0..200 {
            let (c, s, r) = xo_old(&items, &ways, &[0, 1], &f);
            acc += c[0][10] + s[10].re + r.map_or(0.0, |r| r[10]);
        }
        let old = t.elapsed() / 200;
        let t = std::time::Instant::now();
        for _ in 0..200 {
            let o = xo_response(&items, &ways, &[0, 1], &f, false);
            acc += o.curves[0][10] + o.sum[10].re;
        }
        println!("crossover, 2 ways, 480 points, per frame: old {old:?}, new {:?} ({acc})", t.elapsed() / 200);
    }

    #[test]
    fn merge_takes_the_near_field_below_and_the_far_field_above() {
        let f = log_grid(20.0, 20000.0, 200);
        let mk = |db: f32| Meas { name: String::new(), color: Color32::WHITE, visible: true, f: f.clone(), mag: vec![db; f.len()], phase: Vec::new() };
        let (out, off) = merge_nf(&mk(100.0), &mk(80.0), &f, 300.0, 1.0, 0.0, true, 0.0);
        assert!((off + 20.0).abs() < 0.01);
        assert!(out.iter().all(|v| (v - 80.0).abs() < 0.01));
    }
}
