//! Signal statistics: sample histogram, clipping, bit-depth use and long-term trends.

use super::plot::{canvas_h, heading, inset, Plot};
use super::tap::RingTap;
use super::*;
use crate::app::CH_COLORS;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};
use std::collections::VecDeque;
use std::time::Instant;

const BINS: usize = 201;
const TREND_LEN: usize = 2400;
const TRENDS: [(&str, &str); 4] = [("Level (RMS)", "dBFS"), ("Crest factor", "dB"), ("DC offset", "% FS"), ("Noise floor", "dBFS")];

pub struct Stats {
    tap: RingTap,
    ch: usize,
    fade: bool,
    trend: usize,
    hist: [Vec<f32>; 2],
    clip: [u64; 2],
    clip_events: [u64; 2],
    run: [u32; 2],
    min: [f32; 2],
    max: [f32; 2],
    sum: [f64; 2],
    sq: [f64; 2],
    count: [u64; 2],
    bits: [Option<u32>; 2],
    recent: [Vec<f32>; 2],
    bits_t: Instant,
    series: VecDeque<[f32; 4]>,
    series_t: Instant,
    was_running: bool,
}

/// Smallest word length (bits) whose integer grid contains all samples, if any.
pub fn word_length(samples: &[f32]) -> Option<u32> {
    if samples.len() < 256 {
        return None;
    }
    [8u32, 12, 16, 18, 20, 24].into_iter().find(|&b| {
        let scale = (1u64 << (b - 1)) as f64;
        let bad = samples.iter().filter(|&&v| {
            let x = v as f64 * scale;
            (x - x.round()).abs() > 2e-3
        });
        bad.count() * 200 < samples.len()
    })
}

impl Stats {
    pub fn new() -> Self {
        Self {
            tap: RingTap::new(),
            ch: 0,
            fade: false,
            trend: 0,
            hist: [vec![0.0; BINS], vec![0.0; BINS]],
            clip: [0; 2],
            clip_events: [0; 2],
            run: [0; 2],
            min: [0.0; 2],
            max: [0.0; 2],
            sum: [0.0; 2],
            sq: [0.0; 2],
            count: [0; 2],
            bits: [None; 2],
            recent: [Vec::new(), Vec::new()],
            bits_t: Instant::now(),
            series: VecDeque::new(),
            series_t: Instant::now(),
            was_running: false,
        }
    }

    fn reset(&mut self) {
        let keep = (self.ch, self.fade, self.trend);
        let tap = std::mem::replace(&mut self.tap, RingTap::new());
        let series = std::mem::take(&mut self.series);
        *self = Self::new();
        self.tap = tap;
        self.series = series;
        (self.ch, self.fade, self.trend) = keep;
    }
}

impl Tool for Stats {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        // trends are cheap and keep running while the window is closed
        if self.series_t.elapsed().as_millis() >= 250 && cx.an.n > 0 {
            self.series_t = Instant::now();
            let ch = self.ch;
            let m = cx.an.metrics[ch];
            self.series.push_back([cx.an.rms_db[ch], cx.an.peak_db[ch] - cx.an.rms_db[ch], cx.an.dc[ch] * 100.0, if m.valid { m.noise_floor_dbfs } else { f32::NAN }]);
            if self.series.len() > TREND_LEN {
                self.series.pop_front();
            }
        }
        if !visible {
            self.was_running = false;
            return;
        }
        if !self.was_running {
            self.was_running = true;
            self.tap.sync(cx.sh);
            return;
        }
        let (l, r) = self.tap.poll(cx.sh);
        let n = l.len();
        if n == 0 {
            return;
        }
        if self.fade {
            let k = (1.0 - n as f32 / (cx.sr * 10.0)).clamp(0.0, 1.0);
            for h in self.hist.iter_mut().flat_map(|h| h.iter_mut()) {
                *h *= k;
            }
        }
        for (c, x) in [l, r].into_iter().enumerate() {
            for &v in x {
                let b = (((v.clamp(-1.0, 1.0) + 1.0) * 0.5) * (BINS - 1) as f32).round() as usize;
                self.hist[c][b] += 1.0;
                if v.abs() >= 0.9999 {
                    self.clip[c] += 1;
                    self.run[c] += 1;
                    if self.run[c] == 3 {
                        self.clip_events[c] += 1;
                    }
                } else {
                    self.run[c] = 0;
                }
                self.min[c] = self.min[c].min(v);
                self.max[c] = self.max[c].max(v);
                self.sum[c] += v as f64;
                self.sq[c] += (v as f64) * (v as f64);
            }
            self.count[c] += x.len() as u64;
            // keep a sample of non-zero values for the word-length test
            let rec = &mut self.recent[c];
            rec.extend(x.iter().step_by(3).filter(|v| **v != 0.0));
            if rec.len() > 8192 {
                rec.drain(..rec.len() - 8192);
            }
        }
        if self.bits_t.elapsed().as_millis() > 1000 {
            self.bits_t = Instant::now();
            for c in 0..2 {
                self.bits[c] = word_length(&self.recent[c]);
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let c = self.ch;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.ch, 0, RichText::new("L").color(CH_COLORS[0]));
            ui.selectable_value(&mut self.ch, 1, RichText::new("R").color(CH_COLORS[1]));
            ui.checkbox(&mut self.fade, "Fade (10 s)").on_hover_text("Histogram forgets old samples");
            if ui.button("Reset").clicked() {
                self.reset();
            }
        });
        let n = self.count[c].max(1) as f64;
        let mean = self.sum[c] / n;
        let rms = (self.sq[c] / n).sqrt();
        let peak = self.max[c].max(-self.min[c]);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let col = Color32::from_rgb(210, 220, 235);
            let bits = match self.bits[c] {
                Some(b) => format!("{b} bit"),
                None => ">= 24 / float".into(),
            };
            super::plot::readout_sized(ui, "Peak", &format!("{:.1} dBFS", 20.0 * peak.max(1e-9).log10()), col, 17.0);
            super::plot::readout_sized(ui, "Crest", &format!("{:.1} dB", 20.0 * (peak / rms.max(1e-9) as f32).max(1e-9).log10()), col, 17.0);
            super::plot::readout_sized(ui, "DC", &format!("{:.3} %", mean * 100.0), col, 17.0);
            super::plot::readout_sized(ui, "Word length", &bits, col, 17.0);
            let clip_col = if self.clip_events[c] > 0 { Color32::from_rgb(240, 100, 90) } else { Color32::from_rgb(100, 230, 130) };
            super::plot::readout_sized(ui, "Clip events", &format!("{} ({})", self.clip_events[c], self.clip[c]), clip_col, 17.0);
        });
        if let Some(b) = self.bits[c] {
            let used = b as f32;
            ui.label(RichText::new(format!("Signal sits on a {b}-bit grid; headroom {:.1} dB, about {:.1} of {used:.0} bits used.", -20.0 * peak.max(1e-9).log10(), (used + 20.0 * peak.max(1e-9).log10() / 6.02).max(0.0))).small().weak());
        }

        let h = ui.available_height();
        heading(ui, "Sample distribution");
        let (resp, painter, rect) = canvas_h(ui, ((h - 70.0) * 0.5).max(50.0));
        let pr = inset(rect);
        if pr.width() > 40.0 && pr.height() > 20.0 {
            let hist = &self.hist[c];
            let maxc = hist.iter().cloned().fold(1.0, f32::max);
            let top = maxc.log10().ceil().max(1.0);
            let plot = Plot::new(pr, -1.0, 1.0, 0.0, top, false);
            plot.grid_y(&painter, &|v| format!("1e{v:.0}"));
            plot.grid_x(&painter, &|v| format!("{v:.1}"));
            plot.frame(&painter);
            let bw = pr.width() / BINS as f32;
            let clip = painter.with_clip_rect(pr);
            for (i, &v) in hist.iter().enumerate() {
                if v < 1.0 {
                    continue;
                }
                let y = plot.ty(v.log10());
                let edge = i == 0 || i == BINS - 1;
                let col = if edge && v > 0.0 { Color32::from_rgb(240, 90, 80) } else { CH_COLORS[c] };
                clip.rect_filled(egui::Rect::from_min_max(Pos2::new(pr.left() + i as f32 * bw, y), Pos2::new(pr.left() + (i + 1) as f32 * bw, pr.bottom())), 0.0, col);
            }
            if let Some(hp) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                let i = (((hp.x - pr.left()) / bw) as usize).min(BINS - 1);
                plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{:+.3}   {} samples", plot.fx(hp.x), hist[i] as u64), Color32::WHITE, Align2::RIGHT_TOP);
            }
        }
        ui.horizontal(|ui| {
            heading(ui, "Trend (10 min)");
            egui::ComboBox::from_id_salt("st_trend").width(110.0).selected_text(TRENDS[self.trend].0).show_ui(ui, |ui| {
                for (i, t) in TRENDS.iter().enumerate() {
                    ui.selectable_value(&mut self.trend, i, t.0);
                }
            });
        });
        let (resp, painter, rect) = canvas_h(ui, ui.available_height().max(50.0));
        let pr = inset(rect);
        if pr.width() > 40.0 && pr.height() > 20.0 && self.series.len() > 2 {
            let k = self.trend;
            let vals: Vec<f32> = self.series.iter().map(|s| s[k]).filter(|v| v.is_finite()).collect();
            if vals.len() > 2 {
                let (mut lo, mut hi) = vals.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
                let pad = ((hi - lo) * 0.1).max(if k == 2 { 0.01 } else { 0.5 });
                lo -= pad;
                hi += pad;
                let off = if k == 0 || k == 3 { cx.unit_off[c] } else { 0.0 };
                let span = TREND_LEN as f32 * 0.25;
                let plot = Plot::new(pr, -span, 0.0, lo + off, hi + off, false);
                plot.grid_y(&painter, &|v| format!("{v:.1}"));
                plot.grid_x(&painter, &|v| format!("{:.0}s", v));
                plot.frame(&painter);
                let len = self.series.len();
                plot.line(&painter, self.series.iter().enumerate().map(|(i, s)| (-((len - 1 - i) as f32) * 0.25, s[k] + off)), Stroke::new(1.4, CH_COLORS[c]));
                painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, if off != 0.0 { cx.unit_name } else { TRENDS[k].1 }, FontId::proportional(9.0), crate::views::TXT);
                if let Some(hp) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                    let i = (len as f32 - 1.0 + plot.fx(hp.x) / 0.25).round().clamp(0.0, len as f32 - 1.0) as usize;
                    plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{:.2} {}", self.series[i][k] + off, TRENDS[k].1), Color32::WHITE, Align2::RIGHT_TOP);
                }
            }
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("ch", self.ch);
        o.put("fade", self.fade);
        o.put("trend", self.trend);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("ch", &mut self.ch);
        i.set("fade", &mut self.fade);
        i.set("trend", &mut self.trend);
        self.ch = self.ch.min(1);
        self.trend = self.trend.min(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_word_length() {
        let sine = |bits: u32| -> Vec<f32> {
            let q = (1u64 << (bits - 1)) as f64;
            (0..4000).map(|i| ((0.3 * (i as f64 * 0.01237).sin() * q).round() / q) as f32).collect()
        };
        assert_eq!(word_length(&sine(16)), Some(16));
        assert_eq!(word_length(&sine(12)), Some(12));
        assert_eq!(word_length(&sine(24)), Some(24));
        let float: Vec<f32> = (0..4000).map(|i| (0.3 * (i as f32 * 0.01237).sin()) * 0.7321).collect();
        assert_eq!(word_length(&float), None);
    }
}
