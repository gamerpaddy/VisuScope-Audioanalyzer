//! Impulse response viewer with Schroeder decay, RT60 / EDT and clarity metrics.

use super::plot::{canvas, inset, Plot};
use super::room;
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const VIEWS: [&str; 7] = ["Waveform", "Energy (dB)", "Decay", "Bands", "Waterfall", "Speech", "Modes"];

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
    // computed when their view is first opened
    bands: Option<Vec<room::BandRes>>,
    csd: Option<Option<room::Csd>>,
    sti: Option<Option<room::Sti>>,
    modes: Option<Option<room::Modes>>,
}

pub struct Impulse {
    view: usize,
    zoom_ms: f32,
    ana: Option<Analysis>,
    save: Option<dialogs::Pending>,
    csd_tex: Option<(egui::TextureHandle, u64, u32)>,
    csd_range: f32,
    /// room length, width, height (m) for the theoretical axial modes
    room: [f32; 3],
    show_room: bool,
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
        Self { view: 1, zoom_ms: 0.0, ana: None, save: None, csd_tex: None, csd_range: 50.0, room: [5.0, 4.0, 2.6], show_room: false }
    }

    /// Bands, waterfall, speech intelligibility and room modes (views 3..=6).
    fn room_view(&self, ui: &mut Ui, a: &Analysis) {
        match self.view {
            3 => {
                let Some(bands) = &a.bands else { return };
                egui::Grid::new("imp_bands").num_columns(6).spacing([16.0, 3.0]).show(ui, |ui| {
                    for h in ["Band", "EDT", "T20", "T30", "C50", "C80"] {
                        ui.label(RichText::new(h).small().weak());
                    }
                    ui.end_row();
                    for b in bands {
                        ui.label(if b.fc >= 1000.0 { format!("{} kHz", b.fc / 1000.0) } else { format!("{} Hz", b.fc) });
                        match &b.m {
                            Some(m) => {
                                for v in [rt(m.edt), rt(m.t20), rt(m.t30), format!("{:.1} dB", m.c50), format!("{:.1} dB", m.c80)] {
                                    ui.label(v);
                                }
                            }
                            None => {
                                ui.label(RichText::new("no data above the noise (or band above Nyquist)").small().weak());
                            }
                        }
                        ui.end_row();
                    }
                });
                let (_, painter, rect) = canvas(ui, 90.0);
                let pr = inset(rect);
                if pr.width() < 60.0 || pr.height() < 30.0 {
                    return;
                }
                let top = bands.iter().filter_map(|b| b.m.as_ref()).flat_map(|m| [m.edt, m.t20, m.t30]).flatten().fold(0.5f32, f32::max) * 1.1;
                let plot = Plot::new(pr, 0.0, bands.len() as f32, 0.0, top, false);
                plot.grid_y(&painter, &|v| format!("{v:.1}"));
                plot.frame(&painter);
                let cols = [Color32::from_rgb(255, 170, 70), Color32::from_rgb(120, 230, 150), Color32::from_rgb(90, 200, 255)];
                for (i, b) in bands.iter().enumerate() {
                    let x = plot.tx(i as f32);
                    let w = (plot.tx(1.0) - plot.tx(0.0)) / 4.0;
                    if let Some(m) = &b.m {
                        for (j, v) in [m.edt, m.t20, m.t30].iter().enumerate() {
                            if let Some(v) = v {
                                let r = egui::Rect::from_min_max(Pos2::new(x + w * (0.5 + j as f32), plot.ty(*v)), Pos2::new(x + w * (1.5 + j as f32) - 1.0, pr.bottom()));
                                painter.rect_filled(r, 1.0, cols[j]);
                            }
                        }
                    }
                    painter.text(Pos2::new(x + w * 2.0, pr.bottom() + 2.0), Align2::CENTER_TOP, crate::app::fmt_hz_short(b.fc), FontId::proportional(10.0), crate::views::TXT);
                }
                for (j, n) in ["EDT", "T20", "T30"].iter().enumerate() {
                    painter.text(Pos2::new(pr.left() + 6.0 + j as f32 * 40.0, pr.top() + 4.0), Align2::LEFT_TOP, *n, FontId::proportional(10.0), cols[j]);
                }
                painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "s", FontId::proportional(9.0), crate::views::TXT);
            }
            4 => {
                let (Some(Some(c)), Some((tex, _, _))) = (&a.csd, &self.csd_tex) else {
                    ui.label(RichText::new("impulse response too short for a waterfall").weak());
                    return;
                };
                let (resp, painter, rect) = canvas(ui, 140.0);
                let pr = inset(rect);
                if pr.width() < 40.0 || pr.height() < 30.0 {
                    return;
                }
                let plot = Plot::new(pr, c.f0, c.f1, c.span_ms, 0.0, true);
                painter.image(tex.id(), pr, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
                plot.grid_y(&painter, &|v| format!("{v:.0}"));
                plot.frame(&painter);
                painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "ms", FontId::proportional(9.0), Color32::from_white_alpha(160));
                if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                    let (f, t) = (plot.fx(h.x), plot.fy(h.y));
                    let col = (((f / c.f0).ln() / (c.f1 / c.f0).ln()) * (c.cols - 1) as f32).round().clamp(0.0, (c.cols - 1) as f32) as usize;
                    let row = ((t / c.span_ms) * c.rows as f32).floor().clamp(0.0, (c.rows - 1) as f32) as usize;
                    plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{}   {:.0} ms   {:.1} dB", crate::app::fmt_hz(f), t, c.db[row * c.cols + col]), Color32::WHITE, Align2::RIGHT_TOP);
                }
            }
            5 => {
                let Some(Some(s)) = &a.sti else {
                    ui.label(RichText::new("STI needs all octave bands from 125 Hz to 8 kHz below Nyquist, and a usable decay.").weak());
                    return;
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 24.0;
                    let col = |v: f32| super::plot::lerp_col(Color32::from_rgb(255, 110, 90), Color32::from_rgb(120, 230, 150), (v - 0.3) / 0.45);
                    super::plot::readout(ui, &format!("STI ({})", room::sti_rating(s.sti)), &format!("{:.2}", s.sti), col(s.sti));
                    super::plot::readout(ui, "%ALcons", &format!("{:.1} %", s.alcons), Color32::from_rgb(210, 220, 235));
                });
                let (_, painter, rect) = canvas(ui, 90.0);
                let pr = inset(rect);
                if pr.width() < 60.0 || pr.height() < 30.0 {
                    return;
                }
                let plot = Plot::new(pr, 0.0, 7.0, 0.0, 1.0, false);
                plot.grid_y(&painter, &|v| format!("{v:.1}"));
                plot.frame(&painter);
                for (i, &m) in s.mti.iter().enumerate() {
                    let x = plot.tx(i as f32);
                    let w = plot.tx(1.0) - plot.tx(0.0);
                    painter.rect_filled(egui::Rect::from_min_max(Pos2::new(x + w * 0.2, plot.ty(m)), Pos2::new(x + w * 0.8, pr.bottom())), 1.0, Color32::from_rgb(90, 200, 255));
                    painter.text(Pos2::new(x + w * 0.5, pr.bottom() + 2.0), Align2::CENTER_TOP, crate::app::fmt_hz_short(room::BANDS[i]), FontId::proportional(10.0), crate::views::TXT);
                    painter.text(Pos2::new(x + w * 0.5, plot.ty(m) - 2.0), Align2::CENTER_BOTTOM, format!("{m:.2}"), FontId::proportional(10.0), Color32::WHITE);
                }
                painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "MTI", FontId::proportional(9.0), crate::views::TXT);
                ui.label(RichText::new("Indirect STI from the impulse response (modulation transfer, male weighting). No background noise, masking or level weighting: it describes the room only.").small().weak());
            }
            _ => {
                let Some(Some(m)) = &a.modes else {
                    ui.label(RichText::new("impulse response too short for a modal analysis").weak());
                    return;
                };
                let (resp, painter, rect) = canvas(ui, 120.0);
                let split = (rect.height() - 4.0).max(60.0);
                let pr = inset(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split)));
                if pr.width() < 40.0 || pr.height() < 30.0 {
                    return;
                }
                let plot = Plot::new(pr, 20.0, room::MODE_FMAX, -40.0, 3.0, true);
                plot.grid_y(&painter, &|v| format!("{v:.0}"));
                plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
                plot.frame(&painter);
                if self.show_room {
                    let cols = [Color32::from_rgb(255, 214, 10), Color32::from_rgb(120, 255, 120), Color32::from_rgb(190, 130, 255)];
                    for (d, dim) in self.room.iter().enumerate() {
                        for n in 1..=4 {
                            let f = 343.0 * n as f32 / (2.0 * dim.max(0.3));
                            if f < 20.0 || f > room::MODE_FMAX {
                                continue;
                            }
                            let x = plot.tx(f);
                            painter.with_clip_rect(pr).line_segment([Pos2::new(x, pr.top()), Pos2::new(x, pr.bottom())], Stroke::new(1.0, cols[d].gamma_multiply(0.45)));
                            painter.text(Pos2::new(x + 2.0, pr.top() + 2.0 + d as f32 * 10.0), Align2::LEFT_TOP, format!("{}{}", ["L", "W", "H"][d], n), FontId::proportional(9.0), cols[d]);
                        }
                    }
                }
                plot.line(&painter, m.f.iter().copied().zip(m.db.iter().copied()), Stroke::new(1.4, Color32::from_rgb(90, 200, 255)));
                for p in &m.peaks {
                    let (x, y) = (plot.tx(p.f), plot.ty(p.db));
                    painter.circle_stroke(Pos2::new(x, y), 3.5, Stroke::new(1.2, Color32::from_rgb(255, 170, 70)));
                    painter.text(Pos2::new(x, y - 6.0), Align2::CENTER_BOTTOM, format!("{:.0}", p.f), FontId::proportional(9.5), Color32::from_rgb(255, 190, 110));
                }
                painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
                if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                    plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{:.1} Hz", plot.fx(h.x)), Color32::WHITE, Align2::RIGHT_TOP);
                }
                let list: Vec<String> = m.peaks.iter().map(|p| format!("{:.0} Hz ({:.1} dB, T60 {:.2} s)", p.f, p.db, p.t60)).collect();
                ui.label(RichText::new(format!("Resonances: {}", list.join("   "))).small());
            }
        }
    }

    /// Compute what the active view needs (once per measurement).
    fn ensure(&mut self, ir: &[f32], ctx: &egui::Context) {
        let Some(a) = self.ana.as_mut() else { return };
        let cut = (a.m.usable_ms * a.sr / 1000.0) as usize;
        match self.view {
            3 if a.bands.is_none() => a.bands = Some(room::band_decays(ir, a.sr)),
            4 if a.csd.is_none() => a.csd = Some(room::csd(ir, a.sr, a.peak_i, cut)),
            5 if a.sti.is_none() => a.sti = Some(room::sti(ir, a.sr, a.peak_i, cut)),
            6 if a.modes.is_none() => a.modes = Some(room::modes(ir, a.sr, a.peak_i)),
            _ => {}
        }
        if self.view == 4 {
            if let Some(Some(c)) = &a.csd {
                let key = (a.id, self.csd_range.to_bits());
                if self.csd_tex.as_ref().map(|t| (t.1, t.2)) != Some(key) {
                    let lut = crate::colormap::Cmap::Inferno.lut();
                    let px: Vec<Color32> = c.db.iter().map(|&v| lut[(((v + self.csd_range) / self.csd_range).clamp(0.0, 1.0) * 255.0) as usize]).collect();
                    let img = egui::ColorImage::new([c.cols, c.rows], px);
                    match &mut self.csd_tex {
                        Some(t) => t.0.set(img, egui::TextureOptions::LINEAR),
                        None => self.csd_tex = Some((ctx.load_texture("impulse_csd", img, egui::TextureOptions::LINEAR), 0, 0)),
                    }
                    if let Some(t) = &mut self.csd_tex {
                        t.1 = key.0;
                        t.2 = key.1;
                    }
                }
            }
        }
    }

    fn refresh(&mut self, r: &super::sweep::SweepResult) {
        self.ana = analyse(&r.ir, r.sr).map(|(pk, etc, edc, m)| Analysis { id: r.id, sr: r.sr, pre: r.pre, peak_i: pk, etc, edc, m, bands: None, csd: None, sti: None, modes: None });
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
        self.ensure(&r.ir, ui.ctx());
        ui.horizontal_wrapped(|ui| {
            for (i, v) in VIEWS.iter().enumerate() {
                ui.selectable_value(&mut self.view, i, *v);
            }
            ui.add(egui::Slider::new(&mut self.zoom_ms, 0.0..=1500.0).logarithmic(true).text("view ms").custom_formatter(|v, _| if v < 1.0 { "auto".into() } else { format!("{v:.0}") }));
            if self.view == 4 {
                ui.add(egui::DragValue::new(&mut self.csd_range).range(20.0..=90.0).speed(0.5).prefix("range ").suffix(" dB"));
            }
            if self.view == 6 {
                ui.checkbox(&mut self.show_room, "Room modes").on_hover_text("Axial modes of a rectangular room (c = 343 m/s)");
                for (i, n) in ["L", "W", "H"].iter().enumerate() {
                    ui.add_enabled(self.show_room, egui::DragValue::new(&mut self.room[i]).range(0.5..=60.0).speed(0.05).prefix(format!("{n} ")).suffix(" m"));
                }
            }
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

        if self.view >= 3 {
            self.room_view(ui, a);
            return;
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

    fn report(&self, cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        let Some(r) = &cx.sweep.result else { return Vec::new() };
        let Some((pk, etc, edc, m)) = analyse(&r.ir, r.sr) else { return Vec::new() };
        let row = |a: &str, b: String| vec![a.to_string(), b];
        let mut blocks = vec![Block::Table {
            head: vec!["Metric".into(), "Value".into()],
            rows: vec![
                row("EDT", rt(m.edt)),
                row("T20", rt(m.t20)),
                row("T30", rt(m.t30)),
                row("C50", format!("{:.1} dB", m.c50)),
                row("C80", format!("{:.1} dB", m.c80)),
                row("D50", format!("{:.0} %", m.d50)),
                row("Peak to noise", format!("{:.0} dB", m.snr_db)),
                row("Usable decay length", format!("{:.0} ms", m.usable_ms)),
            ],
        }];
        let step = (edc.len() / 700).max(1);
        let edc_pts: Vec<(f32, f32)> = edc.iter().enumerate().step_by(step).map(|(i, v)| (i as f32 / r.sr * 1000.0, *v)).collect();
        let k = ((0.001 * r.sr) as usize).max(1);
        let etc_pts: Vec<(f32, f32)> = thin(etc.iter().enumerate().map(|(i, v)| ((i * k) as f32 / r.sr * 1000.0, *v)).collect(), 700);
        let span = (m.usable_ms * 1.1).clamp(20.0, 4000.0);
        blocks.push(Block::Plot(PlotSpec {
            title: "Energy time curve and Schroeder decay".into(),
            xlog: false,
            xlabel: "ms after the direct sound".into(),
            ylabel: "dB".into(),
            x: (0.0, span),
            y: (-100.0, 0.0),
            series: vec![Series { name: "energy (1 ms)".into(), color: COLORS[0], pts: etc_pts }, Series { name: "Schroeder decay".into(), color: COLORS[1], pts: edc_pts }],
        }));
        let bands = room::band_decays(&r.ir, r.sr);
        if bands.iter().any(|b| b.m.is_some()) {
            let rows = bands
                .iter()
                .map(|b| {
                    let l = if b.fc >= 1000.0 { format!("{} kHz", b.fc / 1000.0) } else { format!("{} Hz", b.fc) };
                    match &b.m {
                        Some(m) => vec![l, rt(m.edt), rt(m.t20), rt(m.t30), format!("{:.1} dB", m.c50), format!("{:.1} dB", m.c80)],
                        None => vec![l, "-".into(), "-".into(), "-".into(), "-".into(), "-".into()],
                    }
                })
                .collect();
            blocks.push(Block::Table { head: ["Octave band", "EDT", "T20", "T30", "C50", "C80"].iter().map(|s| s.to_string()).collect(), rows });
        }
        if let Some(s) = room::sti(&r.ir, r.sr, pk, (m.usable_ms * r.sr / 1000.0) as usize) {
            blocks.push(Block::Text(format!("Speech transmission index (indirect, male, no masking): STI {:.2} ({}), %ALcons {:.1} %", s.sti, room::sti_rating(s.sti), s.alcons)));
        }
        vec![Section { id: "Impulse", title: "Impulse response and room acoustics".into(), blocks }]
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("view", self.view);
        o.put("zoom", self.zoom_ms);
        o.put("csd_range", self.csd_range);
        o.put("show_room", self.show_room);
        o.put("room_l", self.room[0]);
        o.put("room_w", self.room[1]);
        o.put("room_h", self.room[2]);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("view") {
            self.view = v.min(VIEWS.len() - 1);
        }
        i.set("zoom", &mut self.zoom_ms);
        i.set("csd_range", &mut self.csd_range);
        i.set("show_room", &mut self.show_room);
        i.set("room_l", &mut self.room[0]);
        i.set("room_w", &mut self.room[1]);
        i.set("room_h", &mut self.room[2]);
        self.csd_range = self.csd_range.clamp(20.0, 90.0);
        for d in self.room.iter_mut() {
            *d = d.clamp(0.5, 60.0);
        }
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
