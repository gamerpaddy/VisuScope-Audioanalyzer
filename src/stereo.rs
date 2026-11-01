//! Stereo field: phosphor goniometer, phase-correlation meters and correlation-vs-frequency.

use crate::app::*;
use crate::shared::*;
use crate::views::{icon_btn, Ico, BG, GRID_MAJOR, TXT};
use eframe::egui::{self, vec2, Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui};
use std::collections::VecDeque;

const G: usize = 360;

pub struct StereoState {
    cursor: u64,
    buf: Vec<f32>,
    tex: Option<TextureHandle>,
    ll_f: f64,
    rr_f: f64,
    lr_f: f64,
    ll_s: f64,
    rr_s: f64,
    lr_s: f64,
    pub corr_f: f32,
    pub corr_s: f32,
    pub bal_db: f32,
    pub mid_db: f32,
    pub side_db: f32,
    hist: VecDeque<f32>,
    pub gain_db: f32,
    pub auto: bool,
    /// phosphor decay time constant in ms
    pub persist_ms: f32,
    /// display gain of the trace (1 = default)
    pub brightness: f32,
    /// infinite persistence (no decay)
    pub hold: bool,
    pub clear: bool,
    peak: f32,
    tl: Vec<f32>,
    tr: Vec<f32>,
    /// the picture has to be built and uploaded again (something is lit, or it just went dark)
    dirty: bool,
    lit: bool,
    /// brightness the uploaded picture was built with
    built_bright: f32,
}

impl StereoState {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            buf: vec![0.0; G * G],
            tex: None,
            ll_f: 0.0,
            rr_f: 0.0,
            lr_f: 0.0,
            ll_s: 0.0,
            rr_s: 0.0,
            lr_s: 0.0,
            corr_f: 0.0,
            corr_s: 0.0,
            bal_db: 0.0,
            mid_db: -120.0,
            side_db: -120.0,
            hist: VecDeque::with_capacity(900),
            gain_db: 0.0,
            auto: true,
            persist_ms: 60.0,
            brightness: 1.0,
            hold: false,
            clear: false,
            peak: 0.0,
            tl: Vec::new(),
            tr: Vec::new(),
            dirty: true,
            lit: true,
            built_bright: -1.0,
        }
    }

    /// Consume samples that arrived since the last frame.
    pub fn update(&mut self, sh: &Shared, dt: f32) {
        let sr = sh.sr.load(std::sync::atomic::Ordering::Relaxed) as f64;
        if sr == 0.0 {
            return;
        }
        let (from, n) = {
            // never block the UI thread on the audio / DSP threads
            let Some(ring) = sh.ring.try_lock() else { return };
            let total = ring.total;
            if total < self.cursor {
                self.cursor = total;
            }
            let from = self.cursor.max(total.saturating_sub(8192));
            let n = (total - from) as usize;
            self.tl.resize(n, 0.0);
            self.tr.resize(n, 0.0);
            if n > 0 {
                ring.copy_abs(from as usize, &mut self.tl, &mut self.tr);
            }
            self.cursor = total;
            (from, n)
        };
        let _ = from;

        if self.clear {
            self.buf.fill(0.0);
            self.clear = false;
            self.dirty = true;
        }
        let decay = if self.hold { 1.0 } else { (-dt / (self.persist_ms * 0.001).max(0.002)).exp() };
        let mut any = false;
        if decay < 1.0 {
            for v in self.buf.iter_mut() {
                *v *= decay;
                if *v < 1e-3 {
                    // dark enough to show nothing: exactly zero, so that an idle picture stops costing anything
                    *v = 0.0;
                } else {
                    any = true;
                }
            }
        } else {
            any = self.buf.iter().any(|v| *v > 0.0);
        }
        let pk = self.tl.iter().chain(self.tr.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
        self.peak = pk.max(self.peak * 0.985);
        let sc = if self.auto { (0.85 / self.peak.max(1e-4)).min(300.0) } else { 10f32.powf(self.gain_db / 20.0) };
        let half = G as f32 * 0.5;
        let k = half * 0.94 * std::f32::consts::FRAC_1_SQRT_2 * sc;

        let (af, as_) = ((-1.0 / (sr * 0.3)).exp(), (-1.0 / (sr * 3.0)).exp());
        let mut prev: Option<(f32, f32)> = None;
        let stride = (n / 3000).max(1);
        for i in 0..n {
            let (l, r) = (self.tl[i], self.tr[i]);
            let (ld, rd) = (l as f64, r as f64);
            self.ll_f = af * self.ll_f + (1.0 - af) * ld * ld;
            self.rr_f = af * self.rr_f + (1.0 - af) * rd * rd;
            self.lr_f = af * self.lr_f + (1.0 - af) * ld * rd;
            self.ll_s = as_ * self.ll_s + (1.0 - as_) * ld * ld;
            self.rr_s = as_ * self.rr_s + (1.0 - as_) * rd * rd;
            self.lr_s = as_ * self.lr_s + (1.0 - as_) * ld * rd;
            if i % stride != 0 {
                continue;
            }
            let px = half + (r - l) * k;
            let py = half - (l + r) * k;
            if let Some((qx, qy)) = prev {
                let steps = ((px - qx).abs().max((py - qy).abs())).ceil().clamp(1.0, 40.0);
                let inten = 0.32 / steps;
                for s in 1..=steps as usize {
                    let t = s as f32 / steps;
                    let (x, y) = (qx + (px - qx) * t, qy + (py - qy) * t);
                    if x >= 0.0 && y >= 0.0 && (x as usize) < G && (y as usize) < G {
                        let idx = y as usize * G + x as usize;
                        self.buf[idx] = (self.buf[idx] + inten).min(1.0);
                    }
                }
            }
            prev = Some((px, py));
        }
        // new points make the picture change; so does the last frame before it goes dark
        let drew = n > 0 && self.buf.iter().step_by(97).any(|v| *v > 0.0) || any;
        let was = self.lit;
        self.lit = drew;
        self.dirty |= drew || was;
        let corr = |lr: f64, ll: f64, rr: f64| (lr / (ll * rr + 1e-24).sqrt()) as f32;
        self.corr_f = corr(self.lr_f, self.ll_f, self.rr_f);
        self.corr_s = corr(self.lr_s, self.ll_s, self.rr_s);
        self.bal_db = (10.0 * ((self.rr_f + 1e-20) / (self.ll_f + 1e-20)).log10()) as f32;
        let mid = (self.ll_f + self.rr_f + 2.0 * self.lr_f) * 0.5;
        let side = (self.ll_f + self.rr_f - 2.0 * self.lr_f) * 0.5;
        self.mid_db = (10.0 * (mid.max(0.0) + 1e-12).log10()) as f32;
        self.side_db = (10.0 * (side.max(0.0) + 1e-12).log10()) as f32;
        if n > 0 {
            self.hist.push_back(self.corr_f);
            if self.hist.len() > 600 {
                self.hist.pop_front();
            }
        }
    }
}

fn corr_color(v: f32) -> Color32 {
    let t = ((v + 1.0) * 0.5).clamp(0.0, 1.0);
    if t < 0.5 {
        let s = t * 2.0;
        Color32::from_rgb(230, (60.0 + 170.0 * s) as u8, 50)
    } else {
        let s = (t - 0.5) * 2.0;
        Color32::from_rgb((230.0 - 170.0 * s) as u8, 220, (50.0 + 40.0 * s) as u8)
    }
}

impl App {
    pub fn panel_stereo(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let single = self.wins[2].prev.is_some();
            if icon_btn(ui, if single { Ico::Restore } else { Ico::Maximize }, if single { "Restore window size" } else { "Maximize window" }, single) {
                self.max_req = Some(2);
            }
        });
        let avail = ui.available_size();
        if avail.x < 620.0 {
            // narrow cell: scope on top, meters underneath, scrollable
            let side = (avail.x - 16.0).min(avail.y * 0.62).max(100.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.gonio_ui(ui, side);
                self.meters_ui(ui, Some(190.0));
            });
            return;
        }
        // leave room under the scope for the gain / persistence / clear rows
        let side = (avail.y - 110.0).min(avail.x * 0.5).max(100.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(side);
                    self.gonio_ui(ui, side);
                });
                ui.vertical(|ui| self.meters_ui(ui, None));
            });
        });
    }

    fn gonio_ui(&mut self, ui: &mut Ui, side: f32) {
        let (resp, painter) = ui.allocate_painter(vec2(side, side), Sense::hover());
        let r = resp.rect;
        painter.rect_filled(r, 2.0, BG);
        let rebuild = self.stereo.dirty || self.stereo.tex.is_none() || (self.stereo.built_bright - self.stereo.brightness).abs() > 1e-6;
        if rebuild {
            self.stereo.dirty = false;
            self.stereo.built_bright = self.stereo.brightness;
            let s = &self.stereo;
            // the colour curve is a table of 1024 steps (a power per pixel and frame was the most expensive part of this window)
            let lut: Vec<Color32> = (0..1024)
                .map(|i| {
                    let g = (i as f32 / 1023.0).powf(0.55);
                    Color32::from_rgb((g * g * 150.0) as u8, (g * 255.0) as u8, (g * g * 120.0) as u8)
                })
                .collect();
            let k = s.brightness * 1023.0;
            let px: Vec<Color32> = s.buf.iter().map(|&v| lut[(v * k).clamp(0.0, 1023.0) as usize]).collect();
            let img = ColorImage::new([G, G], px);
            match &mut self.stereo.tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.stereo.tex = Some(ui.ctx().load_texture("gonio", img, TextureOptions::LINEAR)),
            }
        }
        if let Some(t) = &self.stereo.tex {
            painter.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        let c = r.center();
        let rad = side * 0.5 * 0.94;
        let gs = Stroke::new(1.0, GRID_MAJOR);
        painter.circle_stroke(c, rad, gs);
        painter.circle_stroke(c, rad * 0.5, Stroke::new(1.0, Color32::from_white_alpha(14)));
        painter.line_segment([c - vec2(rad, 0.0), c + vec2(rad, 0.0)], gs);
        painter.line_segment([c - vec2(0.0, rad), c + vec2(0.0, rad)], gs);
        let d = rad * std::f32::consts::FRAC_1_SQRT_2;
        painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], Stroke::new(1.0, Color32::from_white_alpha(22)));
        painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], Stroke::new(1.0, Color32::from_white_alpha(22)));
        let f = FontId::proportional(11.0);
        painter.text(c + vec2(-d - 4.0, -d - 4.0), Align2::RIGHT_BOTTOM, "L", f.clone(), CH_COLORS[0]);
        painter.text(c + vec2(d + 4.0, -d - 4.0), Align2::LEFT_BOTTOM, "R", f.clone(), CH_COLORS[1]);
        painter.text(c + vec2(4.0, -rad), Align2::LEFT_TOP, "M (mono)", f.clone(), TXT);
        painter.text(c + vec2(rad - 2.0, -4.0), Align2::RIGHT_BOTTOM, "S", f.clone(), TXT);
        painter.text(c + vec2(0.0, rad - 2.0), Align2::CENTER_BOTTOM, "out of phase", f, Color32::from_rgb(230, 90, 70));

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.stereo.auto, "Auto gain");
            ui.add_enabled(!self.stereo.auto, egui::Slider::new(&mut self.stereo.gain_db, -20.0..=60.0).suffix(" dB"));
        });
        ui.horizontal(|ui| {
            ui.add_enabled(
                !self.stereo.hold,
                egui::Slider::new(&mut self.stereo.persist_ms, 5.0..=5000.0).logarithmic(true).suffix(" ms").text("Persist"),
            );
        });
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.stereo.brightness, 0.1..=8.0).logarithmic(true).text("Brightness").custom_formatter(|v, _| format!("x{v:.2}")));
            if ui.small_button("1x").on_hover_text("Default brightness").clicked() {
                self.stereo.brightness = 1.0;
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.stereo.hold, "Infinite");
            if ui.button("Clear").clicked() {
                self.stereo.clear = true;
            }
        });

    }

    fn meters_ui(&mut self, ui: &mut Ui, fixed: Option<f32>) {
        let gs = Stroke::new(1.0, GRID_MAJOR);
        let rest = fixed.unwrap_or_else(|| (ui.available_height() - 76.0).clamp(120.0, 600.0));
        let (hist_h, band_h) = (rest * 0.36, rest * 0.64 - 10.0);
        // correlation meter
        let s = &self.stereo;
        let (resp, painter) = ui.allocate_painter(vec2(ui.available_width(), 34.0), Sense::hover());
        let m = Rect::from_min_max(resp.rect.min + vec2(6.0, 4.0), resp.rect.max - vec2(6.0, 14.0));
        let n = 48;
        for i in 0..n {
            let t0 = i as f32 / n as f32;
            let t1 = (i + 1) as f32 / n as f32;
            let col = corr_color(t0 * 2.0 - 1.0);
            let dim = Color32::from_rgb((col.r() as f32 * 0.35) as u8, (col.g() as f32 * 0.35) as u8, (col.b() as f32 * 0.35) as u8);
            let seg = Rect::from_min_max(Pos2::new(m.left() + t0 * m.width(), m.top()), Pos2::new(m.left() + t1 * m.width(), m.bottom()));
            let active = {
                let v = s.corr_f;
                let mid = 0.5;
                let vt = (v + 1.0) * 0.5;
                (t0 + 0.5 / n as f32 - mid) * (vt - mid) > 0.0 && (t0 + 0.5 / n as f32 - mid).abs() <= (vt - mid).abs()
            };
            painter.rect_filled(seg, 0.0, if active { col } else { dim });
        }
        let sx = |v: f32| m.left() + (v + 1.0) * 0.5 * m.width();
        painter.line_segment([Pos2::new(sx(s.corr_s), m.top() - 3.0), Pos2::new(sx(s.corr_s), m.bottom() + 3.0)], Stroke::new(2.5, Color32::WHITE));
        for (v, t) in [(-1.0, "-1"), (0.0, "0"), (1.0, "+1")] {
            painter.text(Pos2::new(crate::views::fclamp(sx(v), m.left() + 6.0, m.right() - 6.0), m.bottom() + 2.0), Align2::CENTER_TOP, t, FontId::proportional(10.0), TXT);
        }
        ui.label(RichText::new(format!(
            "Correlation  fast {:+.2}   slow {:+.2}   balance {:+.1} dB (R-L)   M {:.1} / S {:.1} dB   width {:.0}%",
            self.sm_st[0],
            self.sm_st[1],
            self.sm_st[2],
            self.sm_st[3],
            self.sm_st[4],
            100.0 * 10f32.powf((self.sm_st[4] - self.sm_st[3]) / 20.0).min(2.0)
        )).small());

        // correlation history
        let (resp, painter) = ui.allocate_painter(vec2(ui.available_width(), hist_h), Sense::hover());
        let h = resp.rect;
        painter.rect_filled(h, 2.0, BG);
        let y = |v: f32| h.center().y - v * (h.height() * 0.45);
        painter.line_segment([Pos2::new(h.left(), y(0.0)), Pos2::new(h.right(), y(0.0))], gs);
        let len = s.hist.len().max(2);
        let mut prev: Option<Pos2> = None;
        for (i, &v) in s.hist.iter().enumerate() {
            let p = Pos2::new(h.right() - (len - 1 - i) as f32 / 600.0 * h.width(), y(v));
            if let Some(q) = prev {
                painter.line_segment([q, p], Stroke::new(1.3, corr_color(v)));
            }
            prev = Some(p);
        }
        painter.text(h.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, "correlation history (10 s)", FontId::proportional(9.0), TXT);

        // correlation vs frequency
        let (resp, painter) = ui.allocate_painter(vec2(ui.available_width(), band_h), Sense::hover());
        let h = resp.rect;
        painter.rect_filled(h, 2.0, BG);
        let plot = Rect::from_min_max(h.min + vec2(24.0, 4.0), h.max - vec2(4.0, 14.0));
        let y = |v: f32| plot.center().y - v * plot.height() * 0.5;
        painter.line_segment([Pos2::new(plot.left(), y(0.0)), Pos2::new(plot.right(), y(0.0))], gs);
        painter.text(Pos2::new(plot.left() - 3.0, y(1.0)), Align2::RIGHT_CENTER, "+1", FontId::proportional(9.0), TXT);
        painter.text(Pos2::new(plot.left() - 3.0, y(-1.0)), Align2::RIGHT_CENTER, "-1", FontId::proportional(9.0), TXT);
        if self.an.meas_n > 0 && self.an.corr.len() > 2 {
            let data = &self.an.corr;
            // computed on the measurement FFT, so it does not depend on the display window
            let bin_hz = self.an.sr / self.an.meas_n as f32;
            let w = plot.width().floor().max(1.0) as usize;
            let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
            let mut last = (0usize, 0.0f32);
            for i in 0..w {
                let u0 = flo_u + i as f32 / w as f32 * (fhi_u - flo_u);
                let u1 = flo_u + (i + 1) as f32 / w as f32 * (fhi_u - flo_u);
                let (b0, b1) = (self.u_to_f(u0) / bin_hz, self.u_to_f(u1) / bin_hz);
                let a = (b0.floor().max(0.0) as usize).min(data.len() - 1);
                let b = (b1.ceil().max(0.0) as usize).min(data.len() - 1);
                let v = if b1 - b0 < 1.0 {
                    let mid = (0.5 * (b0 + b1)).clamp(0.0, (data.len() - 1) as f32);
                    let k = mid.floor() as usize;
                    let fr = mid - k as f32;
                    data[k] * (1.0 - fr) + data[(k + 1).min(data.len() - 1)] * fr
                } else {
                    data[a..=b].iter().sum::<f32>() / (b - a + 1) as f32
                };
                last = (i, v);
                let x = plot.left() + i as f32 + 0.5;
                painter.line_segment([Pos2::new(x, y(0.0)), Pos2::new(x, y(v.clamp(-1.0, 1.0)))], Stroke::new(1.0, corr_color(v)));
            }
            let _ = last;
            for (f, label) in self.freq_ticks(self.u_to_f(flo_u), self.u_to_f(fhi_u)) {
                if label {
                    let x = plot.left() + (self.f_to_u(f) - flo_u) / (fhi_u - flo_u) * plot.width();
                    painter.text(Pos2::new(x, plot.bottom() + 1.0), Align2::CENTER_TOP, fmt_hz_short(f), FontId::proportional(9.0), TXT);
                }
            }
        }
        painter.text(plot.left_top() + vec2(3.0, 0.0), Align2::LEFT_TOP, "L/R correlation vs frequency", FontId::proportional(9.0), TXT);
    }
}
