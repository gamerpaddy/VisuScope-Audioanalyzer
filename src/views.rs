use crate::app::*;
use crate::shared::*;
use eframe::egui::{self, epaint, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui};

pub const BG: Color32 = Color32::from_rgb(10, 12, 16);
pub const GRID: Color32 = Color32::from_rgb(34, 40, 50);
pub const GRID_MAJOR: Color32 = Color32::from_rgb(52, 60, 74);
pub const TXT: Color32 = Color32::from_rgb(150, 160, 175);

/// Like `f32::clamp` but never panics when `lo > hi` (small windows).
pub fn fclamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi.max(lo))
}

pub fn nice_step(range: f32, target: f32) -> f32 {
    let raw = (range / target).max(1e-12);
    let mag = 10f32.powf(raw.log10().floor());
    let n = raw / mag;
    let s = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    s * mag
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Linear-interpolated / peak-reduced lookup of a spectrum between fractional bins.
fn sample_bins(data: &[f32], b0: f32, b1: f32) -> Option<f32> {
    let half = (data.len() - 1) as f32;
    if b1 < 0.0 || b0 > half {
        return None;
    }
    if b1 - b0 < 1.0 {
        let mid = (0.5 * (b0 + b1)).clamp(0.0, half);
        let k = mid.floor() as usize;
        let fr = mid - k as f32;
        let k1 = (k + 1).min(data.len() - 1);
        Some(data[k] * (1.0 - fr) + data[k1] * fr)
    } else {
        let a = b0.max(0.0).floor() as usize;
        let b = (b1.ceil() as usize).min(data.len() - 1);
        let mut m = f32::MIN;
        for &v in &data[a..=b] {
            if v > m {
                m = v;
            }
        }
        Some(m)
    }
}

/// Chronological pieces (start, len) of a ring window ending `back` entries before `head`.
pub fn ring_pieces(head: usize, back: usize, span: usize, cap: usize) -> Vec<(usize, usize)> {
    let end = (head + cap - back % cap) % cap;
    let s = (end + cap - span % cap) % cap;
    if s + span <= cap {
        vec![(s, span)]
    } else {
        vec![(s, cap - s), (0, span - (cap - s))]
    }
}

impl App {
    pub fn freq_ticks(&self, flo: f32, fhi: f32) -> Vec<(f32, bool)> {
        let mut v = Vec::new();
        if self.p.log_freq {
            let decades = (fhi / flo).log10();
            for e in (flo.log10().floor() as i32)..=(fhi.log10().ceil() as i32) {
                for m in 1..10 {
                    let f = m as f32 * 10f32.powi(e);
                    if f < flo || f > fhi {
                        continue;
                    }
                    let label = m == 1 || (decades < 2.2 && (m == 2 || m == 5)) || decades < 0.7;
                    v.push((f, label));
                }
            }
        } else {
            let step = nice_step(fhi - flo, 8.0);
            let mut f = (flo / step).ceil() * step;
            while f <= fhi {
                v.push((f, true));
                f += step;
            }
        }
        v
    }

    /// Toolbar with painted icon buttons. Returns the requested action.
    pub fn header(&mut self, ui: &mut Ui, idx: usize, title: &str, ax: (&str, &str), tilt: bool) -> Act {
        let mut a = Act::default();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            let _ = title; // shown in the window title bar
            let lbl =|ui: &mut Ui, t: &str| ui.label(RichText::new(t).small().weak());
            lbl(ui, ax.0);
            if icon_btn(ui, Ico::ZoomIn, &format!("Zoom in ({})", ax.0), false) {
                a.zx = 0.6;
            }
            if icon_btn(ui, Ico::ZoomOut, &format!("Zoom out ({})", ax.0), false) {
                a.zx = 1.0 / 0.6;
            }
            ui.add_space(4.0);
            lbl(ui, ax.1);
            if icon_btn(ui, Ico::ZoomIn, &format!("Zoom in ({})", ax.1), false) {
                a.zy = 0.6;
            }
            if icon_btn(ui, Ico::ZoomOut, &format!("Zoom out ({})", ax.1), false) {
                a.zy = 1.0 / 0.6;
            }
            ui.add_space(4.0);
            ui.label(RichText::new("pan").small().weak());
            if icon_btn(ui, Ico::Left, "Pan left", false) {
                a.px = -0.2;
            }
            if icon_btn(ui, Ico::Right, "Pan right", false) {
                a.px = 0.2;
            }
            if icon_btn(ui, Ico::Up, "Pan up", false) {
                a.py = 0.2;
            }
            if icon_btn(ui, Ico::Down, "Pan down", false) {
                a.py = -0.2;
            }
            if tilt {
                ui.add_space(4.0);
                ui.label(RichText::new("tilt").small().weak());
                if icon_btn(ui, Ico::TiltUp, "Tilt up", false) {
                    a.tilt = 5.0;
                }
                if icon_btn(ui, Ico::TiltDown, "Tilt down", false) {
                    a.tilt = -5.0;
                }
                ui.label(RichText::new("rot").small().weak());
                if icon_btn(ui, Ico::RotL, "Rotate left", false) {
                    a.rot = -8.0;
                }
                if icon_btn(ui, Ico::RotR, "Rotate right", false) {
                    a.rot = 8.0;
                }
            }
            ui.add_space(4.0);
            if icon_btn(ui, Ico::Reset, "Reset / fit view", false) {
                a.reset = true;
            }
            if idx == 1 {
                let tip = if self.rotated { "Rotate to spectrogram (time horizontal)" } else { "Rotate to waterfall (time vertical)" };
                if icon_btn(ui, Ico::Rotate, tip, self.rotated) {
                    self.rotated = !self.rotated;
                }
            }
            ui.add_space(4.0);
            let single = self.wins[idx.min(crate::windows::WIN_COUNT - 1)].prev.is_some();
            let tip = if single { "Restore window size" } else { "Maximize window" };
            if icon_btn(ui, if single { Ico::Restore } else { Ico::Maximize }, tip, single) {
                self.max_req = Some(idx);
            }
        });
        a
    }

    fn draw_cursor_label(&self, painter: &Painter, pos: Pos2, text: String, col: Color32, align: Align2) {
        let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), col);
        let r = align.anchor_size(pos, galley.size());
        painter.rect_filled(r.expand(2.0), 2.0, Color32::from_black_alpha(170));
        painter.galley(r.min, galley, col);
    }

    // ------------------------------------------------------------------ spectrum

    pub fn panel_spectrum(&mut self, ui: &mut Ui) {
        let act = self.header(ui, 0, "Spectrum", ("Freq", "dB"), false);
        if act.reset {
            self.f_lo = 0.0;
            self.f_hi = 1.0;
            self.db_lo = -130.0;
            self.db_hi = 5.0;
        }
        if act.zx != 0.0 {
            self.zoom_f(act.zx);
        }
        if act.px != 0.0 {
            self.pan_f(act.px);
        }
        let range = self.db_hi - self.db_lo;
        if act.zy != 0.0 {
            let c = 0.5 * (self.db_hi + self.db_lo);
            let nr = (range * act.zy).clamp(10.0, 300.0);
            self.db_lo = c - nr * 0.5;
            self.db_hi = c + nr * 0.5;
        }
        if act.py != 0.0 {
            self.db_lo += act.py * range;
            self.db_hi += act.py * range;
        }

        let size = ui.available_size();
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, BG);
        let plot = Rect::from_min_max(rect.min + vec2(46.0, 6.0), rect.max - vec2(8.0, 18.0));
        if plot.width() < 20.0 || plot.height() < 20.0 {
            return;
        }
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
        let (flo, fhi) = (self.u_to_f(flo_u), self.u_to_f(fhi_u));
        let x_of_u = |u: f32| plot.left() + (u - flo_u) / (fhi_u - flo_u) * plot.width();
        let (dlo, dhi) = (self.db_lo, self.db_hi);
        let y_of_db = |d: f32| plot.bottom() - (d - dlo) / (dhi - dlo) * plot.height();

        // interaction
        if resp.dragged() {
            let d = resp.drag_delta();
            pan_view(&mut self.f_lo, &mut self.f_hi, -d.x / plot.width() * (fhi_u - flo_u));
            let dd = d.y / plot.height() * (dhi - dlo);
            self.db_lo += dd;
            self.db_hi += dd;
        }
        let hover = resp.hover_pos().filter(|p| plot.contains(*p));
        if let Some(hp) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let f = (-scroll * 0.004).exp();
                if ui.input(|i| i.modifiers.shift) {
                    let anchor = dhi - (hp.y - plot.top()) / plot.height() * (dhi - dlo);
                    let t = (anchor - dlo) / (dhi - dlo);
                    let nr = ((dhi - dlo) * f).clamp(10.0, 300.0);
                    self.db_lo = anchor - t * nr;
                    self.db_hi = self.db_lo + nr;
                } else {
                    let anchor = flo_u + (hp.x - plot.left()) / plot.width() * (fhi_u - flo_u);
                    zoom_view(&mut self.f_lo, &mut self.f_hi, f, anchor, 0.002);
                }
            }
        }
        let place = |app: &App, p: Pos2| -> f32 {
            let u = flo_u + (p.x - plot.left()) / plot.width() * (fhi_u - flo_u);
            let f = app.u_to_f(u);
            if app.snap_peak {
                let f2 = app.u_to_f(flo_u + (p.x + 8.0 - plot.left()) / plot.width() * (fhi_u - flo_u));
                app.snap_freq(f, (f2 - f).abs())
            } else {
                f
            }
        };
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.cur_a = Some(place(self, p));
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.cur_b = Some(place(self, p));
            }
        }

        let clip = painter.with_clip_rect(plot.expand(1.0));
        // dB grid
        let ds = nice_step(dhi - dlo, 8.0);
        let mut d = (dlo / ds).ceil() * ds;
        while d <= dhi {
            let y = y_of_db(d);
            clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, if d == 0.0 { GRID_MAJOR } else { GRID }));
            painter.text(Pos2::new(plot.left() - 4.0, y), Align2::RIGHT_CENTER, format!("{d:.0}"), FontId::proportional(10.0), TXT);
            d += ds;
        }
        for (f, label) in self.freq_ticks(flo, fhi) {
            let x = x_of_u(self.f_to_u(f));
            clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, if label { GRID_MAJOR } else { GRID }));
            if label {
                painter.text(Pos2::new(x, plot.bottom() + 2.0), Align2::CENTER_TOP, fmt_hz_short(f), FontId::proportional(10.0), TXT);
            }
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, GRID_MAJOR), egui::StrokeKind::Inside);
        painter.text(Pos2::new(rect.left() + 2.0, plot.top()), Align2::LEFT_TOP, self.unit_name(), FontId::proportional(9.0), TXT);

        if self.an.n == 0 {
            painter.text(plot.center(), Align2::CENTER_CENTER, "waiting for audio...", FontId::proportional(14.0), TXT);
            return;
        }

        // pixel -> bin boundaries
        let w = plot.width().floor().max(1.0) as usize;
        let bin_hz = self.an.sr / self.an.n as f32;
        let bpos: Vec<f32> = (0..=w)
            .map(|i| {
                let u = flo_u + (i as f32 / w as f32) * (fhi_u - flo_u);
                self.u_to_f(u) / bin_hz
            })
            .collect();
        let offs = [self.unit_off(0), self.unit_off(1)];
        let trace = |data: &[f32], off: f32| -> Vec<Pos2> {
            let mut pts = Vec::with_capacity(w);
            for i in 0..w {
                if let Some(v) = sample_bins(data, bpos[i], bpos[i + 1]) {
                    pts.push(Pos2::new(plot.left() + i as f32 + 0.5, y_of_db(v + off).clamp(plot.top() - 2.0, plot.bottom() + 2.0)));
                }
            }
            pts
        };
        let lw = self.line_w;

        for s in &self.snaps {
            if !s.visible {
                continue;
            }
            let sb = |hz: f32| hz / (s.sr / s.n as f32);
            let mut pts = Vec::with_capacity(w);
            for i in 0..w {
                if let Some(v) = sample_bins(&s.data, sb(bpos[i] * bin_hz), sb(bpos[i + 1] * bin_hz)) {
                    pts.push(Pos2::new(plot.left() + i as f32 + 0.5, y_of_db(v + offs[s.ch.min(1)]).clamp(plot.top() - 2.0, plot.bottom() + 2.0)));
                }
            }
            clip.add(Shape::line(pts, Stroke::new(1.0, s.color)));
        }

        for ch in 0..2 {
            if !self.show_ch[ch] {
                continue;
            }
            let base = CH_COLORS[ch];
            if self.show_min {
                clip.add(Shape::line(trace(&self.an.min[ch], offs[ch]), Stroke::new(lw * 0.8, lerp_color(BG, base, 0.45))));
            }
            if self.show_max {
                clip.add(Shape::line(trace(&self.an.max[ch], offs[ch]), Stroke::new(lw, Color32::from_rgb(255, 170, 60))));
            }
            if self.show_avg {
                clip.add(Shape::line(trace(&self.an.avg[ch], offs[ch]), Stroke::new(lw * 1.2, lerp_color(base, Color32::WHITE, 0.55))));
            }
            if self.show_live {
                let pts = trace(&self.an.live[ch], offs[ch]);
                if self.fill && pts.len() > 1 {
                    let mut mesh = epaint::Mesh::default();
                    let top = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 70);
                    let bot = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 0);
                    for p in &pts {
                        mesh.vertices.push(epaint::Vertex { pos: *p, uv: epaint::WHITE_UV, color: top });
                        mesh.vertices.push(epaint::Vertex { pos: Pos2::new(p.x, plot.bottom()), uv: epaint::WHITE_UV, color: bot });
                    }
                    for i in 0..(pts.len() as u32 - 1) {
                        let a = i * 2;
                        mesh.indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
                    }
                    clip.add(Shape::mesh(mesh));
                }
                clip.add(Shape::line(pts, Stroke::new(lw, base)));
            }
        }

        // harmonic markers
        let m = self.an.metrics[self.meas_ch];
        if m.valid {
            for h in 1..=self.p.harmonics.min(MAXH) {
                let f = m.harm_hz[h - 1];
                if f <= 0.0 || (h > 1 && m.harm_db[h - 1] < -199.0) {
                    continue;
                }
                let x = x_of_u(self.f_to_u(f));
                if x < plot.left() || x > plot.right() {
                    continue;
                }
                let c = Color32::from_rgba_unmultiplied(255, 255, 255, 120);
                clip.add(Shape::convex_polygon(
                    vec![Pos2::new(x - 4.0, plot.top()), Pos2::new(x + 4.0, plot.top()), Pos2::new(x, plot.top() + 7.0)],
                    c,
                    Stroke::NONE,
                ));
                clip.text(Pos2::new(x, plot.top() + 8.0), Align2::CENTER_TOP, format!("H{h}"), FontId::proportional(9.0), c);
            }
        }

        // tool overlays (peak labels, reference masks ...)
        {
            let (log, srv) = (self.p.log_freq, self.sr());
            let xf = |f: f32| x_of_u(freq_to_u(f, log, srv));
            let ov = crate::tools::SpecOverlay { p: &clip, plot, x_of_f: &xf, y_of_db: &y_of_db };
            self.tool_overlays(&ov);
        }

        // cursors
        for (name, c, f) in [("A", CUR_A, self.cur_a), ("B", CUR_B, self.cur_b)] {
            if let Some(f) = f {
                let x = x_of_u(self.f_to_u(f));
                if x < plot.left() || x > plot.right() {
                    continue;
                }
                clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, c));
                let lv = self.level_at(f, self.meas_ch).map(|v| v + offs[self.meas_ch.min(1)]);
                let txt = match lv {
                    Some(v) => format!("{name} {}  {v:.1} {}", fmt_hz(f), self.unit_name()),
                    None => format!("{name} {}", fmt_hz(f)),
                };
                let ty = fclamp(lv.map(|v| y_of_db(v)).unwrap_or(plot.top() + 20.0), plot.top() + 20.0, plot.bottom() - 10.0);
                self.draw_cursor_label(&clip, Pos2::new(x + 4.0, ty), txt, c, Align2::LEFT_CENTER);
            }
        }

        // hover crosshair
        if let Some(hp) = hover {
            clip.line_segment([Pos2::new(hp.x, plot.top()), Pos2::new(hp.x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(50)));
            let u = flo_u + (hp.x - plot.left()) / plot.width() * (fhi_u - flo_u);
            let f = self.u_to_f(u);
            let l = self.level_at(f, 0).map(|v| format!("{:.1}", v + offs[0])).unwrap_or("-".into());
            let r = self.level_at(f, 1).map(|v| format!("{:.1}", v + offs[1])).unwrap_or("-".into());
            self.draw_cursor_label(&painter, plot.right_top() + vec2(-4.0, 4.0), format!("{}   L {l}  R {r} {}", fmt_hz(f), self.unit_name()), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }

    // ------------------------------------------------------------------ spectrogram

    pub fn panel_spectrogram(&mut self, ui: &mut Ui) {
        let act = self.header(ui, 1, "Spectrogram", ("Freq", "Time"), false);
        if act.reset {
            self.f_lo = 0.0;
            self.f_hi = 1.0;
            self.sg_span = SG_W / 2;
            self.sg_back = 0;
        }
        if act.zx != 0.0 {
            self.zoom_f(act.zx);
        }
        if act.py != 0.0 {
            self.pan_f(act.py);
        }
        if act.zy != 0.0 {
            self.sg_span = ((self.sg_span as f32 * act.zy) as usize).clamp(64, SG_W);
        }
        if act.px != 0.0 {
            let d = (-act.px * self.sg_span as f32) as isize;
            self.sg_back = (self.sg_back as isize + d).clamp(0, (SG_W - self.sg_span) as isize) as usize;
        }
        self.sg_back = self.sg_back.min(SG_W - self.sg_span);

        let size = ui.available_size();
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, BG);
        let plot = Rect::from_min_max(rect.min + vec2(46.0, 4.0), rect.max - vec2(8.0, 18.0));
        if plot.width() < 20.0 || plot.height() < 20.0 {
            return;
        }
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
        let y_of_u = |u: f32| plot.bottom() - (u - flo_u) / (fhi_u - flo_u) * plot.height();
        let u_of_y = |y: f32| flo_u + (plot.bottom() - y) / plot.height() * (fhi_u - flo_u);

        if resp.dragged() {
            let d = resp.drag_delta();
            pan_view(&mut self.f_lo, &mut self.f_hi, d.y / plot.height() * (fhi_u - flo_u));
            let dc = (d.x / plot.width() * self.sg_span as f32) as isize;
            self.sg_back = (self.sg_back as isize + dc).clamp(0, (SG_W - self.sg_span) as isize) as usize;
        }
        let hover = resp.hover_pos().filter(|p| plot.contains(*p));
        if let Some(hp) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let f = (-scroll * 0.004).exp();
                if ui.input(|i| i.modifiers.shift) {
                    self.sg_span = ((self.sg_span as f32 * f) as usize).clamp(64, SG_W);
                } else {
                    zoom_view(&mut self.f_lo, &mut self.f_hi, f, u_of_y(hp.y), 0.002);
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let f = self.u_to_f(u_of_y(p.y));
                self.cur_a = Some(f);
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let f = self.u_to_f(u_of_y(p.y));
                self.cur_b = Some(f);
            }
        }
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);

        // image
        let span = self.sg_span;
        let pieces = ring_pieces(self.sg_head, self.sg_back, span, SG_W);
        let mut off = 0usize;
        for (s, len) in pieces {
            let x0 = plot.left() + off as f32 / span as f32 * plot.width();
            let x1 = plot.left() + (off + len) as f32 / span as f32 * plot.width();
            let uv = Rect::from_min_max(
                Pos2::new(s as f32 / SG_W as f32, 1.0 - fhi_u),
                Pos2::new((s + len) as f32 / SG_W as f32, 1.0 - flo_u),
            );
            painter.image(self.sg_tex.id(), Rect::from_min_max(Pos2::new(x0, plot.top()), Pos2::new(x1, plot.bottom())), uv, Color32::WHITE);
            off += len;
        }

        // axes
        let (flo, fhi) = (self.u_to_f(flo_u), self.u_to_f(fhi_u));
        let clip = painter.with_clip_rect(plot);
        for (f, label) in self.freq_ticks(flo, fhi) {
            let y = y_of_u(self.f_to_u(f));
            clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(if label { 28 } else { 10 })));
            if label {
                painter.text(Pos2::new(plot.left() - 4.0, y), Align2::RIGHT_CENTER, fmt_hz_short(f), FontId::proportional(10.0), TXT);
            }
        }
        let rate = self.p.cols_per_sec.min(self.p.update_hz).max(1.0);
        let spc = 1.0 / rate;
        let total_s = span as f32 * spc;
        let step = nice_step(total_s, 6.0);
        let t_new = self.sg_back as f32 * spc;
        let mut t = (t_new / step).ceil() * step;
        while t <= t_new + total_s {
            let x = plot.right() - (t - t_new) / total_s * plot.width();
            clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(14)));
            painter.text(Pos2::new(x, plot.bottom() + 2.0), Align2::CENTER_TOP, if t == 0.0 { "now".to_string() } else { format!("-{t:.1}s") }, FontId::proportional(10.0), TXT);
            t += step;
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, GRID_MAJOR), egui::StrokeKind::Inside);

        for (name, c, f) in [("A", CUR_A, self.cur_a), ("B", CUR_B, self.cur_b)] {
            if let Some(f) = f {
                let y = y_of_u(self.f_to_u(f));
                if y < plot.top() || y > plot.bottom() {
                    continue;
                }
                clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, c));
                self.draw_cursor_label(&clip, Pos2::new(plot.left() + 4.0, y - 8.0), format!("{name} {}", fmt_hz(f)), c, Align2::LEFT_CENTER);
            }
        }

        if let Some(hp) = hover {
            clip.line_segment([Pos2::new(plot.left(), hp.y), Pos2::new(plot.right(), hp.y)], Stroke::new(1.0, Color32::from_white_alpha(40)));
            clip.line_segment([Pos2::new(hp.x, plot.top()), Pos2::new(hp.x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(40)));
            let f = self.u_to_f(u_of_y(hp.y));
            let frac = (hp.x - plot.left()) / plot.width();
            let col = ((self.sg_head + SG_W * 2 - self.sg_back - span) as f32 + frac * span as f32) as usize % SG_W;
            let row_from_bottom = ((u_of_y(hp.y) * ROWS as f32) as usize).min(ROWS - 1);
            let v = self.sg_hist[col * ROWS + (ROWS - 1 - row_from_bottom)];
            let db = v as f32 / 255.0 * HIST_DB_RANGE - HIST_DB_RANGE;
            let ago = t_new + (1.0 - frac) * total_s;
            self.draw_cursor_label(&painter, plot.right_top() + vec2(-4.0, 4.0), format!("{}  {db:.1} dB  -{ago:.2}s", fmt_hz(f)), Color32::WHITE, Align2::RIGHT_TOP);
        }
        if self.sg_back > 0 {
            self.draw_cursor_label(&painter, plot.left_bottom() + vec2(4.0, -4.0), "scrolled back - drag right edge to return".into(), Color32::YELLOW, Align2::LEFT_BOTTOM);
        }
    }

    // ------------------------------------------------------------------ waterfall

    pub fn panel_waterfall(&mut self, ui: &mut Ui) {
        let act = self.header(ui, 1, "Waterfall", ("Freq", "Time"), false);
        if act.reset {
            self.f_lo = 0.0;
            self.f_hi = 1.0;
            self.wf_span = WF_H / 2;
            self.wf_back = 0;
        }
        if act.zx != 0.0 {
            self.zoom_f(act.zx);
        }
        if act.px != 0.0 {
            self.pan_f(act.px);
        }
        if act.zy != 0.0 {
            self.wf_span = ((self.wf_span as f32 * act.zy) as usize).clamp(32, WF_H);
        }
        if act.py != 0.0 {
            let d = (act.py * self.wf_span as f32) as isize;
            self.wf_back = (self.wf_back as isize + d).clamp(0, (WF_H - self.wf_span) as isize) as usize;
        }
        self.wf_back = self.wf_back.min(WF_H - self.wf_span);

        let size = ui.available_size();
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, BG);
        let plot = Rect::from_min_max(rect.min + vec2(34.0, 4.0), rect.max - vec2(8.0, 18.0));
        if plot.width() < 20.0 || plot.height() < 20.0 {
            return;
        }
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
        let x_of_u = |u: f32| plot.left() + (u - flo_u) / (fhi_u - flo_u) * plot.width();
        let u_of_x = |x: f32| flo_u + (x - plot.left()) / plot.width() * (fhi_u - flo_u);

        if resp.dragged() {
            let d = resp.drag_delta();
            pan_view(&mut self.f_lo, &mut self.f_hi, -d.x / plot.width() * (fhi_u - flo_u));
            // newest at top: dragging down shows older rows
            let dr = (d.y / plot.height() * self.wf_span as f32) as isize;
            self.wf_back = (self.wf_back as isize + dr).clamp(0, (WF_H - self.wf_span) as isize) as usize;
        }
        let hover = resp.hover_pos().filter(|p| plot.contains(*p));
        if let Some(hp) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let f = (-scroll * 0.004).exp();
                if ui.input(|i| i.modifiers.shift) {
                    self.wf_span = ((self.wf_span as f32 * f) as usize).clamp(32, WF_H);
                } else {
                    zoom_view(&mut self.f_lo, &mut self.f_hi, f, u_of_x(hp.x), 0.002);
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.cur_a = Some(self.u_to_f(u_of_x(p.x)));
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.cur_b = Some(self.u_to_f(u_of_x(p.x)));
            }
        }
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);

        let span = self.wf_span;
        let pieces = ring_pieces(self.wf_head, self.wf_back, span, WF_H);
        let mut off = 0usize;
        for (s, len) in pieces {
            // chronological rows [off, off+len); newest is at the top of the plot
            let y_top = plot.top() + (span - (off + len)) as f32 / span as f32 * plot.height();
            let y_bot = plot.top() + (span - off) as f32 / span as f32 * plot.height();
            let uv = Rect::from_min_max(
                Pos2::new(flo_u, (s + len) as f32 / WF_H as f32),
                Pos2::new(fhi_u, s as f32 / WF_H as f32),
            );
            painter.image(self.wf_tex.id(), Rect::from_min_max(Pos2::new(plot.left(), y_top), Pos2::new(plot.right(), y_bot)), uv, Color32::WHITE);
            off += len;
        }

        let (flo, fhi) = (self.u_to_f(flo_u), self.u_to_f(fhi_u));
        let clip = painter.with_clip_rect(plot);
        for (f, label) in self.freq_ticks(flo, fhi) {
            let x = x_of_u(self.f_to_u(f));
            clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(if label { 26 } else { 9 })));
            if label {
                painter.text(Pos2::new(x, plot.bottom() + 2.0), Align2::CENTER_TOP, fmt_hz_short(f), FontId::proportional(10.0), TXT);
            }
        }
        let rate = self.p.cols_per_sec.min(self.p.update_hz).max(1.0);
        let total_s = span as f32 / rate;
        let step = nice_step(total_s, 5.0);
        let t_new = self.wf_back as f32 / rate;
        let mut t = (t_new / step).ceil() * step;
        while t <= t_new + total_s {
            let y = plot.top() + (t - t_new) / total_s * plot.height();
            painter.text(Pos2::new(plot.left() - 3.0, y), Align2::RIGHT_CENTER, if t == 0.0 { "now".into() } else { format!("-{t:.1}s") }, FontId::proportional(9.0), TXT);
            clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(10)));
            t += step;
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, GRID_MAJOR), egui::StrokeKind::Inside);
        for (name, c, f) in [("A", CUR_A, self.cur_a), ("B", CUR_B, self.cur_b)] {
            if let Some(f) = f {
                let x = x_of_u(self.f_to_u(f));
                if x < plot.left() || x > plot.right() {
                    continue;
                }
                clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, c));
                self.draw_cursor_label(&clip, Pos2::new(x + 3.0, plot.top() + 8.0), format!("{name} {}", fmt_hz(f)), c, Align2::LEFT_CENTER);
            }
        }
        if let Some(hp) = hover {
            let f = self.u_to_f(u_of_x(hp.x));
            let frac = (hp.y - plot.top()) / plot.height();
            let row_back = (frac * span as f32) as usize;
            let idx = (self.wf_head + WF_H * 2 - 1 - self.wf_back - row_back.min(span - 1)) % WF_H;
            let bx = ((u_of_x(hp.x) * ROWS as f32) as usize).min(ROWS - 1);
            let db = self.wf_hist[idx * ROWS + bx] as f32 / 255.0 * HIST_DB_RANGE - HIST_DB_RANGE;
            let ago = t_new + frac * total_s;
            clip.line_segment([Pos2::new(hp.x, plot.top()), Pos2::new(hp.x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(40)));
            self.draw_cursor_label(&painter, plot.right_top() + vec2(-4.0, 4.0), format!("{}  {db:.1} dB  -{ago:.2}s", fmt_hz(f)), Color32::WHITE, Align2::RIGHT_TOP);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Ico {
    ZoomIn,
    ZoomOut,
    Left,
    Right,
    Up,
    Down,
    TiltUp,
    TiltDown,
    RotL,
    RotR,
    Reset,
    Rotate,
    Maximize,
    Restore,
}

fn tri(p: &Painter, c: Pos2, dir: (f32, f32), s: f32, col: Color32) {
    // triangle pointing along `dir`
    let (dx, dy) = dir;
    let (px, py) = (-dy, dx);
    let pts = vec![
        c + vec2(dx * s, dy * s),
        c + vec2(-dx * s * 0.7 + px * s, -dy * s * 0.7 + py * s),
        c + vec2(-dx * s * 0.7 - px * s, -dy * s * 0.7 - py * s),
    ];
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn arc(p: &Painter, c: Pos2, r: f32, flip: bool, st: Stroke, col: Color32) {
    let f = if flip { -1.0 } else { 1.0 };
    let n = 14;
    let (a0, a1) = (-0.6f32, 4.4f32);
    let pt = |a: f32| c + vec2(f * r * a.cos(), -r * a.sin());
    let pts: Vec<Pos2> = (0..=n).map(|i| pt(a0 + (a1 - a0) * i as f32 / n as f32)).collect();
    let end = pt(a1);
    let tangent = vec2(f * -(a1.sin()), -a1.cos()) * -1.0;
    let t = tangent.normalized();
    p.add(Shape::line(pts, st));
    tri(p, end, (t.x, t.y), 3.5, col);
}

/// Painted toolbar button. `active` draws it highlighted (toggle state).
pub fn icon_btn(ui: &mut Ui, ico: Ico, tip: &str, active: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    let hov = resp.hovered();
    let down = resp.is_pointer_button_down_on();
    let bg = if down {
        Color32::from_rgb(70, 115, 180)
    } else if active {
        Color32::from_rgb(45, 85, 140)
    } else if hov {
        Color32::from_rgb(56, 68, 88)
    } else {
        Color32::from_rgb(30, 36, 46)
    };
    let p = ui.painter();
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, if hov { Color32::from_rgb(110, 130, 160) } else { Color32::from_rgb(52, 60, 74) }), egui::StrokeKind::Inside);
    let col = if hov || active { Color32::WHITE } else { Color32::from_rgb(195, 205, 220) };
    let st = Stroke::new(1.6, col);
    let c = rect.center();
    match ico {
        Ico::ZoomIn | Ico::ZoomOut => {
            let lc = c + vec2(-1.5, -1.5);
            p.circle_stroke(lc, 5.5, st);
            p.line_segment([lc + vec2(4.0, 4.0), c + vec2(8.0, 8.0)], Stroke::new(2.2, col));
            p.line_segment([lc + vec2(-3.0, 0.0), lc + vec2(3.0, 0.0)], st);
            if ico == Ico::ZoomIn {
                p.line_segment([lc + vec2(0.0, -3.0), lc + vec2(0.0, 3.0)], st);
            }
        }
        Ico::Left => tri(p, c, (-1.0, 0.0), 5.5, col),
        Ico::Right => tri(p, c, (1.0, 0.0), 5.5, col),
        Ico::Up => tri(p, c, (0.0, -1.0), 5.5, col),
        Ico::Down => tri(p, c, (0.0, 1.0), 5.5, col),
        Ico::TiltUp | Ico::TiltDown => {
            let up = ico == Ico::TiltUp;
            let oy = if up { 3.0 } else { -3.0 };
            let plane = vec![c + vec2(-9.0, 3.0 + oy), c + vec2(-4.0, -3.0 + oy), c + vec2(9.0, -3.0 + oy), c + vec2(4.0, 3.0 + oy)];
            p.add(Shape::closed_line(plane, Stroke::new(1.4, col)));
            tri(p, c + vec2(0.0, if up { -8.0 } else { 8.0 }), (0.0, if up { -1.0 } else { 1.0 }), 3.2, col);
        }
        Ico::RotL => arc(p, c, 6.5, true, st, col),
        Ico::RotR | Ico::Rotate => arc(p, c, 6.5, false, st, col),
        Ico::Reset => {
            let s = 7.0;
            let k = 4.0;
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let corner = c + vec2(sx * s, sy * s);
                p.line_segment([corner, corner + vec2(-sx * k, 0.0)], st);
                p.line_segment([corner, corner + vec2(0.0, -sy * k)], st);
            }
            p.circle_filled(c, 1.8, col);
        }
        Ico::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, vec2(12.0, 11.0)), 1.0, st, egui::StrokeKind::Inside);
            p.line_segment([c + vec2(-6.0, -3.5), c + vec2(6.0, -3.5)], st);
        }
        Ico::Restore => {
            p.rect_stroke(Rect::from_center_size(c + vec2(-2.0, 2.0), vec2(9.0, 8.0)), 1.0, st, egui::StrokeKind::Inside);
            p.rect_stroke(Rect::from_center_size(c + vec2(2.0, -2.0), vec2(9.0, 8.0)), 1.0, st, egui::StrokeKind::Inside);
        }
    }
    resp.on_hover_text(tip).clicked()
}
