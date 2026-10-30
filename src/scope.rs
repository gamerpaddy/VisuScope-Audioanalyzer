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
    last_trig: Instant,
    meas: [Meas; 2],
    meas_t: Instant,
}

/// 4th-order Butterworth-style low-pass applied forwards and backwards (zero phase, so the trigger
/// point does not move relative to the displayed signal).
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
            last_trig: long_ago,
            meas: Default::default(),
            meas_t: long_ago,
        }
    }

    fn n_win(&self, sr: f32) -> usize {
        ((10.0 * self.t_div * sr).round() as usize).clamp(64, RING_CAP / 2 - 16)
    }

    pub fn min_t_div(sr: f32) -> f32 {
        64.0 / (10.0 * sr)
    }

    pub fn max_t_div(sr: f32) -> f32 {
        (RING_CAP as f32 / 2.0 - 16.0) / (10.0 * sr)
    }

    /// Grab the newest samples and align them to a trigger crossing.
    fn acquire(&mut self, sh: &Shared, scale: [f32; 2]) {
        let sr = sh.sr.load(Relaxed) as f32;
        if sr <= 0.0 || !self.running || (self.mode == 2 && !self.armed) {
            return;
        }
        let n_win = self.n_win(sr);
        if self.mode == 3 {
            // roll mode: no trigger, always show the newest n_win samples scrolling in from the right
            for c in 0..2 {
                self.tbuf[c].resize(n_win, 0.0);
            }
            {
                let Some(ring) = sh.ring.try_lock() else { return };
                let (a, b) = self.tbuf.split_at_mut(1);
                ring.copy_last(&mut a[0], &mut b[0]);
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
        let m = (n_win * 2).min(RING_CAP);
        for c in 0..2 {
            self.tbuf[c].resize(m, 0.0);
        }
        {
            let Some(ring) = sh.ring.try_lock() else { return };
            let (a, b) = self.tbuf.split_at_mut(1);
            ring.copy_last(&mut a[0], &mut b[0]);
        }
        let src = self.trig_src;
        let filtered;
        let x: &[f32] = if self.hf_reject {
            filtered = lowpass_zero_phase(&self.tbuf[src], self.hf_hz, sr);
            &filtered
        } else {
            &self.tbuf[src]
        };
        let lvl = self.level / scale[src].max(1e-12);
        let hy = self.hyst * 0.01;
        let hi = m - post - 1;
        let lo = pre.max(16);
        let mut found = None;
        let mut i = hi;
        while i > lo {
            let (a, b) = (x[i - 1], x[i]);
            let hit = if self.rising { a < lvl && b >= lvl && x[i - 12] < lvl - hy } else { a > lvl && b <= lvl && x[i - 12] > lvl + hy };
            if hit {
                found = Some(i);
                break;
            }
            i -= 1;
        }
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
                s.t_div = fclamp(s.t_div * act.zx, ScopeState::min_t_div(sr), ScopeState::max_t_div(sr));
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

        // controls
        let cal_on = self.cal.on;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(5.0, 3.0);
            let s = &mut self.scope;
            let run_txt = if s.running { "Stop" } else { "Run" };
            if ui.button(run_txt).clicked() {
                s.running = !s.running;
            }
            ui.checkbox(&mut s.show[0], RichText::new("L").color(CH_COLORS[0]));
            ui.checkbox(&mut s.show[1], RichText::new("R").color(CH_COLORS[1]));
            ui.label(RichText::new("Trig").small().weak());
            egui::ComboBox::from_id_salt("tsrc").width(36.0).selected_text(["L", "R"][s.trig_src]).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.trig_src, 0, "L");
                ui.selectable_value(&mut s.trig_src, 1, "R");
            });
            egui::ComboBox::from_id_salt("tmode").width(64.0).selected_text(["Auto", "Normal", "Single", "Roll"][s.mode as usize]).show_ui(ui, |ui| {
                for (i, n) in ["Auto", "Normal", "Single", "Roll"].iter().enumerate() {
                    ui.selectable_value(&mut s.mode, i as u8, *n);
                }
            });
            egui::ComboBox::from_id_salt("tedge").width(58.0).selected_text(if s.rising { "Rising" } else { "Falling" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.rising, true, "Rising");
                ui.selectable_value(&mut s.rising, false, "Falling");
            });
            if s.mode == 2 && ui.button("Arm").clicked() {
                s.armed = true;
            }
            // The level spans full scale (+-1 FS, or +-volts when calibrated) whatever part of it is on screen.
            let fs = scale[s.trig_src].max(1e-9);
            s.level = s.level.clamp(-fs, fs);
            ui.add_enabled_ui(s.mode != 3, |ui| {
                ui.spacing_mut().slider_width = 130.0;
                ui.add(
                    egui::Slider::new(&mut s.level, -fs..=fs)
                        .text("level")
                        .custom_formatter(move |v, _| if cal_on { fmt_volts(v as f32) } else { format!("{v:+.3} FS") }),
                )
                .on_hover_text("Trigger level over the full range, also above or below the visible window");
            });
            let mut pct = s.pos * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(2.0..=98.0).suffix("%").prefix("pos ")).changed() {
                s.pos = pct / 100.0;
            }
            ui.add(egui::DragValue::new(&mut s.hyst).range(0.0..=20.0).speed(0.05).suffix("% hyst"));
            ui.checkbox(&mut s.hf_reject, "HF reject").on_hover_text("Low-pass filter on the trigger signal only. The displayed trace is not filtered.");
            ui.add_enabled(s.hf_reject, egui::DragValue::new(&mut s.hf_hz).range(100.0..=96000.0).speed(20.0).suffix(" Hz"));
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
                )).small().color(CH_COLORS[c]));
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
        let s = &self.scope;
        let n_win = s.n_win(sr);
        let visible = n_win as f32 / sr;        let t_per_div = visible / 10.0;
        let center_y = plot.center().y;
        let y_of = |v: f32| center_y - (v + s.v_off) / (8.0 * s.v_div) * plot.height();

        // interaction: click / drag sets the trigger level, wheel zooms
        let mut new_level = None;
        if s.mode != 3 && (resp.clicked() || resp.dragged_by(egui::PointerButton::Primary)) {
            if let Some(p) = resp.interact_pointer_pos() {
                new_level = Some((center_y - p.y) / plot.height() * 8.0 * s.v_div - s.v_off);
            }
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
            let col = CH_COLORS[c];
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