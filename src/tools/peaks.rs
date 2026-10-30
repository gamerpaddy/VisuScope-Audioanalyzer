//! Automatic peak picking (with spectrum labels and note names) and a tuner.

use super::plot::heading;
use super::*;
use crate::app::{fmt_hz, CH_COLORS};
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, RichText, Shape, Stroke};
use std::time::Instant;

#[derive(Clone, Copy)]
pub struct Peak {
    pub f: f32,
    pub db: f32,
}

const LABELS: [&str; 3] = ["frequency", "frequency + level", "note"];

pub struct Peaks {
    n: usize,
    thr: f32,
    prom: f32,
    ch: usize,
    use_avg: bool,
    a4: f32,
    show: bool,
    label: usize,
    list: Vec<Peak>,
    tuner: Option<(f32, f32)>,
    seq: u64,
    last: Instant,
}

/// Local maxima above `thr` dB that stand out `prom` dB from their surroundings, strongest first.
pub fn find_peaks(data: &[f32], df: f32, n_max: usize, thr: f32, prom: f32, fmin: f32, fmax: f32) -> Vec<Peak> {
    if data.len() < 16 || df <= 0.0 {
        return Vec::new();
    }
    let kmin = ((fmin / df).ceil() as usize).max(6);
    let kmax = ((fmax / df) as usize).min(data.len() - 7);
    let mut cand: Vec<(usize, f32)> = Vec::new();
    for k in kmin..=kmax {
        let v = data[k];
        if v > thr && v >= data[k - 1] && v > data[k + 1] {
            let floor = data[k - 6..k].iter().chain(&data[k + 1..k + 7]).fold(f32::MAX, |m, &x| m.min(x));
            if v - floor >= prom {
                cand.push((k, v));
            }
        }
    }
    cand.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<Peak> = Vec::new();
    for (k, _) in cand {
        let (a, b, c) = (data[k - 1], data[k], data[k + 1]);
        let den = a - 2.0 * b + c;
        let p = if den.abs() > 1e-9 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
        let f = (k as f32 + p) * df;
        let db = b - 0.25 * (a - c) * p;
        if out.iter().any(|o| (o.f - f).abs() < (4.0 * df).max(0.004 * f)) {
            continue;
        }
        out.push(Peak { f, db });
        if out.len() >= n_max {
            break;
        }
    }
    out
}

impl Peaks {
    pub fn new() -> Self {
        Self { n: 8, thr: -90.0, prom: 12.0, ch: 0, use_avg: true, a4: 440.0, show: true, label: 1, list: Vec::new(), tuner: None, seq: 0, last: Instant::now() }
    }
}

impl Tool for Peaks {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        // also runs while the window is closed as long as labels are drawn on the spectrum
        if !(visible || self.show) || cx.an.seq == self.seq || self.last.elapsed().as_millis() < 60 || cx.an.n == 0 {
            return;
        }
        self.seq = cx.an.seq;
        self.last = Instant::now();
        let data = if self.use_avg { &cx.an.avg[self.ch] } else { &cx.an.live[self.ch] };
        self.list = find_peaks(data, cx.an.sr / cx.an.n as f32, self.n, self.thr, self.prom, 20.0, 20000.0);
        // tuner follows the strongest peak in the musical range
        let best = self.list.iter().filter(|p| (30.0..5000.0).contains(&p.f)).max_by(|a, b| a.db.total_cmp(&b.db));
        self.tuner = match (best, self.tuner) {
            (Some(b), Some((f, _))) if (b.f / f - 1.0).abs() < 0.03 => Some((f + (b.f - f) * 0.35, b.db)),
            (Some(b), _) => Some((b.f, b.db)),
            (None, _) => None,
        };
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.ch, 0, RichText::new("L").color(CH_COLORS[0]));
            ui.selectable_value(&mut self.ch, 1, RichText::new("R").color(CH_COLORS[1]));
            ui.checkbox(&mut self.use_avg, "averaged").on_hover_text("Use the averaged spectrum (steadier) instead of the live one");
            ui.add(egui::DragValue::new(&mut self.n).range(1..=32).prefix("max "));
            ui.add(egui::DragValue::new(&mut self.thr).range(-200.0..=0.0).speed(0.5).prefix("min ").suffix(" dB"));
            ui.add(egui::DragValue::new(&mut self.prom).range(1.0..=60.0).speed(0.2).prefix("prominence ").suffix(" dB"));
            ui.add(egui::DragValue::new(&mut self.a4).range(400.0..=480.0).speed(0.1).prefix("A4 ").suffix(" Hz"));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show, "Mark on spectrum");
            egui::ComboBox::from_id_salt("pk_label").width(130.0).selected_text(LABELS[self.label]).show_ui(ui, |ui| {
                for (i, l) in LABELS.iter().enumerate() {
                    ui.selectable_value(&mut self.label, i, *l);
                }
            });
        });
        ui.separator();
        // tuner
        heading(ui, "Tuner");
        match self.tuner {
            Some((f, db)) => {
                let (note, cents) = note_of(f, self.a4);
                let col = if cents.abs() < 5.0 { Color32::from_rgb(90, 230, 130) } else if cents.abs() < 20.0 { Color32::from_rgb(240, 200, 80) } else { Color32::from_rgb(240, 110, 90) };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&note).size(34.0).strong().color(col));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{cents:+.0} cents")).monospace().color(col));
                        ui.label(RichText::new(format!("{}  {:.1} {}", fmt_hz(f), db + cx.unit_off[self.ch], cx.unit_name)).small().weak());
                    });
                });
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(360.0), 22.0), egui::Sense::hover());
                let p = ui.painter();
                p.rect_filled(rect, 3.0, Color32::from_gray(24));
                for c in (-50..=50).step_by(10) {
                    let x = rect.center().x + c as f32 / 50.0 * rect.width() * 0.5;
                    p.line_segment([Pos2::new(x, rect.bottom() - if c % 50 == 0 || c == 0 { 10.0 } else { 5.0 }), Pos2::new(x, rect.bottom())], Stroke::new(1.0, Color32::from_gray(90)));
                }
                let x = rect.center().x + (cents / 50.0).clamp(-1.0, 1.0) * rect.width() * 0.5;
                p.add(Shape::convex_polygon(vec![Pos2::new(x - 5.0, rect.top()), Pos2::new(x + 5.0, rect.top()), Pos2::new(x, rect.bottom() - 6.0)], col, Stroke::NONE));
            }
            None => {
                ui.label(RichText::new("no stable pitch").weak());
            }
        }
        ui.separator();
        let top = self.list.iter().max_by(|a, b| a.db.total_cmp(&b.db)).map(|p| p.f);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("peaks_grid").num_columns(6).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                for h in ["#", "Frequency", "Level", "Note", "Cents", "vs. top"] {
                    ui.label(RichText::new(h).small().weak());
                }
                ui.end_row();
                let mut by_f = self.list.clone();
                by_f.sort_by(|a, b| a.f.total_cmp(&b.f));
                for (i, p) in by_f.iter().enumerate() {
                    let (note, cents) = note_of(p.f, self.a4);
                    ui.label(format!("{}", i + 1));
                    ui.label(RichText::new(fmt_hz(p.f)).monospace());
                    ui.label(RichText::new(format!("{:.1} {}", p.db + cx.unit_off[self.ch], cx.unit_name)).monospace());
                    ui.label(note);
                    ui.label(RichText::new(format!("{cents:+.0}")).monospace());
                    let rel = top.map(|t| p.f / t).unwrap_or(1.0);
                    ui.label(RichText::new(if (rel - 1.0).abs() < 0.002 { "fundamental".into() } else if (rel - rel.round()).abs() < 0.01 && rel > 1.0 { format!("H{}", rel.round() as i32) } else { format!("x{rel:.3}") }).small());
                    ui.end_row();
                }
            });
        });
    }

    fn overlay(&self, ov: &SpecOverlay, cx: &Ctx) {
        if !self.show || self.list.is_empty() {
            return;
        }
        let off = cx.unit_off[self.ch];
        let mut by_f = self.list.clone();
        by_f.sort_by(|a, b| a.f.total_cmp(&b.f));
        let mut last_x = f32::MIN;
        for p in by_f {
            let x = (ov.x_of_f)(p.f);
            if x < ov.plot.left() || x > ov.plot.right() {
                continue;
            }
            let y = (ov.y_of_db)(p.db + off).clamp(ov.plot.top() + 14.0, ov.plot.bottom() - 4.0);
            let col = Color32::from_rgb(255, 240, 140);
            ov.p.add(Shape::convex_polygon(vec![Pos2::new(x - 3.5, y - 9.0), Pos2::new(x + 3.5, y - 9.0), Pos2::new(x, y - 3.0)], col, Stroke::NONE));
            if x - last_x < 46.0 {
                continue;
            }
            last_x = x;
            let text = match self.label {
                0 => crate::app::fmt_hz_short(p.f),
                1 => format!("{}  {:.1}", crate::app::fmt_hz_short(p.f), p.db + off),
                _ => {
                    let (n, c) = note_of(p.f, self.a4);
                    format!("{n} {c:+.0}c")
                }
            };
            let g = ov.p.layout_no_wrap(text, FontId::proportional(10.0), col);
            let r = Align2::CENTER_BOTTOM.anchor_size(Pos2::new(x, y - 10.0), g.size());
            ov.p.rect_filled(r.expand(1.5), 2.0, Color32::from_black_alpha(160));
            ov.p.galley(r.min, g, col);
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("n", self.n);
        o.put("thr", self.thr);
        o.put("prom", self.prom);
        o.put("ch", self.ch);
        o.put("avg", self.use_avg);
        o.put("a4", self.a4);
        o.put("show", self.show);
        o.put("label", self.label);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("n", &mut self.n);
        i.set("thr", &mut self.thr);
        i.set("prom", &mut self.prom);
        i.set("ch", &mut self.ch);
        i.set("avg", &mut self.use_avg);
        i.set("a4", &mut self.a4);
        i.set("show", &mut self.show);
        i.set("label", &mut self.label);
        self.n = self.n.clamp(1, 32);
        self.ch = self.ch.min(1);
        self.label = self.label.min(2);
        self.a4 = self.a4.clamp(400.0, 480.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_refines_peaks() {
        // synthetic spectrum with two peaks, the first between bins
        let df = 10.0;
        let mut d = vec![-100.0f32; 2000];
        let shape = |x: f32| -40.0 * x * x; // parabola in dB
        for k in 0..2000 {
            let f = k as f32 * df;
            d[k] = d[k].max(-20.0 + shape((f - 1003.0) / df)).max(-30.0 + shape((f - 5000.0) / df));
        }
        let p = find_peaks(&d, df, 5, -80.0, 10.0, 20.0, 20000.0);
        assert_eq!(p.len(), 2, "{} peaks", p.len());
        assert!((p[0].f - 1003.0).abs() < 1.0, "f {}", p[0].f);
        assert!((p[0].db + 20.0).abs() < 0.2);
        assert!((p[1].f - 5000.0).abs() < 1.0);
    }
}
