//! Fractional-octave band analyser (1/1 ... 1/24) with A/C weighting, time weighting and a target tilt.

use super::plot::{canvas, lerp_col, Plot};
use super::*;
use crate::app::fmt_hz_short;
use eframe::egui::{self, vec2, Align2, Color32, Pos2, Rect, RichText, Stroke};
use std::time::Instant;

const FRACTIONS: [(usize, &str); 5] = [(1, "1/1"), (3, "1/3"), (6, "1/6"), (12, "1/12"), (24, "1/24")];
const TIMES: [&str; 4] = ["Live", "Fast 125 ms", "Slow 1 s", "Average"];
const WEIGHTS: [&str; 3] = ["Z (flat)", "A", "C"];

pub struct Octave {
    frac: usize,
    weight: usize,
    time: usize,
    ch: usize,
    hold: bool,
    target: bool,
    tilt: f32,
    target_db: f32,
    deviation: bool,
    y_lo: f32,
    y_hi: f32,
    // computed
    bands: Vec<Band>,
    key: (usize, u32, usize),
    pow: Vec<f32>,
    level: Vec<f32>,
    peak: Vec<f32>,
    total: f32,
    seq: u64,
    last: Instant,
}

#[derive(Clone, Copy)]
struct Band {
    fc: f32,
    lo: f32,
    hi: f32,
    w_db: [f32; 3],
}

fn a_weight(f: f32) -> f32 {
    let f2 = f * f;
    let ra = 12194f32.powi(2) * f2 * f2 / ((f2 + 20.6f32.powi(2)) * ((f2 + 107.7f32.powi(2)) * (f2 + 737.9f32.powi(2))).sqrt() * (f2 + 12194f32.powi(2)));
    20.0 * ra.log10() + 2.0
}

fn c_weight(f: f32) -> f32 {
    let f2 = f * f;
    let rc = 12194f32.powi(2) * f2 / ((f2 + 20.6f32.powi(2)) * (f2 + 12194f32.powi(2)));
    20.0 * rc.log10() + 0.06
}

/// ISO-style base-10 bands between `fmin` and `fmax`.
fn make_bands(b: usize, fmin: f32, fmax: f32) -> Vec<Band> {
    let half = 10f32.powf(3.0 / (20.0 * b as f32));
    let mut v = Vec::new();
    for k in -60 * b as i32..=40 * b as i32 {
        let fc = 1000.0 * 10f32.powf(3.0 * k as f32 / (10.0 * b as f32));
        if fc / half < fmin * 0.99 || fc * half > fmax * 1.01 {
            continue;
        }
        v.push(Band { fc, lo: fc / half, hi: fc * half, w_db: [0.0, a_weight(fc), c_weight(fc)] });
    }
    v
}

impl Octave {
    pub fn new() -> Self {
        Self {
            frac: 1,
            weight: 0,
            time: 1,
            ch: 0,
            hold: false,
            target: false,
            tilt: 0.0,
            target_db: -30.0,
            deviation: false,
            y_lo: -100.0,
            y_hi: 0.0,
            bands: Vec::new(),
            key: (0, 0, 0),
            pow: Vec::new(),
            level: Vec::new(),
            peak: Vec::new(),
            total: -200.0,
            seq: 0,
            last: Instant::now(),
        }
    }

    fn compute(&mut self, cx: &Ctx) {
        let an = cx.an;
        let (data, n) = if self.time == 3 { (&an.avg[self.ch], an.n) } else { (&an.live[self.ch], an.n) };
        if n == 0 || data.len() < 8 {
            return;
        }
        let nyq = an.sr * 0.5;
        let key = (self.frac, an.sr.to_bits(), n);
        if key != self.key {
            self.key = key;
            self.bands = make_bands(FRACTIONS[self.frac].0, 20.0, nyq.min(20000.0));
            self.level = vec![-200.0; self.bands.len()];
            self.pow = vec![0.0; self.bands.len()];
            self.peak = vec![-200.0; self.bands.len()];
        }
        let df = an.sr / n as f32;
        let enbw = an.enbw_bins.max(1.0);
        let dt = self.last.elapsed().as_secs_f32().min(0.5);
        self.last = Instant::now();
        let tau = match self.time {
            1 => 0.125,
            2 => 1.0,
            _ => 0.0,
        };
        let alpha = if tau > 0.0 { 1.0 - (-dt / tau).exp() } else { 1.0 };
        let mut tot = 0.0f32;
        for (i, b) in self.bands.iter().enumerate() {
            let (b1, b2) = (b.lo / df, b.hi / df);
            let k0 = ((b1 + 0.5).floor() as usize).min(data.len() - 1);
            let k1 = ((b2 - 0.5).ceil().max(0.0) as usize).min(data.len() - 1);
            let mut s = 0.0f32;
            for k in k0..=k1 {
                let w = (b2.min(k as f32 + 0.5) - b1.max(k as f32 - 0.5)).clamp(0.0, 1.0);
                if w > 0.0 {
                    s += w * 10f32.powf(data[k] * 0.1);
                }
            }
            let p = s / enbw * 10f32.powf(b.w_db[self.weight] * 0.1);
            self.pow[i] += (p - self.pow[i]) * alpha;
            let db = 10.0 * self.pow[i].max(1e-20).log10();
            self.level[i] = db;
            if db > self.peak[i] {
                self.peak[i] = db;
            }
            tot += self.pow[i];
        }
        // hold marker decays slowly unless "hold" is on
        if !self.hold {
            for (p, l) in self.peak.iter_mut().zip(&self.level) {
                *p = (*p - 12.0 * dt).max(*l);
            }
        }
        self.total = 10.0 * tot.max(1e-20).log10();
    }

    fn target_at(&self, fc: f32) -> f32 {
        self.target_db + self.tilt * (fc / 1000.0).log2()
    }
}

impl Tool for Octave {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        if !visible || cx.an.seq == self.seq || self.last.elapsed().as_millis() < 30 {
            return;
        }
        self.seq = cx.an.seq;
        self.compute(cx);
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let combo = |ui: &mut Ui, id: &str, cur: &mut usize, items: &[&str], w: f32| {
                egui::ComboBox::from_id_salt(id).width(w).selected_text(items[*cur]).show_ui(ui, |ui| {
                    for (i, t) in items.iter().enumerate() {
                        ui.selectable_value(cur, i, *t);
                    }
                });
            };
            let names: Vec<&str> = FRACTIONS.iter().map(|f| f.1).collect();
            combo(ui, "oct_frac", &mut self.frac, &names, 56.0);
            combo(ui, "oct_w", &mut self.weight, &WEIGHTS, 70.0);
            combo(ui, "oct_t", &mut self.time, &TIMES, 96.0);
            ui.selectable_value(&mut self.ch, 0, RichText::new("L").color(crate::app::CH_COLORS[0]));
            ui.selectable_value(&mut self.ch, 1, RichText::new("R").color(crate::app::CH_COLORS[1]));
            ui.checkbox(&mut self.hold, "Hold").on_hover_text("Keep peak markers (clear by switching off)");
            ui.checkbox(&mut self.target, "Target").on_hover_text("Target curve: flat for pink noise, tilted for others");
            ui.checkbox(&mut self.deviation, "Deviation").on_hover_text("Show level minus target");
        });
        if self.target {
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.tilt, -6.0..=6.0).text("tilt dB/oct").fixed_decimals(1));
                ui.add(egui::Slider::new(&mut self.target_db, -120.0..=0.0).text("at 1 kHz").fixed_decimals(0));
                if ui.button("Fit").on_hover_text("Set the target level to the mean band level").clicked() && !self.level.is_empty() {
                    let mean: f32 = self.level.iter().enumerate().map(|(i, l)| l - self.tilt * (self.bands[i].fc / 1000.0).log2()).sum::<f32>() / self.level.len() as f32;
                    self.target_db = mean;
                }
            });
        }
        let off = cx.unit_off[self.ch];
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Total {:.1} {}{}", self.total + off, cx.unit_name, ["", "(A)", "(C)"][self.weight])).monospace());
            if !self.deviation {
                ui.add(egui::DragValue::new(&mut self.y_lo).speed(0.5).range(-200.0..=self.y_hi - 10.0).prefix("min "));
                ui.add(egui::DragValue::new(&mut self.y_hi).speed(0.5).range(self.y_lo + 10.0..=40.0).prefix("max "));
            }
        });

        let (resp, painter, rect) = canvas(ui, 120.0);
        let pr = super::plot::inset(rect);
        if pr.width() < 30.0 || pr.height() < 30.0 || self.bands.is_empty() {
            painter.text(rect.center(), Align2::CENTER_CENTER, "waiting for audio...", egui::FontId::proportional(14.0), crate::views::TXT);
            return;
        }
        let nb = self.bands.len();
        let (lo, hi) = if self.deviation { (-20.0, 20.0) } else { (self.y_lo + off, self.y_hi + off) };
        let plot = Plot::new(pr, 0.0, nb as f32, lo, hi, false);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.frame(&painter);
        let bw = pr.width() / nb as f32;
        let gap = if bw > 6.0 { 1.0 } else { 0.0 };
        let clip = painter.with_clip_rect(pr.expand(1.0));
        let (c0, c1) = (Color32::from_rgb(30, 150, 170), Color32::from_rgb(240, 200, 70));
        for (i, b) in self.bands.iter().enumerate() {
            let x0 = pr.left() + i as f32 * bw;
            let t = if self.deviation { self.level[i] - self.target_at(b.fc) } else { self.level[i] } + if self.deviation { 0.0 } else { off };
            let base = if self.deviation { plot.ty(0.0) } else { pr.bottom() };
            let y = plot.ty(t).clamp(pr.top(), pr.bottom());
            let col = if self.deviation { if t >= 0.0 { Color32::from_rgb(235, 140, 60) } else { Color32::from_rgb(70, 150, 230) } } else { lerp_col(c0, c1, (self.level[i] - self.y_lo) / (self.y_hi - self.y_lo)) };
            let r = Rect::from_two_pos(Pos2::new(x0 + gap, base), Pos2::new(x0 + bw - gap, y));
            clip.rect_filled(r, 0.0, col);
            if !self.deviation {
                let py = plot.ty(self.peak[i] + off).clamp(pr.top(), pr.bottom());
                clip.line_segment([Pos2::new(x0 + gap, py), Pos2::new(x0 + bw - gap, py)], Stroke::new(1.5, Color32::from_white_alpha(200)));
            }
        }
        if self.target && !self.deviation {
            let pts = self.bands.iter().enumerate().map(|(i, b)| (i as f32 + 0.5, self.target_at(b.fc) + off));
            plot.line(&painter, pts, Stroke::new(1.5, Color32::from_rgb(255, 90, 120)));
        }
        // labels at the 1/1-octave centres
        let step = (FRACTIONS[self.frac].0 as f32 / 1.0).round() as usize;
        let mut last_x = f32::MIN;
        for (i, b) in self.bands.iter().enumerate() {
            if i % step.max(1) != 0 {
                continue;
            }
            let x = pr.left() + (i as f32 + 0.5) * bw;
            if x - last_x < 30.0 {
                continue;
            }
            last_x = x;
            painter.text(Pos2::new(x, pr.bottom() + 2.0), Align2::CENTER_TOP, fmt_hz_short(b.fc), egui::FontId::proportional(10.0), crate::views::TXT);
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, if self.deviation { "dB dev" } else { cx.unit_name }, egui::FontId::proportional(9.0), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let i = (((h.x - pr.left()) / bw) as usize).min(nb - 1);
            let b = self.bands[i];
            let txt = format!("{} Hz   {:.1} {}", fmt_hz_short(b.fc), self.level[i] + off, cx.unit_name);
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
            clip.rect_stroke(Rect::from_min_max(Pos2::new(pr.left() + i as f32 * bw, pr.top()), Pos2::new(pr.left() + (i + 1) as f32 * bw, pr.bottom())), 0.0, Stroke::new(1.0, Color32::from_white_alpha(60)), egui::StrokeKind::Inside);
        }
        let _ = vec2(0.0, 0.0);
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("frac", self.frac);
        o.put("weight", self.weight);
        o.put("time", self.time);
        o.put("ch", self.ch);
        o.put("target", self.target);
        o.put("tilt", self.tilt);
        o.put("target_db", self.target_db);
        o.put("dev", self.deviation);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("frac") {
            self.frac = v.min(FRACTIONS.len() - 1);
        }
        if let Some(v) = i.get::<usize>("weight") {
            self.weight = v.min(2);
        }
        if let Some(v) = i.get::<usize>("time") {
            self.time = v.min(3);
        }
        if let Some(v) = i.get::<usize>("ch") {
            self.ch = v.min(1);
        }
        i.set("target", &mut self.target);
        i.set("tilt", &mut self.tilt);
        i.set("target_db", &mut self.target_db);
        i.set("dev", &mut self.deviation);
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        self.y_hi = self.y_hi.clamp(-60.0, 40.0);
        self.y_lo = self.y_lo.clamp(-200.0, self.y_hi - 10.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_contiguous_and_weights_sane() {
        let b = make_bands(3, 20.0, 20000.0);
        assert!(b.len() >= 28 && b.len() <= 33, "{}", b.len());
        for w in b.windows(2) {
            assert!((w[0].hi / w[1].lo - 1.0).abs() < 1e-3, "gap between bands");
        }
        assert!(a_weight(1000.0).abs() < 0.05);
        assert!((a_weight(100.0) + 19.1).abs() < 0.2);
        assert!(c_weight(1000.0).abs() < 0.05);
    }
}
