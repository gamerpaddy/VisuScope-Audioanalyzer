//! Drawing of the timeline: grid, ruler, clips, automation and the recording preview.

use super::*;

impl Daw {
    pub(super) fn draw_grid(&self, lp: &Painter, view: &View, lane: Rect) {
        let step = time_step(70.0 / view.pps);
        let t_left = (lane.left() - view.left) as f64 / view.pps + view.t0;
        let t_right = (lane.right() - view.left) as f64 / view.pps + view.t0;
        let mut t = (t_left / step).floor() * step;
        while t <= t_right {
            let x = view.left + ((t - view.t0) * view.pps) as f32;
            lp.line_segment([pos2(x, lane.top()), pos2(x, lane.bottom())], Stroke::new(1.0, GRID_LINE));
            t += step;
        }
    }

    /// The volume automation of a track: a line over its lane, with dots to drag while it is being edited.
    /// The drawing of a draw track: a zero line and the curve.
    pub(super) fn draw_curve(&self, lp: &Painter, view: &View, lane: Rect, row: Rect, t: &Track) {
        let y_of = |v: f32| row.bottom() - 4.0 - ((v + 1.0) * 0.5).clamp(0.0, 1.0) * (row.height() - 8.0);
        let col = track_color(t);
        let y0 = y_of(0.0);
        lp.line_segment([pos2(lane.left(), y0), pos2(lane.right(), y0)], Stroke::new(1.0, Color32::from_white_alpha(36)));
        if t.curve.is_empty() {
            lp.text(pos2(lane.left() + 6.0, row.center().y), Align2::LEFT_CENTER, "draw here with the mouse: a control signal from -1 to 1, not heard in the master mix", FontId::proportional(10.0), Color32::from_white_alpha(90));
            return;
        }
        let mut pts: Vec<Pos2> = Vec::with_capacity(t.curve.len() + 2);
        let first = t.curve[0];
        pts.push(pos2(lane.left(), y_of(first.1)));
        for (f, v) in &t.curve {
            let x = view.x(*f);
            // (points far outside the view are skipped, the line still passes through)
            if x > lane.left() - 200.0 && x < lane.right() + 200.0 || pts.len() == 1 {
                pts.push(pos2(x, y_of(*v)));
            }
        }
        let last = t.curve[t.curve.len() - 1];
        pts.push(pos2(lane.right(), y_of(last.1)));
        lp.add(Shape::line(pts, Stroke::new(1.8, col)));
    }

    /// The track of a drawing under a position (its index, and the value the height means).
    pub(super) fn draw_row_at(p: &Project, rows: &[Rect], pos: Pos2) -> Option<(usize, f32)> {
        let ti = rows.iter().position(|r| pos.y >= r.top() && pos.y < r.bottom())?;
        let t = p.tracks.get(ti).filter(|t| t.draw)?;
        let _ = t;
        let row = rows[ti];
        Some((ti, (((row.bottom() - 4.0 - pos.y) / (row.height() - 8.0).max(1.0)).clamp(0.0, 1.0) * 2.0 - 1.0)))
    }

    pub(super) fn draw_auto(&self, lp: &Painter, view: &View, lane: Rect, row: Rect, t: &Track) {
        let editing = self.auto_edit == Some(t.id);
        let y_of = |db: f32| row.bottom() - 4.0 - ((db - AUTO_RANGE.0) / (AUTO_RANGE.1 - AUTO_RANGE.0)).clamp(0.0, 1.0) * (row.height() - 8.0);
        let col = Color32::from_rgb(250, 140, 200);
        let stroke = Stroke::new(if editing { 1.8 } else { 1.2 }, col.gamma_multiply(if editing { 1.0 } else { 0.7 }));
        if editing {
            let y0 = y_of(0.0);
            lp.line_segment([pos2(lane.left(), y0), pos2(lane.right(), y0)], Stroke::new(1.0, Color32::from_white_alpha(40)));
            lp.text(pos2(lane.left() + 4.0, y0 - 2.0), Align2::LEFT_BOTTOM, "0 dB   double-click: new point, drag: move, right-click: delete", FontId::proportional(9.0), Color32::from_white_alpha(110));
        }
        let mut pts: Vec<Pos2> = Vec::with_capacity(t.auto.len() + 2);
        if let Some(first) = t.auto.first() {
            pts.push(pos2(lane.left(), y_of(first.1)));
        }
        for (f, d) in &t.auto {
            pts.push(pos2(view.x(*f), y_of(*d)));
        }
        if let Some(last) = t.auto.last() {
            pts.push(pos2(lane.right(), y_of(last.1)));
        }
        if pts.len() >= 2 {
            lp.add(Shape::line(pts, stroke));
        }
        if editing {
            for (f, d) in &t.auto {
                lp.circle_filled(pos2(view.x(*f), y_of(*d)), 4.0, col);
                lp.circle_stroke(pos2(view.x(*f), y_of(*d)), 4.0, Stroke::new(1.0, Color32::BLACK));
            }
        }
    }

    /// The automation point of the edited track under a position (track index, point index).
    pub(super) fn auto_hit(&self, p: &Project, view: &View, rows: &[Rect], pos: Pos2) -> Option<(usize, usize)> {
        let ti = self.auto_edit.and_then(|id| p.track_index(id))?;
        let row = *rows.get(ti)?;
        let t = &p.tracks[ti];
        let y_of = |db: f32| row.bottom() - 4.0 - ((db - AUTO_RANGE.0) / (AUTO_RANGE.1 - AUTO_RANGE.0)).clamp(0.0, 1.0) * (row.height() - 8.0);
        t.auto.iter().enumerate().map(|(i, (f, d))| (i, (view.x(*f) - pos.x).hypot(y_of(*d) - pos.y))).filter(|(_, dist)| *dist <= 9.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| (ti, i))
    }

    pub(super) fn draw_ruler(&self, painter: &Painter, view: &View, ruler: Rect, rect: Rect, marks: &[Mark]) {
        painter.rect_filled(ruler, 0.0, Color32::from_rgb(26, 31, 42));
        let rp = painter.with_clip_rect(ruler);
        let step = time_step(80.0 / view.pps);
        let t_left = (ruler.left() - view.left) as f64 / view.pps + view.t0;
        let t_right = (ruler.right() - view.left) as f64 / view.pps + view.t0;
        let mut t = (t_left / step).floor() * step;
        while t <= t_right {
            let x = view.left + ((t - view.t0) * view.pps) as f32;
            rp.line_segment([pos2(x, ruler.bottom() - 9.0), pos2(x, ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(110, 122, 142)));
            for k in 1..5 {
                let xm = x + (step * k as f64 / 5.0 * view.pps) as f32;
                rp.line_segment([pos2(xm, ruler.bottom() - 4.0), pos2(xm, ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(64, 74, 92)));
            }
            rp.text(pos2(x + 3.0, ruler.top() + 2.0), Align2::LEFT_TOP, ruler_label(t.max(0.0), step), FontId::proportional(9.5), Color32::from_rgb(150, 162, 182));
            t += step;
        }
        if let Some((a, b)) = self.range {
            let r = Rect::from_min_max(pos2(view.x(a), ruler.top()), pos2(view.x(b), ruler.bottom()));
            rp.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 160, 255, 60));
        }
        // loop markers
        if self.loop_on {
            let (a, b) = self.range.unwrap_or((0, 0));
            if b > a {
                rp.rect_filled(Rect::from_min_max(pos2(view.x(a), ruler.top()), pos2(view.x(b), ruler.top() + 3.0)), 0.0, Color32::from_rgb(110, 200, 140));
            }
        }
        for (i, m) in marks.iter().enumerate() {
            let col = Color32::from_rgb(255, 196, 80);
            let x = view.x(m.at);
            if m.len > 0 {
                let x2 = view.x(m.at + m.len);
                rp.rect_filled(Rect::from_min_max(pos2(x, ruler.bottom() - 5.0), pos2(x2, ruler.bottom())), 0.0, col.gamma_multiply(0.55));
                rp.line_segment([pos2(x, ruler.bottom() - 11.0), pos2(x, ruler.bottom())], Stroke::new(1.5, col));
                rp.line_segment([pos2(x2, ruler.bottom() - 11.0), pos2(x2, ruler.bottom())], Stroke::new(1.0, col.gamma_multiply(0.7)));
            } else {
                rp.add(Shape::convex_polygon(vec![pos2(x, ruler.bottom()), pos2(x - 4.0, ruler.bottom() - 8.0), pos2(x + 4.0, ruler.bottom() - 8.0)], col, Stroke::NONE));
                rp.line_segment([pos2(x, ruler.bottom() - 8.0), pos2(x, ruler.top() + 11.0)], Stroke::new(1.0, col.gamma_multiply(0.6)));
            }
            // the name goes where there is room: up to the next mark
            let next_x = marks.get(i + 1).map(|n| view.x(n.at)).unwrap_or(f32::MAX);
            if next_x - x > 36.0 {
                rp.with_clip_rect(Rect::from_min_max(pos2(x + 3.0, ruler.top()), pos2(next_x.min(ruler.right()) - 2.0, ruler.bottom()))).text(pos2(x + 6.0, ruler.bottom() - 11.0), Align2::LEFT_TOP, &m.name, FontId::proportional(9.0), col);
            }
        }
        let px = view.x(self.pos());
        rp.add(Shape::convex_polygon(vec![pos2(px - 5.0, ruler.top() + 1.0), pos2(px + 5.0, ruler.top() + 1.0), pos2(px, ruler.top() + 9.0)], PLAYHEAD, Stroke::NONE));
        painter.line_segment([pos2(rect.left(), ruler.bottom()), pos2(rect.right(), ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_clip(&self, lp: &Painter, view: &View, lane: Rect, c: &Clip, col: Color32, selected: bool, chans: &[Option<usize>], chm: [bool; 2], dimmed: bool, show_beats: bool) {
        let (x0, x1) = (view.x(c.start), view.x(c.end()));
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        let cr = Rect::from_min_max(pos2(x0, lane.top() + 3.0), pos2(x1.max(x0 + 1.5), lane.bottom() - 3.0));
        let off = c.muted || dimmed;
        let base = if off { Color32::from_gray(90) } else { col };
        let vis = cr.intersect(lp.clip_rect());
        let cp = lp.with_clip_rect(vis);
        cp.rect_filled(cr, 4.0, base.gamma_multiply(if selected { 0.42 } else { 0.26 }));
        // name strip
        let strip = Rect::from_min_max(cr.min, pos2(cr.max.x, cr.min.y + 14.0));
        cp.rect_filled(strip, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, base.gamma_multiply(if selected { 0.9 } else { 0.62 }));
        let mut label = c.name.clone();
        if c.gain_db.abs() > 0.05 {
            label.push_str(&format!("   {:+.1} dB", c.gain_db));
        }
        if c.reverse {
            label.push_str("   rev");
        }
        if c.muted {
            label.push_str("   muted");
        }
        // (the name stays in view when the start of the clip is scrolled out of sight)
        let nx = (strip.left().max(lane.left()) + 5.0).min((strip.right() - 40.0).max(strip.left() + 5.0));
        cp.text(pos2(nx, strip.min.y + 1.5), Align2::LEFT_TOP, label, FontId::proportional(10.0), Color32::from_rgb(10, 12, 16));
        // waveform
        let inner = Rect::from_min_max(pos2(cr.left(), strip.bottom() + 1.0), pos2(cr.right(), cr.bottom() - 1.0));
        let chans: &[Option<usize>] = if c.src.stereo() { chans } else { &[None] };
        let wave_col = base.gamma_multiply(if selected { 1.0 } else { 0.85 });
        let g = db_to_lin(c.gain_db);
        let fpp = view.sr / view.pps;
        let (xa, xb) = (vis.left().floor().max(cr.left().floor()), vis.right().ceil().min(cr.right().ceil()));
        let n_ch = chans.len();
        let lane_h = inner.height() / n_ch as f32;
        let mut shapes: Vec<Shape> = Vec::with_capacity(((xb - xa).max(0.0) as usize) * n_ch);
        let mut x = xa;
        while x < xb {
            let fa = ((x - view.left) as f64 / view.pps + view.t0) * view.sr - c.start as f64;
            let a = fa.max(0.0) as usize;
            let b = ((fa + fpp).ceil().max(0.0) as usize).min(c.len);
            if b > a {
                for (k, ch) in chans.iter().enumerate() {
                    let (lo, hi) = wave_minmax(c, *ch, a, b);
                    let sub = Rect::from_min_size(pos2(inner.left(), inner.top() + k as f32 * lane_h), vec2(inner.width(), lane_h));
                    let (mid, half) = (sub.center().y, sub.height() * 0.5 * 0.94);
                    let y0 = mid - (hi * g).clamp(-1.0, 1.0) * half;
                    let y1 = (mid - (lo * g).clamp(-1.0, 1.0) * half).max(y0 + 1.0);
                    let over = hi * g > 1.0 || -lo * g > 1.0;
                    let silent = ch.is_some_and(|c| chm[c]);
                    shapes.push(Shape::line_segment([pos2(x + 0.5, y0), pos2(x + 0.5, y1)], Stroke::new(1.0, if silent { Color32::from_gray(80) } else if over && !off && self.show_clip { Color32::from_rgb(255, 90, 90) } else { wave_col })));
                }
            }
            x += 1.0;
        }
        cp.extend(shapes);
        // the beat of the music (when this track shows it)
        let mut bpm = 0.0;
        if show_beats {
            let sr = view.sr as u32;
            if let Some(g) = c.src.grid(sr) {
                bpm = g.bpm;
                let mut last_x = f32::MIN;
                for &b in &g.beats {
                    if b < c.off || b >= c.off + c.len {
                        continue;
                    }
                    let rel = b - c.off;
                    let x = view.x(c.start + if c.reverse { (c.len - 1 - rel) as i64 } else { rel as i64 });
                    if x < vis.left() - 1.0 || x > vis.right() + 1.0 || (x - last_x).abs() < 3.0 {
                        continue;
                    }
                    last_x = x;
                    let col = Color32::from_rgba_unmultiplied(255, 220, 120, 200);
                    cp.line_segment([pos2(x, inner.top()), pos2(x, inner.bottom())], Stroke::new(1.0, col.gamma_multiply(0.7)));
                    cp.add(Shape::convex_polygon(vec![pos2(x - 3.5, strip.bottom() + 1.0), pos2(x + 3.5, strip.bottom() + 1.0), pos2(x, strip.bottom() + 6.0)], col, Stroke::NONE));
                }
            } else if c.src.grid_pending() {
                lp.ctx().request_repaint_after(std::time::Duration::from_millis(400));
            }
        }
        if bpm > 0.0 {
            cp.text(pos2(strip.right() - 5.0, strip.min.y + 1.5), Align2::RIGHT_TOP, format!("{bpm:.0} bpm"), FontId::proportional(10.0), Color32::from_rgb(10, 12, 16));
        }
        for k in 0..n_ch {
            let y = inner.top() + (k as f32 + 0.5) * lane_h;
            cp.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(14)));
            if n_ch == 2 && cr.width() > 40.0 {
                let ytop = inner.top() + k as f32 * lane_h;
                if k == 1 {
                    cp.line_segment([pos2(inner.left(), ytop), pos2(inner.right(), ytop)], Stroke::new(1.0, Color32::from_white_alpha(40)));
                }
                cp.text(pos2(inner.left().max(lane.left()) + 4.0, ytop + 2.0), Align2::LEFT_TOP, if k == 0 { "L" } else { "R" }, FontId::proportional(9.0), Color32::from_white_alpha(140));
            }
        }
        // fades
        let (fi, fo) = (c.fade_in as f64 / fpp, c.fade_out as f64 / fpp);
        let shade = Color32::from_black_alpha(110);
        let fl = Stroke::new(1.2, Color32::from_white_alpha(170));
        if fi >= 1.0 {
            let w = fi as f32;
            cp.add(Shape::convex_polygon(vec![pos2(x0, strip.bottom()), pos2(x0 + w, strip.bottom()), pos2(x0, cr.bottom())], shade, Stroke::NONE));
            cp.line_segment([pos2(x0, cr.bottom()), pos2(x0 + w, strip.bottom())], fl);
        }
        if fo >= 1.0 {
            let w = fo as f32;
            cp.add(Shape::convex_polygon(vec![pos2(x1, strip.bottom()), pos2(x1 - w, strip.bottom()), pos2(x1, cr.bottom())], shade, Stroke::NONE));
            cp.line_segment([pos2(x1, cr.bottom()), pos2(x1 - w, strip.bottom())], fl);
        }
        if selected && cr.width() >= 60.0 {
            cp.circle_filled(pos2(x0 + fi as f32 + 7.0, strip.center().y), 3.0, Color32::from_white_alpha(200));
            cp.circle_filled(pos2(x1 - fo as f32 - 7.0, strip.center().y), 3.0, Color32::from_white_alpha(200));
        }
        cp.rect_stroke(cr, 4.0, Stroke::new(if selected { 1.6 } else { 1.0 }, if selected { Color32::from_rgb(235, 240, 250) } else { base.gamma_multiply(0.9) }), egui::StrokeKind::Inside);
    }

    /// Live waveform of what is being recorded on an armed track.
    pub(super) fn draw_rec_preview(&self, lp: &Painter, view: &View, lane: Rect, t: &Track) {
        let Some(rec) = &self.rec else { return };
        let Some(plan) = rec.plan.iter().find(|r| r.track_id == t.id) else { return };
        let Some(cap) = self.capt.get(&plan.dev) else { return };
        let Some((dev_sr, _)) = cap.format() else { return };
        let frames = cap.frames();
        if frames == 0 {
            return;
        }
        let proj_frames = (frames as f64 * view.sr / dev_sr as f64) as i64;
        let (x0, x1) = (view.x(rec.start), view.x(rec.start + proj_frames));
        let cr = Rect::from_min_max(pos2(x0, lane.top() + 3.0), pos2(x1.max(x0 + 2.0), lane.bottom() - 3.0));
        lp.rect_filled(cr, 3.0, Color32::from_rgba_unmultiplied(235, 70, 70, 40));
        lp.rect_stroke(cr, 3.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(235, 90, 90, 160)), egui::StrokeKind::Inside);
        let vis = cr.intersect(lp.clip_rect());
        if vis.width() < 1.0 {
            return;
        }
        // columns of the visible part only
        let lane_idx = plan.lanes[0];
        let cols = vis.width().ceil() as usize;
        let frac0 = ((vis.left() - cr.left()) / cr.width().max(1.0)) as f64;
        let frac1 = ((vis.right() - cr.left()) / cr.width().max(1.0)) as f64;
        let (a, b) = ((frac0 * frames as f64) as usize, (frac1 * frames as f64) as usize);
        let mid = cr.center().y;
        let half = (cr.height() - 4.0) * 0.5;
        let mut shapes = Vec::new();
        for (i, (lo, hi)) in cap.preview(lane_idx, a, b, cols).into_iter().enumerate() {
            let x = vis.left() + i as f32 + 0.5;
            shapes.push(Shape::line_segment([pos2(x, mid - hi.clamp(-1.0, 1.0) * half), pos2(x, (mid - lo.clamp(-1.0, 1.0) * half).max(mid - hi.clamp(-1.0, 1.0) * half + 1.0))], Stroke::new(1.0, Color32::from_rgb(255, 150, 150))));
        }
        lp.with_clip_rect(vis).extend(shapes);
    }
}
