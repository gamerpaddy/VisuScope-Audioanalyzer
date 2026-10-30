//! Impulse response viewer with Schroeder decay, RT60 / EDT and clarity metrics.

use super::plot::{canvas, inset, Plot};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const VIEWS: [&str; 3] = ["Waveform", "Energy (dB)", "Decay"];

#[derive(Default, Clone)]
pub struct Metrics {
    pub edt: Option<f32>,
    pub t20: Option<f32>,
    pub t30: Option<f32>,
    pub c50: f32,
    pub c80: f32,
    pub d50: f32,
    pub snr_db: f32,
    pub usable_ms: f32,
}

struct Analysis {
    id: u64,
    sr: f32,
    pre: usize,
    peak_i: usize,
    /// energy in dB relative to the peak, 1 ms smoothed
    etc: Vec<f32>,
    /// Schroeder decay in dB, starting at the peak
    edc: Vec<f32>,
    m: Metrics,
}

pub struct Impulse {
    view: usize,
    zoom_ms: f32,
    ana: Option<Analysis>,
    save: Option<dialogs::Pending>,
}

fn regress(edc: &[f32], sr: f32, a: f32, b: f32) -> Option<f32> {
    let ia = edc.iter().position(|&v| v <= a)?;
    let ib = edc.iter().position(|&v| v <= b)?;
    if ib <= ia + 3 {
        return None;
    }
    let n = (ib - ia + 1) as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0f64, 0f64, 0f64, 0f64);
    for i in ia..=ib {
        let (x, y) = (i as f64 / sr as f64, edc[i] as f64);
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    (slope < -1.0).then(|| (-60.0 / slope) as f32)
}

pub fn analyse(ir: &[f32], sr: f32) -> Option<(usize, Vec<f32>, Vec<f32>, Metrics)> {
    if ir.len() < 512 {
        return None;
    }
    let tail = ir.len() / 10;
    let dc = ir[ir.len() - tail..].iter().sum::<f32>() / tail as f32;
    let h: Vec<f32> = ir.iter().map(|v| v - dc).collect();
    let pk = h.iter().enumerate().fold((0, 0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m }).0;
    let e: Vec<f64> = h[pk..].iter().map(|v| (*v as f64).powi(2)).collect();
    let n = e.len();
    let noise = e[n - n / 10..].iter().sum::<f64>() / (n / 10).max(1) as f64;
    // truncation: where a 10 ms moving average meets the noise floor (+3 dB)
    let w = ((0.01 * sr) as usize).max(8);
    let mut cut = n;
    let mut acc: f64 = e[..w.min(n)].iter().sum();
    for i in w..n {
        acc += e[i] - e[i - w];
        if i > (0.05 * sr) as usize && acc / w as f64 <= noise * 2.0 {
            cut = i;
            break;
        }
    }
    let cut = cut.max((0.05 * sr) as usize).min(n);
    // Schroeder integral with noise compensation
    let mut edc_lin = vec![0f64; cut];
    let mut s = 0f64;
    for i in (0..cut).rev() {
        s += (e[i] - noise).max(0.0);
        edc_lin[i] = s;
    }
    let tot = edc_lin[0].max(1e-30);
    let edc: Vec<f32> = edc_lin.iter().map(|v| (10.0 * (v / tot).max(1e-12).log10()) as f32).collect();
    let (n50, n80) = (((0.05 * sr) as usize).min(cut), ((0.08 * sr) as usize).min(cut));
    let sum = |a: usize, b: usize| e[a..b].iter().sum::<f64>().max(1e-30);
    let m = Metrics {
        edt: regress(&edc, sr, 0.0, -10.0),
        t20: regress(&edc, sr, -5.0, -25.0),
        t30: regress(&edc, sr, -5.0, -35.0),
        c50: (10.0 * (sum(0, n50) / sum(n50, cut.max(n50 + 1))).log10()) as f32,
        c80: (10.0 * (sum(0, n80) / sum(n80, cut.max(n80 + 1))).log10()) as f32,
        d50: (100.0 * sum(0, n50) / sum(0, cut)) as f32,
        snr_db: (10.0 * (e[0] / noise.max(1e-30)).log10()) as f32,
        usable_ms: cut as f32 / sr * 1000.0,
    };
    // 1 ms smoothed energy for the "Energy" view
    let k = ((0.001 * sr) as usize).max(1);
    let peak_e = e[0].max(1e-30);
    let etc: Vec<f32> = (0..n / k).map(|i| (10.0 * (e[i * k..(i + 1) * k].iter().sum::<f64>() / k as f64 / peak_e).max(1e-14).log10()) as f32).collect();
    Some((pk, etc, edc, m))
}

impl Impulse {
    pub fn new() -> Self {
        Self { view: 1, zoom_ms: 0.0, ana: None, save: None }
    }

    fn refresh(&mut self, r: &super::sweep::SweepResult) {
        self.ana = analyse(&r.ir, r.sr).map(|(pk, etc, edc, m)| Analysis { id: r.id, sr: r.sr, pre: r.pre, peak_i: pk, etc, edc, m });
    }
}

fn rt(v: Option<f32>) -> String {
    v.map(|x| format!("{x:.2} s")).unwrap_or_else(|| "-".into())
}

impl Tool for Impulse {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        if let Some(d) = &self.save {
            if let Some(res) = d.take() {
                self.save = None;
                if let (Some(path), Some(r)) = (res, &cx.sweep.result) {
                    let mut s = String::from("time_ms,amplitude\n");
                    for (i, v) in r.ir.iter().enumerate() {
                        s += &format!("{:.4},{:.8}\n", (i as f32 - r.pre as f32) / r.sr * 1000.0, v);
                    }
                    cx.req.note = Some(match std::fs::write(&path, s) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let Some(r) = &cx.sweep.result else {
            ui.add_space(8.0);
            ui.label(RichText::new("No measurement yet. Run one in the Response window (the sweep also gives the impulse response).").weak());
            return;
        };
        if self.ana.as_ref().map(|a| a.id) != Some(r.id) {
            self.refresh(r);
        }
        ui.horizontal_wrapped(|ui| {
            for (i, v) in VIEWS.iter().enumerate() {
                ui.selectable_value(&mut self.view, i, *v);
            }
            ui.add(egui::Slider::new(&mut self.zoom_ms, 0.0..=1500.0).logarithmic(true).text("view ms").custom_formatter(|v, _| if v < 1.0 { "auto".into() } else { format!("{v:.0}") }));
            if ui.add_enabled(self.save.is_none(), egui::Button::new("CSV")).on_hover_text("Export the impulse response").clicked() {
                self.save = Some(dialogs::save("Export impulse response", "CSV", &["csv"], "impulse.csv".into()));
            }
        });
        let Some(a) = &self.ana else {
            ui.label("impulse response too short");
            return;
        };
        let m = &a.m;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (cap, val) in [
                ("EDT", rt(m.edt)),
                ("T20", rt(m.t20)),
                ("T30", rt(m.t30)),
                ("C50", format!("{:.1} dB", m.c50)),
                ("C80", format!("{:.1} dB", m.c80)),
                ("D50", format!("{:.0} %", m.d50)),
                ("peak/noise", format!("{:.0} dB", m.snr_db)),
            ] {
                super::plot::readout(ui, cap, &val, Color32::from_rgb(210, 220, 235));
            }
        });
        if m.snr_db < 35.0 {
            ui.colored_label(Color32::from_rgb(255, 190, 80), "Low signal-to-noise: T30 / EDT may be unreliable. Raise the level or average more runs.");
        }

        let (resp, painter, rect) = canvas(ui, 120.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let span = if self.zoom_ms >= 1.0 { self.zoom_ms } else { (m.usable_ms * 1.15).clamp(20.0, 4000.0) };
        let t_pre = a.pre as f32 / a.sr * 1000.0;
        let x0 = -t_pre.min(span * 0.1);
        let ir = &r.ir;
        let (y0, y1, unit) = match self.view {
            0 => {
                let pk = ir.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
                (-pk * 1.1, pk * 1.1, "")
            }
            1 => (-100.0, 3.0, "dB"),
            _ => (-80.0, 3.0, "dB"),
        };
        let plot = Plot::new(pr, x0, span, y0, y1, false);
        plot.grid_y(&painter, &|v| if self.view == 0 { format!("{v:.2}") } else { format!("{v:.0}") });
        plot.grid_x(&painter, &|v| format!("{v:.0}"));
        plot.frame(&painter);
        let t_of = |i: usize, off: usize| (i as f32 - off as f32) / a.sr * 1000.0;
        match self.view {
            0 => {
                // decimate to pixels (min/max pairs)
                let w = pr.width() as usize;
                let (i0, i1) = (((x0 / 1000.0 * a.sr) as isize + a.pre as isize).max(0) as usize, (((span / 1000.0) * a.sr) as usize + a.pre).min(ir.len()));
                let clip = painter.with_clip_rect(pr);
                for x in 0..w {
                    let (s, e) = (i0 + x * (i1 - i0) / w, (i0 + (x + 1) * (i1 - i0) / w).max(i0 + x * (i1 - i0) / w + 1).min(ir.len()));
                    if s >= e {
                        continue;
                    }
                    let (lo, hi) = ir[s..e].iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
                    let px = pr.left() + x as f32 + 0.5;
                    clip.line_segment([Pos2::new(px, plot.ty(hi)), Pos2::new(px, plot.ty(lo) + 0.5)], Stroke::new(1.0, Color32::from_rgb(90, 200, 255)));
                }
            }
            1 => {
                let k = ((0.001 * a.sr) as usize).max(1);
                plot.line(&painter, a.etc.iter().enumerate().map(|(i, v)| (t_of(i * k + a.peak_i, a.pre), *v)), Stroke::new(1.3, Color32::from_rgb(90, 200, 255)));
                plot.line(&painter, a.edc.iter().enumerate().step_by(((a.edc.len() / 2000) + 1).max(1)).map(|(i, v)| (t_of(i + a.peak_i, a.pre), *v)), Stroke::new(1.3, Color32::from_rgb(255, 170, 70)));
            }
            _ => {
                plot.line(&painter, a.edc.iter().enumerate().step_by(((a.edc.len() / 2000) + 1).max(1)).map(|(i, v)| (t_of(i + a.peak_i, a.pre), *v)), Stroke::new(1.6, Color32::from_rgb(255, 170, 70)));
                for (lvl, name) in [(-5.0, "-5"), (-25.0, "-25"), (-35.0, "-35")] {
                    let y = plot.ty(lvl);
                    painter.line_segment([Pos2::new(pr.left(), y), Pos2::new(pr.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(35)));
                    painter.text(Pos2::new(pr.right() - 3.0, y - 1.0), Align2::RIGHT_BOTTOM, name, FontId::proportional(9.0), Color32::from_white_alpha(90));
                }
                // regression lines for T20 / T30
                for (t, lo, hi, col) in [(m.t20, -5.0f32, -25.0f32, Color32::from_rgb(120, 230, 150)), (m.t30, -5.0, -35.0, Color32::from_rgb(230, 130, 230))] {
                    if let Some(t) = t {
                        let slope = -60.0 / t;
                        let ia = a.edc.iter().position(|&v| v <= lo).unwrap_or(0);
                        let ta = ia as f32 / a.sr;
                        let tb = ta + (hi - lo) / slope;
                        let p0 = Pos2::new(plot.tx(t_of(a.peak_i, a.pre) + ta * 1000.0), plot.ty(lo));
                        let p1 = Pos2::new(plot.tx(t_of(a.peak_i, a.pre) + tb * 1000.0), plot.ty(hi));
                        painter.with_clip_rect(pr).line_segment([p0, p1], Stroke::new(1.0, col));
                    }
                }
            }
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, if unit.is_empty() { "amp" } else { unit }, FontId::proportional(9.0), crate::views::TXT);
        painter.text(Pos2::new(pr.right(), rect.bottom() - 1.0), Align2::RIGHT_BOTTOM, "ms", FontId::proportional(9.0), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{:.1} ms", plot.fx(h.x)), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("view", self.view);
        o.put("zoom", self.zoom_ms);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("view") {
            self.view = v.min(2);
        }
        i.set("zoom", &mut self.zoom_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt60_of_exponential_decay() {
        // noise decaying 60 dB in 0.8 s  => RT60 = 0.8 s
        let sr = 48000.0;
        let n = (1.5 * sr) as usize;
        let mut seed = 12345u64;
        let ir: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let r = ((seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                let env = 10f32.powf(-3.0 * (i as f32 / sr) / 0.8);
                r * env + if i == 0 { 5.0 } else { 0.0 } * 0.0
            })
            .collect();
        let (_, _, _, m) = analyse(&ir, sr).unwrap();
        let t30 = m.t30.expect("t30");
        let t20 = m.t20.expect("t20");
        assert!((t30 - 0.8).abs() < 0.08, "T30 {t30}");
        assert!((t20 - 0.8).abs() < 0.1, "T20 {t20}");
    }
}
