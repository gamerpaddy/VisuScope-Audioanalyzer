//! The Traces window: all stored curves of the measurement windows in one place, to look at, compare and manage.

use super::plot::{canvas, inset, Plot};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

pub struct TraceWin {
    y_lo: f32,
    y_hi: f32,
    auto: bool,
}

impl TraceWin {
    pub fn new() -> Self {
        Self { y_lo: -40.0, y_hi: 20.0, auto: true }
    }
}

impl Tool for TraceWin {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.auto, "Auto scale");
            ui.add_enabled_ui(!self.auto, |ui| {
                ui.add(egui::DragValue::new(&mut self.y_lo).range(-300.0..=self.y_hi - 5.0).speed(0.5).prefix("min "));
                ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 5.0..=200.0).speed(0.5).prefix("max "));
            });
            if let Some(r) = cx.lib.rel {
                let name = cx.lib.items.get(r).map(|m| m.name.clone()).unwrap_or_default();
                ui.label(RichText::new(format!("relative to {name}")).small().color(Color32::from_rgb(255, 214, 10)));
            }
        });
        let shown: Vec<usize> = (0..cx.lib.items.len()).filter(|&i| cx.lib.items[i].visible && cx.lib.items[i].f.len() >= 2).collect();
        // the plot
        let f0 = shown.iter().map(|&i| cx.lib.items[i].f[0]).fold(f32::MAX, f32::min).max(1.0);
        let f1 = shown.iter().map(|&i| *cx.lib.items[i].f.last().unwrap()).fold(0.0, f32::max);
        let (resp, painter, rect) = canvas(ui, 160.0);
        let list_h = 0.0f32;
        let _ = list_h;
        let pr = inset(rect);
        if pr.width() > 40.0 && pr.height() > 30.0 {
            if shown.is_empty() || f1 <= f0 * 1.01 {
                painter.text(pr.center(), Align2::CENTER_CENTER, "No trace to show.
Store one in the Response, Transfer or Distortion window (Store),
or load a file with Load...", FontId::proportional(12.0), crate::views::TXT);
            } else {
                let ppo = 24.0;
                let grid: Vec<f32> = (0..400).map(|i| f0 * (f1 / f0).powf(i as f32 / 399.0)).collect();
                let (layers, _) = cx.lib.layers(&grid, ppo, 0.0);
                // (layers are the visible traces, in the order of the list)
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for (_, _, v) in &layers {
                    for &x in v.iter().filter(|x| x.is_finite()) {
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                }
                let (y0, y1) = if self.auto && lo < hi { (lo - 3.0, hi + 3.0) } else { (self.y_lo, self.y_hi) };
                let plot = Plot::new(pr, f0, f1, y0, y1.max(y0 + 1.0), true);
                plot.grid_y(&painter, &|v| format!("{v:.0}"));
                plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
                plot.frame(&painter);
                for (_, col, v) in &layers {
                    plot.line(&painter, grid.iter().copied().zip(v.iter().copied()), Stroke::new(1.5, *col));
                }
                if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
                    let f = plot.fx(h.x);
                    let mut lines: Vec<(String, Color32)> = vec![(crate::app::fmt_hz(f), Color32::WHITE)];
                    for &i in &shown {
                        let m = &cx.lib.items[i];
                        let v = m.at(f);
                        if v.is_finite() {
                            lines.push((format!("{}  {:.1} dB", m.name, v), m.color));
                        }
                    }
                    // a box with one line per trace (a long line would be cut off at the edge of the plot)
                    let g: Vec<_> = lines.iter().map(|(t, c)| painter.layout_no_wrap(t.clone(), FontId::proportional(11.0), *c)).collect();
                    let w = g.iter().map(|g| g.size().x).fold(0.0, f32::max) + 8.0;
                    let hh: f32 = g.iter().map(|g| g.size().y).sum::<f32>() + 6.0;
                    let mut p = Pos2::new(pr.right() - w - 4.0, pr.top() + 4.0);
                    if h.x > p.x - 10.0 {
                        p.x = pr.left() + 4.0;
                    }
                    painter.rect_filled(egui::Rect::from_min_size(p, egui::vec2(w, hh)), 3.0, Color32::from_black_alpha(190));
                    let mut y = p.y + 3.0;
                    for (g, (_, c)) in g.into_iter().zip(&lines) {
                        let hgt = g.size().y;
                        painter.galley(Pos2::new(p.x + 4.0, y), g, *c);
                        y += hgt;
                    }
                    painter.line_segment([Pos2::new(h.x, pr.top()), Pos2::new(h.x, pr.bottom())], Stroke::new(1.0, Color32::from_white_alpha(50)));
                }
            }
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| cx.lib.manage(ui));
    }
}
