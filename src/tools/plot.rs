//! Small plotting kit shared by the tool windows (axes, grid, lines, canvas).

use crate::app::fmt_hz_short;
use crate::views::{nice_step, BG, GRID, GRID_MAJOR, TXT};
use eframe::egui::{self, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

pub struct Plot {
    pub r: Rect,
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub xlog: bool,
}

/// Allocate the remaining space of `ui` (at least `min_h` high) and paint the background.
pub fn canvas(ui: &mut Ui, min_h: f32) -> (Response, Painter, Rect) {
    let size = vec2(ui.available_width().max(60.0), ui.available_height().max(min_h));
    let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    painter.rect_filled(resp.rect, 0.0, BG);
    let rect = resp.rect;
    (resp, painter, rect)
}

/// Like `canvas` with an explicit height (for stacking several plots).
pub fn canvas_h(ui: &mut Ui, h: f32) -> (Response, Painter, Rect) {
    let (resp, painter) = ui.allocate_painter(vec2(ui.available_width().max(60.0), h.max(40.0)), Sense::click_and_drag());
    painter.rect_filled(resp.rect, 0.0, BG);
    let rect = resp.rect;
    (resp, painter, rect)
}

/// Plot rectangle inside a canvas, leaving room for axis labels.
pub fn inset(r: Rect) -> Rect {
    Rect::from_min_max(r.min + vec2(44.0, 6.0), r.max - vec2(8.0, 18.0))
}

impl Plot {
    pub fn new(r: Rect, x0: f32, x1: f32, y0: f32, y1: f32, xlog: bool) -> Self {
        Self { r, x0, x1, y0, y1, xlog }
    }

    pub fn tx(&self, x: f32) -> f32 {
        let (a, b, v) = if self.xlog { (self.x0.max(1e-6).ln(), self.x1.max(1e-6).ln(), x.max(1e-6).ln()) } else { (self.x0, self.x1, x) };
        self.r.left() + (v - a) / (b - a).max(1e-12) * self.r.width()
    }

    pub fn fx(&self, px: f32) -> f32 {
        let t = (px - self.r.left()) / self.r.width().max(1.0);
        if self.xlog {
            let (a, b) = (self.x0.max(1e-6).ln(), self.x1.max(1e-6).ln());
            (a + t * (b - a)).exp()
        } else {
            self.x0 + t * (self.x1 - self.x0)
        }
    }

    pub fn ty(&self, y: f32) -> f32 {
        self.r.bottom() - (y - self.y0) / (self.y1 - self.y0).max(1e-12) * self.r.height()
    }

    pub fn fy(&self, py: f32) -> f32 {
        self.y0 + (self.r.bottom() - py) / self.r.height().max(1.0) * (self.y1 - self.y0)
    }

    pub fn grid_y(&self, p: &Painter, fmt: &dyn Fn(f32) -> String) {
        let step = nice_step(self.y1 - self.y0, (self.r.height() / 34.0).clamp(3.0, 10.0));
        let mut v = (self.y0 / step).ceil() * step;
        let clip = p.with_clip_rect(self.r.expand(1.0));
        while v <= self.y1 {
            let y = self.ty(v);
            clip.line_segment([Pos2::new(self.r.left(), y), Pos2::new(self.r.right(), y)], Stroke::new(1.0, if v.abs() < 1e-6 { GRID_MAJOR } else { GRID }));
            p.text(Pos2::new(self.r.left() - 4.0, y), Align2::RIGHT_CENTER, fmt(v), FontId::proportional(10.0), TXT);
            v += step;
        }
    }

    /// Vertical grid; log axes get decade lines, linear axes nice steps. `fmt` labels major lines.
    pub fn grid_x(&self, p: &Painter, fmt: &dyn Fn(f32) -> String) {
        let clip = p.with_clip_rect(self.r.expand(1.0));
        let mut line = |x: f32, major: bool, label: bool| {
            let px = self.tx(x);
            if px < self.r.left() - 0.5 || px > self.r.right() + 0.5 {
                return;
            }
            clip.line_segment([Pos2::new(px, self.r.top()), Pos2::new(px, self.r.bottom())], Stroke::new(1.0, if major { GRID_MAJOR } else { GRID }));
            if label {
                p.text(Pos2::new(px, self.r.bottom() + 2.0), Align2::CENTER_TOP, fmt(x), FontId::proportional(10.0), TXT);
            }
        };
        if self.xlog {
            let decades = (self.x1 / self.x0.max(1e-6)).log10();
            for e in (self.x0.max(1e-6).log10().floor() as i32)..=(self.x1.log10().ceil() as i32) {
                for m in 1..10 {
                    let f = m as f32 * 10f32.powi(e);
                    let label = m == 1 || (decades < 2.2 && (m == 2 || m == 5)) || decades < 0.7;
                    line(f, m == 1, label);
                }
            }
        } else {
            let step = nice_step(self.x1 - self.x0, (self.r.width() / 70.0).clamp(3.0, 12.0));
            let mut x = (self.x0 / step).ceil() * step;
            while x <= self.x1 {
                line(x, true, true);
                x += step;
            }
        }
    }

    pub fn frame(&self, p: &Painter) {
        p.rect_stroke(self.r, 0.0, Stroke::new(1.0, GRID_MAJOR), egui::StrokeKind::Inside);
    }

    /// Polyline clipped to the plot; NaN / infinite points break the line.
    pub fn line(&self, p: &Painter, pts: impl Iterator<Item = (f32, f32)>, stroke: Stroke) {
        let clip = p.with_clip_rect(self.r.expand(1.0));
        let mut seg: Vec<Pos2> = Vec::new();
        for (x, y) in pts {
            if !x.is_finite() || !y.is_finite() {
                if seg.len() > 1 {
                    clip.add(Shape::line(std::mem::take(&mut seg), stroke));
                }
                seg.clear();
                continue;
            }
            seg.push(Pos2::new(self.tx(x), self.ty(y).clamp(self.r.top() - 3.0, self.r.bottom() + 3.0)));
        }
        if seg.len() > 1 {
            clip.add(Shape::line(seg, stroke));
        }
    }

    pub fn label(&self, p: &Painter, pos: Pos2, text: String, col: Color32, align: Align2) {
        let g = p.layout_no_wrap(text, FontId::proportional(11.0), col);
        let r = align.anchor_size(pos, g.size());
        p.rect_filled(r.expand(2.0), 2.0, Color32::from_black_alpha(175));
        p.galley(r.min, g, col);
    }
}

pub fn hz_label(f: f32) -> String {
    fmt_hz_short(f)
}

pub fn lerp_col(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Compact section heading used inside tool windows.
pub fn heading(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).strong().color(Color32::from_rgb(190, 200, 215)));
}

/// Big number with a small caption underneath.
pub fn readout(ui: &mut Ui, caption: &str, value: &str, col: Color32) {
    readout_sized(ui, caption, value, col, 22.0);
}

pub fn readout_sized(ui: &mut Ui, caption: &str, value: &str, col: Color32, size: f32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add(egui::Label::new(egui::RichText::new(value).size(size).strong().color(col).monospace()).wrap_mode(egui::TextWrapMode::Extend));
        ui.add(egui::Label::new(egui::RichText::new(caption).small().weak()).wrap_mode(egui::TextWrapMode::Extend));
    });
}
