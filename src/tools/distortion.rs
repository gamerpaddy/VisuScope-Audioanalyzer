//! Distortion sweeps: THD / THD+N versus frequency or level and IMD versus level (stepping the
//! generator automatically), plus the harmonic distortion of one exponential sweep (Farina).

use super::plot::{canvas, inset, Plot};
use super::sweep::{State, HARM_K};
use super::*;
use crate::tonegen::Wave;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};
use std::time::Instant;

const MODES: [&str; 4] = ["THD vs frequency", "THD vs level", "IMD vs level", "Swept harmonics"];
const C_THDN: Color32 = Color32::from_rgb(255, 170, 70);
const C_THD: Color32 = Color32::from_rgb(90, 200, 255);
const C_H2: Color32 = Color32::from_rgb(120, 230, 150);
const C_H3: Color32 = Color32::from_rgb(230, 130, 230);
const C_IMD: Color32 = Color32::from_rgb(255, 100, 160);
const HARM_COLS: [Color32; 5] = [C_H2, C_H3, Color32::from_rgb(255, 214, 10), Color32::from_rgb(160, 220, 80), Color32::from_rgb(190, 190, 200)];

#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    thd: f32,
    thdn: f32,
    h2: f32,
    h3: f32,
    imd: f32,
    fund: f32,
}

#[derive(Default)]
struct Acc {
    n: u32,
    thd: f64,
    thdn: f64,
    h2: f64,
    h3: f64,
    imd: f64,
    fund: f64,
}

enum Phase {
    Idle,
    Start,
    Settle(Instant),
    Average(Instant, Acc),
}

struct Saved {
    wave: Wave,
    freq: f32,
    level: f32,
    on: bool,
}

pub struct Distortion {
    mode: usize,
    // frequency sweep
    f_lo: f32,
    f_hi: f32,
    points: usize,
    level_db: f32,
    // level sweep
    freq: f32,
    l_lo: f32,
    l_hi: f32,
    l_step: f32,
    smpte: bool,
    settle: f32,
    avg_s: f32,
    y_lo: f32,
    y_hi: f32,
    show: [bool; 4],
    // run state
    phase: Phase,
    plan: Vec<f32>,
    idx: usize,
    pts: Vec<Pt>,
    saved: Option<Saved>,
    last_seq: u64,
    save: Option<dialogs::Pending>,
    note: String,
}

fn db_of(lin: f64) -> f32 {
    (10.0 * lin.max(1e-30).log10()) as f32
}

fn pct(db: f32) -> String {
    let p = 100.0 * 10f32.powf(db / 20.0);
    if p >= 0.1 { format!("{p:.3} %") } else { format!("{p:.5} %") }
}

/// Peak level (dBFS) of a tone near `f` in the display spectrum.
fn tone_db(an: &Analysis, ch: usize, f: f32) -> Option<f32> {
    let d = &an.live[ch];
    if an.n == 0 || d.len() < 8 {
        return None;
    }
    let k = (f / (an.sr / an.n as f32)).round() as isize;
    if k < 3 || k as usize + 3 >= d.len() {
        return None;
    }
    let (mut kb, mut vb) = (k as usize, f32::MIN);
    for j in (k - 2) as usize..=(k + 2) as usize {
        if d[j] > vb {
            vb = d[j];
            kb = j;
        }
    }
    Some(crate::dsp::corrected_peak_db(d, kb, &an.peak_tab).unwrap_or(vb))
}

/// Largest value within one bin of `f` (dBFS).
fn bin_db(an: &Analysis, ch: usize, f: f32) -> f32 {
    let d = &an.live[ch];
    let k = (f / (an.sr / an.n as f32)).round() as isize;
    if k < 1 || k as usize + 1 >= d.len() {
        return -200.0;
    }
    let k = k as usize;
    d[k - 1].max(d[k]).max(d[k + 1])
}

/// Twin-tone IMD in dB re the test signal, and the level of the louder tone. SMPTE: sidebands
/// f2 +- n f1 (n = 1..4) over f2; CCIF: f2 - f1, 2 f1 - f2, 2 f2 - f1 over (A1 + A2).
pub fn imd(an: &Analysis, ch: usize, smpte: bool) -> Option<(f32, f32)> {
    let (f1, f2) = if smpte { (60.0, 7000.0) } else { (19000.0, 20000.0) };
    let (l1, l2) = (tone_db(an, ch, f1)?, tone_db(an, ch, f2)?);
    if l1.max(l2) < -100.0 {
        return None;
    }
    let lin = |d: f32| 10f32.powf(d / 20.0);
    let nyq = an.sr * 0.5;
    let (mut sum, den) = (0f32, if smpte { lin(l2) } else { lin(l1) + lin(l2) });
    let mut add = |f: f32| {
        if f > 0.0 && f < nyq * 0.99 {
            sum += lin(bin_db(an, ch, f)).powi(2);
        }
    };
    if smpte {
        for n in 1..=4 {
            add(f2 - n as f32 * f1);
            add(f2 + n as f32 * f1);
        }
    } else {
        add(f2 - f1);
        add(2.0 * f1 - f2);
        add(2.0 * f2 - f1);
    }
    Some((20.0 * (sum.sqrt() / den.max(1e-12)).max(1e-10).log10(), l1.max(l2)))
}

impl Distortion {
    pub fn new() -> Self {
        Self {
            mode: 0,
            f_lo: 20.0,
            f_hi: 20000.0,
            points: 24,
            level_db: -3.0,
            freq: 1000.0,
            l_lo: -60.0,
            l_hi: -3.0,
            l_step: 3.0,
            smpte: true,
            settle: 0.5,
            avg_s: 0.8,
            y_lo: -130.0,
            y_hi: 0.0,
            show: [true, true, false, false],
            phase: Phase::Idle,
            plan: Vec::new(),
            idx: 0,
            pts: Vec::new(),
            saved: None,
            last_seq: 0,
            save: None,
            note: String::new(),
        }
    }

    fn running(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    fn make_plan(&self) -> Vec<f32> {
        match self.mode {
            0 => (0..self.points).map(|i| self.f_lo * (self.f_hi / self.f_lo).powf(i as f32 / (self.points - 1).max(1) as f32)).collect(),
            _ => {
                let n = (((self.l_hi - self.l_lo) / self.l_step.max(0.5)).round() as usize + 1).clamp(2, 200);
                (0..n).map(|i| self.l_lo + (self.l_hi - self.l_lo) * i as f32 / (n - 1) as f32).collect()
            }
        }
    }

    fn start(&mut self, cx: &mut Ctx) {
        if cx.sr <= 0.0 || cx.sh.sr.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            self.note = "No input running".into();
            return;
        }
        self.plan = self.make_plan();
        self.pts.clear();
        self.idx = 0;
        self.note.clear();
        self.saved = Some(Saved { wave: cx.tg.wave, freq: cx.tg.freq, level: cx.tg.level_db[0], on: cx.tg.on });
        self.last_seq = 0;
        self.phase = Phase::Start;
    }

    fn finish(&mut self, cx: &mut Ctx, aborted: bool) {
        if let Some(s) = self.saved.take() {
            cx.req.gen_cmd = Some(GenCmd { wave: Some(s.wave), freq: Some(s.freq), level_db: Some(s.level), on: Some(s.on) });
        }
        self.phase = Phase::Idle;
        if aborted {
            self.note = "stopped".into();
        }
    }

    fn gen_for(&self, x: f32) -> GenCmd {
        match self.mode {
            0 => GenCmd { wave: Some(Wave::Sine), freq: Some(x), level_db: Some(self.level_db), on: Some(true) },
            1 => GenCmd { wave: Some(Wave::Sine), freq: Some(self.freq), level_db: Some(x), on: Some(true) },
            _ => GenCmd { wave: Some(if self.smpte { Wave::Smpte } else { Wave::Ccif }), freq: None, level_db: Some(x), on: Some(true) },
        }
    }

    fn step(&mut self, cx: &mut Ctx) {
        let ch = cx.meas_ch;
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => {}
            Phase::Start => {
                cx.req.gen_cmd = Some(self.gen_for(self.plan[self.idx]));
                self.last_seq = cx.an.seq;
                self.phase = Phase::Settle(Instant::now());
            }
            Phase::Settle(t0) => {
                // the measurement window has to be refilled with the new signal
                let need = cx.an.meas_n as f32 / cx.sr.max(1.0) + 0.3 + self.settle;
                self.phase = if t0.elapsed().as_secs_f32() >= need { Phase::Average(Instant::now(), Acc::default()) } else { Phase::Settle(t0) };
            }
            Phase::Average(t0, mut acc) => {
                if cx.an.seq != self.last_seq {
                    self.last_seq = cx.an.seq;
                    let x = self.plan[self.idx];
                    let m = cx.an.metrics[ch];
                    let lin = |db: f32| 10f64.powf(db as f64 / 10.0);
                    if self.mode == 2 {
                        if let Some((d, l)) = imd(cx.an, ch, self.smpte) {
                            acc.n += 1;
                            acc.imd += lin(d);
                            acc.fund += lin(l);
                        }
                    } else if m.valid && (self.mode != 0 || (m.fund_hz - x).abs() < x * 0.05) {
                        acc.n += 1;
                        acc.thd += lin(m.thd_db);
                        acc.thdn += lin(m.thdn_db);
                        acc.h2 += lin(m.harm_db[1]);
                        acc.h3 += lin(m.harm_db[2]);
                        acc.fund += lin(m.fund_dbfs);
                    }
                }
                let el = t0.elapsed().as_secs_f32();
                if (el >= self.avg_s && acc.n >= 3) || el >= self.avg_s + 5.0 {
                    let n = acc.n.max(1) as f64;
                    let ok = acc.n >= 3;
                    let v = |s: f64| if ok { db_of(s / n) } else { f32::NAN };
                    self.pts.push(Pt { x: self.plan[self.idx], thd: v(acc.thd), thdn: v(acc.thdn), h2: v(acc.h2), h3: v(acc.h3), imd: v(acc.imd), fund: v(acc.fund) });
                    self.idx += 1;
                    if self.idx >= self.plan.len() {
                        self.finish(cx, false);
                        self.note = format!("{} points", self.pts.len());
                    } else {
                        self.phase = Phase::Start;
                    }
                } else {
                    self.phase = Phase::Average(t0, acc);
                }
            }
        }
    }

    fn csv(&self, cx: &Ctx) -> String {
        let mut s = String::new();
        if self.mode == 3 {
            if let Some(h) = cx.sweep.result.as_ref().and_then(|r| r.harm.as_ref()) {
                s += "freq_hz,fund_db,thd_db,h2_db,h3_db,h4_db,h5_db,h6_db\n";
                for i in 0..h.f.len() {
                    s += &format!("{:.2},{:.2},{:.2}", h.f[i], h.h1_db[i], h.thd_db[i]);
                    for k in 0..HARM_K - 1 {
                        s += &format!(",{:.2}", h.hd_db[k][i]);
                    }
                    s.push('\n');
                }
            }
            return s;
        }
        s += if self.mode == 0 { "freq_hz" } else { "level_dbfs" };
        s += ",thd_db,thdn_db,h2_db,h3_db,imd_db,level_db\n";
        for p in &self.pts {
            s += &format!("{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}\n", p.x, p.thd, p.thdn, p.h2, p.h3, p.imd, p.fund);
        }
        s
    }

    fn controls(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.running(), |ui| {
                for (i, n) in MODES.iter().enumerate() {
                    ui.selectable_value(&mut self.mode, i, *n);
                }
            });
        });
        let busy = self.running();
        if self.mode == 3 {
            let sw = &mut *cx.sweep;
            let sb = sw.busy();
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new(if sb { "Stop" } else { "Measure" }).strong()).min_size(egui::vec2(96.0, 26.0))).clicked() {
                    if sb {
                        sw.cancel();
                    } else {
                        // a tone left on would add to the sweep
                        cx.req.gen_cmd = Some(GenCmd { on: Some(false), ..Default::default() });
                        sw.start();
                    }
                }
                if sb {
                    ui.add(egui::ProgressBar::new(sw.progress()).desired_width(120.0).show_percentage());
                }
                ui.add_enabled_ui(!sb, |ui| {
                    ui.add(egui::DragValue::new(&mut sw.p.amp_db).range(-60.0..=0.0).speed(0.2).suffix(" dBFS"));
                    ui.add(egui::DragValue::new(&mut sw.p.secs).range(1.0..=12.0).speed(0.1).suffix(" s")).on_hover_text("Longer sweeps separate the harmonics better and lower the noise");
                });
                if let State::Failed(e) = &sw.state {
                    ui.colored_label(Color32::from_rgb(255, 130, 100), e);
                }
            });
            ui.label(RichText::new("One sweep gives every harmonic: each distortion product arrives before the main impulse and is read at k times the fundamental. Shares the sweep of the Response window.").small().weak());
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if self.mode == 0 {
                    ui.add(egui::DragValue::new(&mut self.f_lo).range(10.0..=2000.0).speed(1.0).suffix(" Hz"));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut self.f_hi).range(1000.0..=96000.0).speed(10.0).suffix(" Hz"));
                    ui.add(egui::DragValue::new(&mut self.points).range(4..=80).suffix(" points"));
                    ui.add(egui::DragValue::new(&mut self.level_db).range(-60.0..=0.0).speed(0.2).suffix(" dBFS"));
                } else {
                    if self.mode == 1 {
                        ui.add(egui::DragValue::new(&mut self.freq).range(10.0..=20000.0).speed(5.0).suffix(" Hz"));
                    } else {
                        ui.selectable_value(&mut self.smpte, true, "SMPTE");
                        ui.selectable_value(&mut self.smpte, false, "CCIF");
                    }
                    ui.add(egui::DragValue::new(&mut self.l_lo).range(-100.0..=-1.0).speed(0.5).suffix(" dBFS"));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut self.l_hi).range(-90.0..=0.0).speed(0.5).suffix(" dBFS"));
                    ui.add(egui::DragValue::new(&mut self.l_step).range(0.5..=12.0).speed(0.1).prefix("step ").suffix(" dB"));
                }
                ui.add(egui::DragValue::new(&mut self.settle).range(0.0..=5.0).speed(0.05).prefix("settle +").suffix(" s")).on_hover_text("Extra wait after each change, on top of refilling the measurement window");
            });
        });
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new(if busy { "Stop" } else { "Start" }).strong()).min_size(egui::vec2(96.0, 26.0))).clicked() {
                if busy {
                    self.finish(cx, true);
                } else {
                    self.start(cx);
                }
            }
            if busy {
                ui.add(egui::ProgressBar::new(self.idx as f32 / self.plan.len().max(1) as f32).desired_width(140.0).text(format!("{} / {}", self.idx + 1, self.plan.len())));
            }
            if !self.note.is_empty() {
                ui.label(RichText::new(&self.note).small().weak());
            }
        });
        if !busy && self.pts.is_empty() {
            ui.label(RichText::new("Needs the generator output fed back into the input (cable or [Loopback] input). The generator is switched and restored automatically.").small().weak());
        }
    }
}

impl Tool for Distortion {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        if let Some(d) = &self.save {
            if let Some(res) = d.take() {
                self.save = None;
                if let Some(path) = res {
                    cx.req.note = Some(match std::fs::write(&path, self.csv(cx)) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
        if !visible && self.running() {
            self.finish(cx, true);
        }
        if self.running() {
            self.step(cx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        self.controls(ui, cx);
        let swept = self.mode == 3;
        ui.horizontal_wrapped(|ui| {
            if !swept {
                ui.checkbox(&mut self.show[0], RichText::new("THD+N").color(C_THDN));
                ui.checkbox(&mut self.show[1], RichText::new("THD").color(C_THD));
                ui.checkbox(&mut self.show[2], RichText::new("H2").color(C_H2));
                ui.checkbox(&mut self.show[3], RichText::new("H3").color(C_H3));
            }
            ui.add(egui::DragValue::new(&mut self.y_lo).range(-200.0..=self.y_hi - 10.0).speed(0.5).prefix("min "));
            ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 10.0..=40.0).speed(0.5).prefix("max "));
            if ui.add_enabled(self.save.is_none(), egui::Button::new("CSV")).clicked() {
                self.save = Some(dialogs::save("Export distortion sweep", "CSV", &["csv"], "distortion.csv".into()));
            }
        });

        // the curves to draw: (x, [(name, colour, values)])
        let mut xs: Vec<f32> = Vec::new();
        let mut curves: Vec<(String, Color32, Vec<f32>)> = Vec::new();
        if swept {
            if let Some(h) = cx.sweep.result.as_ref().and_then(|r| r.harm.as_ref()) {
                xs = h.f.clone();
                curves.push(("THD".into(), C_THD, h.thd_db.clone()));
                for k in 0..HARM_K - 1 {
                    curves.push((format!("H{}", k + 2), HARM_COLS[k], h.hd_db[k].clone()));
                }
            }
        } else {
            xs = self.pts.iter().map(|p| p.x).collect();
            let col = |f: fn(&Pt) -> f32| self.pts.iter().map(f).collect::<Vec<f32>>();
            if self.mode == 2 {
                curves.push(("IMD".into(), C_IMD, col(|p| p.imd)));
            } else {
                if self.show[0] {
                    curves.push(("THD+N".into(), C_THDN, col(|p| p.thdn)));
                }
                if self.show[1] {
                    curves.push(("THD".into(), C_THD, col(|p| p.thd)));
                }
                if self.show[2] {
                    curves.push(("H2".into(), C_H2, col(|p| p.h2)));
                }
                if self.show[3] {
                    curves.push(("H3".into(), C_H3, col(|p| p.h3)));
                }
            }
        }
        // stored traces (frequency modes): compare devices or settings
        let freq_mode = self.mode == 0 || swept;
        let (layers, rel) = if freq_mode {
            let store = if xs.len() > 1 { curves.first().map(|c| (xs.clone(), c.2.clone(), Vec::new())) } else { None };
            cx.lib.ui(ui, store);
            if xs.len() > 1 { cx.lib.layers(&xs, 1.0, 0.0) } else { (Vec::new(), None) }
        } else {
            (Vec::new(), None)
        };
        if let (Some(r), Some(first)) = (&rel, curves.first_mut()) {
            for (v, d) in first.2.iter_mut().zip(r) {
                *v -= d;
            }
        }

        let (resp, painter, rect) = canvas(ui, 140.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (x0, x1, xlog) = match self.mode {
            0 => (self.f_lo, self.f_hi, true),
            3 => (cx.sweep.result.as_ref().map(|r| r.params.f0).unwrap_or(20.0), cx.sweep.result.as_ref().map(|r| (r.params.f1 / 2.0).min(r.sr * 0.49)).unwrap_or(10000.0), true),
            _ => (self.l_lo - 1.0, self.l_hi + 1.0, false),
        };
        let plot = Plot::new(pr, x0, x1.max(x0 * 1.5 + 1.0), self.y_lo, self.y_hi, xlog);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        if xlog {
            plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        } else {
            plot.grid_x(&painter, &|v| format!("{v:.0}"));
        }
        plot.frame(&painter);
        for (_, col, v) in &layers {
            plot.line(&painter, xs.iter().copied().zip(v.iter().copied()), Stroke::new(1.2, *col));
        }
        for (i, (name, col, v)) in curves.iter().enumerate() {
            plot.line(&painter, xs.iter().copied().zip(v.iter().copied()), Stroke::new(if i == 0 { 1.8 } else { 1.2 }, *col));
            if !swept {
                for (x, y) in xs.iter().zip(v) {
                    if y.is_finite() {
                        painter.circle_filled(Pos2::new(plot.tx(*x), plot.ty(*y).clamp(pr.top(), pr.bottom())), 2.2, *col);
                    }
                }
            }
            painter.text(Pos2::new(pr.left() + 6.0 + i as f32 * 50.0, pr.top() + 4.0), Align2::LEFT_TOP, name, FontId::proportional(10.0), *col);
        }
        for (i, (name, col, _)) in layers.iter().enumerate() {
            painter.text(Pos2::new(pr.right() - 6.0, pr.bottom() - 6.0 - i as f32 * 12.0), Align2::RIGHT_BOTTOM, name, FontId::proportional(10.0), *col);
        }
        let xl = match self.mode {
            0 | 3 => "Hz",
            _ => "dBFS",
        };
        painter.text(Pos2::new(pr.right(), rect.bottom() - 1.0), Align2::RIGHT_BOTTOM, xl, FontId::proportional(9.0), crate::views::TXT);
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
        if xs.is_empty() {
            painter.text(pr.center(), Align2::CENTER_CENTER, if swept { "press Measure" } else { "press Start" }, FontId::proportional(13.0), crate::views::TXT);
        }
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)).filter(|_| !xs.is_empty()) {
            let x = plot.fx(h.x);
            let i = xs.iter().enumerate().min_by(|a, b| (plot.tx(*a.1) - plot.tx(x)).abs().total_cmp(&(plot.tx(*b.1) - plot.tx(x)).abs())).map(|a| a.0).unwrap_or(0);
            let mut txt = if xlog { crate::app::fmt_hz(xs[i]) } else { format!("{:.1} dBFS", xs[i]) };
            for (name, _, v) in &curves {
                if v[i].is_finite() {
                    txt += &format!("   {name} {:.1} dB ({})", v[i], pct(v[i]));
                }
            }
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 18.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn report(&self, cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        let mut series: Vec<Series> = Vec::new();
        let (title, xlog, xlabel, x);
        if self.mode == 3 {
            let Some(h) = cx.sweep.result.as_ref().and_then(|r| r.harm.as_ref()) else { return Vec::new() };
            let mk = |name: String, col: usize, v: &[f32]| Series { name, color: COLORS[col], pts: h.f.iter().copied().zip(v.iter().copied()).collect() };
            series.push(mk("THD".into(), 0, &h.thd_db));
            for k in 0..HARM_K - 1 {
                series.push(mk(format!("H{}", k + 2), (k + 1) % COLORS.len(), &h.hd_db[k]));
            }
            (title, xlog, xlabel, x) = ("Swept harmonic distortion (relative to the fundamental)".to_string(), true, "Hz", (h.f[0], *h.f.last().unwrap()));
        } else {
            if self.pts.is_empty() {
                return Vec::new();
            }
            let mk = |name: &str, col: usize, f: fn(&Pt) -> f32| Series { name: name.into(), color: COLORS[col], pts: self.pts.iter().map(|p| (p.x, f(p))).collect() };
            if self.mode == 2 {
                series.push(mk("IMD", 4, |p| p.imd));
            } else {
                series.push(mk("THD+N", 1, |p| p.thdn));
                series.push(mk("THD", 0, |p| p.thd));
                series.push(mk("H2", 2, |p| p.h2));
                series.push(mk("H3", 3, |p| p.h3));
            }
            let (a, b) = (self.pts.iter().map(|p| p.x).fold(f32::MAX, f32::min), self.pts.iter().map(|p| p.x).fold(f32::MIN, f32::max));
            (title, xlog, xlabel, x) = (MODES[self.mode].to_string(), self.mode == 0, if self.mode == 0 { "Hz" } else { "dBFS" }, (a, b.max(a + 1.0)));
        }
        let (y0, y1) = auto_range(&series, 10.0, 30.0);
        vec![Section { id: "Distortion", title: "Distortion".into(), blocks: vec![Block::Plot(PlotSpec { title, xlog, xlabel: xlabel.into(), ylabel: "dB".into(), x, y: (y0, y1), series })] }]
    }

    fn status(&self) -> Option<(String, Color32)> {
        self.running().then(|| (format!("distortion sweep {}/{}", self.idx + 1, self.plan.len()), Color32::from_rgb(255, 190, 80)))
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("mode", self.mode);
        o.put("f_lo", self.f_lo);
        o.put("f_hi", self.f_hi);
        o.put("points", self.points);
        o.put("level_db", self.level_db);
        o.put("freq", self.freq);
        o.put("l_lo", self.l_lo);
        o.put("l_hi", self.l_hi);
        o.put("l_step", self.l_step);
        o.put("smpte", self.smpte);
        o.put("settle", self.settle);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("mode") {
            self.mode = v.min(MODES.len() - 1);
        }
        i.set("f_lo", &mut self.f_lo);
        i.set("f_hi", &mut self.f_hi);
        i.set("points", &mut self.points);
        i.set("level_db", &mut self.level_db);
        i.set("freq", &mut self.freq);
        i.set("l_lo", &mut self.l_lo);
        i.set("l_hi", &mut self.l_hi);
        i.set("l_step", &mut self.l_step);
        i.set("smpte", &mut self.smpte);
        i.set("settle", &mut self.settle);
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        self.f_lo = self.f_lo.clamp(10.0, 2000.0);
        self.f_hi = self.f_hi.clamp(self.f_lo * 2.0, 96000.0);
        self.points = self.points.clamp(4, 80);
        self.level_db = self.level_db.clamp(-60.0, 0.0);
        self.freq = self.freq.clamp(10.0, 20000.0);
        self.l_lo = self.l_lo.clamp(-100.0, -1.0);
        self.l_hi = self.l_hi.clamp(self.l_lo + 1.0, 0.0);
        self.l_step = self.l_step.clamp(0.5, 12.0);
        self.settle = self.settle.clamp(0.0, 5.0);
        self.y_hi = self.y_hi.clamp(-50.0, 40.0);
        self.y_lo = self.y_lo.clamp(-200.0, self.y_hi - 10.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smpte_and_ccif_imd() {
        // sr = n so that bin k is k Hz
        let mut an = Analysis { sr: 16384.0, n: 16384, ..Default::default() };
        let mut d = vec![-140.0f32; 8193];
        let db = |a: f32| 20.0 * a.log10();
        d[60] = db(0.8);
        d[7000] = db(0.2);
        d[7000 - 60] = db(0.002);
        d[7000 + 60] = db(0.002);
        an.live[0] = d.clone();
        let (v, lvl) = imd(&an, 0, true).unwrap();
        assert!((v - 20.0 * (2f32.sqrt() * 0.002 / 0.2).log10()).abs() < 0.1, "SMPTE {v}");
        assert!((lvl - db(0.8)).abs() < 0.1);
        // CCIF: 19k + 20k, difference tone 1k and 2f1-f2 = 18k
        let mut d = vec![-140.0f32; 8193];
        // sr/2 = 8192 so use a lower pair with the same structure
        let _ = &mut d;
        let mut an2 = Analysis { sr: 65536.0, n: 65536, ..Default::default() };
        let mut d2 = vec![-140.0f32; 32769];
        d2[19000] = db(0.5);
        d2[20000] = db(0.5);
        d2[1000] = db(0.001);
        d2[18000] = db(0.001);
        an2.live[0] = d2;
        let (v, _) = imd(&an2, 0, false).unwrap();
        assert!((v - 20.0 * (2f32.sqrt() * 0.001 / 1.0).log10()).abs() < 0.1, "CCIF {v}");
    }
}
