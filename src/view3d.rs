//! 3D time/frequency/amplitude surface with orbit, tilt, pan and zoom.

use crate::app::*;
use crate::shared::*;
use crate::views::{BG, GRID_MAJOR, TXT};
use eframe::egui::{self, epaint, vec2, Align2, Color32, FontId, Pos2, Sense, Shape, Stroke, Ui};

pub const D3_MAX: usize = 256;

pub struct View3d {
    hist: Vec<u8>,
    head: usize,
    count: usize,
    acc: f32,
    pub rows: usize,
    pub cols: usize,
    pub rate: f32,
    pub hscale: f32,
    pub wire: bool,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    pan: egui::Vec2,
}

impl View3d {
    pub fn new() -> Self {
        Self {
            hist: vec![0; D3_MAX * ROWS],
            head: 0,
            count: 0,
            acc: 0.0,
            rows: 128,
            cols: 384,
            rate: 24.0,
            hscale: 0.8,
            wire: false,
            yaw: -28.0,
            pitch: 38.0,
            zoom: 1.0,
            pan: vec2(0.0, 0.0),
        }
    }

    fn reset_view(&mut self) {
        self.yaw = -28.0;
        self.pitch = 38.0;
        self.zoom = 1.0;
        self.pan = vec2(0.0, 0.0);
        self.hscale = 0.8;
    }

    pub fn clear(&mut self) {
        self.hist.fill(0);
        self.head = 0;
        self.count = 0;
    }

    pub fn push(&mut self, col: &[u8], col_rate: f32) {
        self.acc += self.rate / col_rate.max(1.0);
        if self.acc < 1.0 {
            return;
        }
        self.acc -= 1.0;
        if self.acc > 1.0 {
            self.acc = 0.0;
        }
        self.hist[self.head * ROWS..(self.head + 1) * ROWS].copy_from_slice(col);
        self.head = (self.head + 1) % D3_MAX;
        self.count = (self.count + 1).min(D3_MAX);
    }
}

impl App {
    pub fn panel_3d(&mut self, ui: &mut Ui) {
        let act = self.header(ui, 3, "3D spectrum", ("Zoom", "Height"), true);
        if act.reset {
            self.d3.reset_view();
            self.f_lo = 0.0;
            self.f_hi = 1.0;
        }
        if act.zx != 0.0 {
            self.d3.zoom = (self.d3.zoom / act.zx).clamp(0.3, 6.0);
        }
        if act.zy != 0.0 {
            self.d3.hscale = (self.d3.hscale / act.zy).clamp(0.1, 3.0);
        }
        self.d3.pan += vec2(act.px, -act.py) * 0.3;
        self.d3.pitch = (self.d3.pitch + act.tilt).clamp(2.0, 89.0);
        self.d3.yaw = (self.d3.yaw + act.rot).clamp(-85.0, 85.0);

        let size = ui.available_size();
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, BG);
        let painter = painter.with_clip_rect(rect);
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.d3.yaw = (self.d3.yaw + d.x * 0.4).clamp(-85.0, 85.0);
            self.d3.pitch = (self.d3.pitch + d.y * 0.3).clamp(2.0, 89.0);
        } else if resp.dragged() {
            let d = resp.drag_delta();
            self.d3.pan += vec2(d.x, d.y) / rect.width().min(rect.height()) * 2.0;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if ui.input(|i| i.modifiers.shift) {
                    let c = 0.5 * (self.f_lo + self.f_hi);
                    zoom_view(&mut self.f_lo, &mut self.f_hi, (-scroll * 0.004).exp(), c, 0.002);
                } else {
                    self.d3.zoom = (self.d3.zoom * (scroll * 0.003).exp()).clamp(0.3, 6.0);
                }
            }
        }

        let d3 = &self.d3;
        let rows = d3.rows.min(D3_MAX).min(d3.count);
        if rows < 3 {
            painter.text(rect.center(), Align2::CENTER_CENTER, "collecting data...", FontId::proportional(14.0), TXT);
            return;
        }
        let cols = d3.cols.max(8);
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
        let hs = d3.hscale;

        // camera
        let (sy, cy) = d3.yaw.to_radians().sin_cos();
        let (sp, cp) = d3.pitch.to_radians().sin_cos();
        const DIST: f32 = 3.4;
        let scale = rect.width().min(rect.height()) * 0.42 * d3.zoom;
        let center = rect.center() + d3.pan * (rect.width().min(rect.height()) * 0.5) + vec2(0.0, 0.0);
        let proj = |x: f32, y: f32, z: f32| -> (Pos2, f32) {
            let y = y - hs * 0.35;
            let x1 = x * cy + z * sy;
            let z1 = -x * sy + z * cy;
            let y2 = y * cp - z1 * sp;
            let z2 = y * sp + z1 * cp;
            let k = DIST / (DIST - z2);
            (Pos2::new(center.x + x1 * k * scale, center.y - y2 * k * scale), z2)
        };
        let near_new = proj(0.0, 0.0, 1.0).1 > proj(0.0, 0.0, -1.0).1;
        let x_asc = proj(1.0, 0.0, 0.0).1 > proj(-1.0, 0.0, 0.0).1;

        // floor box
        let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
        let fl: Vec<Pos2> = corners.iter().map(|&(x, z)| proj(x, 0.0, z).0).collect();
        let st = Stroke::new(1.0, GRID_MAJOR);
        painter.add(Shape::closed_line(fl.clone(), st));
        for &(x, z) in &[(-1.0, -1.0), (1.0, -1.0)] {
            painter.line_segment([proj(x, 0.0, z).0, proj(x, hs, z).0], st);
        }
        for i in 1..4 {
            let t = i as f32 / 4.0;
            painter.line_segment([proj(-1.0, hs * t, -1.0).0, proj(1.0, hs * t, -1.0).0], Stroke::new(1.0, Color32::from_white_alpha(14)));
        }

        // height / colour tables
        let span_db = (self.col_hi - self.col_lo).max(1.0);
        let mut h_tab = [0.0f32; 256];
        for (v, h) in h_tab.iter_mut().enumerate() {
            let db = v as f32 / 255.0 * HIST_DB_RANGE - HIST_DB_RANGE;
            *h = ((db - self.col_lo) / span_db).clamp(0.0, 1.0);
        }

        // frequency -> source row range per column
        let half = 0.5 * (fhi_u - flo_u) / (cols - 1) as f32;
        let src: Vec<(usize, usize)> = (0..cols)
            .map(|c| {
                let u = flo_u + c as f32 / (cols - 1) as f32 * (fhi_u - flo_u);
                let r0 = ((u - half) * ROWS as f32).floor().max(0.0) as usize;
                let r1 = (((u + half) * ROWS as f32).ceil() as usize).min(ROWS - 1);
                (r0.min(ROWS - 1), r1.max(r0.min(ROWS - 1)))
            })
            .collect();

        // vertices
        let mut verts: Vec<epaint::Vertex> = Vec::with_capacity(rows * cols);
        for t in 0..rows {
            let hi = (d3.head + D3_MAX - rows + t) % D3_MAX;
            let rowd = &d3.hist[hi * ROWS..(hi + 1) * ROWS];
            let z = t as f32 / (rows - 1) as f32 * 2.0 - 1.0;
            let age = t as f32 / (rows - 1) as f32;
            let shade = 0.28 + 0.72 * age.powf(0.7);
            for c in 0..cols {
                let (r0, r1) = src[c];
                let mut m = 0u8;
                for &v in &rowd[r0..=r1] {
                    if v > m {
                        m = v;
                    }
                }
                let x = c as f32 / (cols - 1) as f32 * 2.0 - 1.0;
                let (p, _) = proj(x, h_tab[m as usize] * hs, z);
                let base = self.dyn_lut[m as usize];
                let col = Color32::from_rgb((base.r() as f32 * shade) as u8, (base.g() as f32 * shade) as u8, (base.b() as f32 * shade) as u8);
                verts.push(epaint::Vertex { pos: p, uv: epaint::WHITE_UV, color: col });
            }
        }

        // draw groups far -> near
        let group = if d3.wire { 4 } else { rows };
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut t0 = 0;
        while t0 < rows - 1 {
            let t1 = (t0 + group).min(rows - 1);
            groups.push((t0, t1));
            t0 = t1;
        }
        if !near_new {
            groups.reverse();
        }
        for (t0, t1) in groups {
            let n_rows = t1 - t0 + 1;
            let mut mesh = epaint::Mesh::default();
            mesh.vertices.extend_from_slice(&verts[t0 * cols..(t1 + 1) * cols]);
            mesh.indices.reserve((n_rows - 1) * (cols - 1) * 6);
            let strips: Vec<usize> = if near_new { (0..n_rows - 1).collect() } else { (0..n_rows - 1).rev().collect() };
            for s in strips {
                let cells: Vec<usize> = if x_asc { (0..cols - 1).collect() } else { (0..cols - 1).rev().collect() };
                for c in cells {
                    let a = (s * cols + c) as u32;
                    let b = a + 1;
                    let cc = ((s + 1) * cols + c) as u32;
                    let d = cc + 1;
                    mesh.indices.extend_from_slice(&[a, b, cc, b, d, cc]);
                }
            }
            painter.add(Shape::mesh(mesh));
            if d3.wire {
                let far = if near_new { t0 } else { t1 };
                let pts: Vec<Pos2> = verts[far * cols..(far + 1) * cols].iter().map(|v| v.pos).collect();
                painter.add(Shape::line(pts, Stroke::new(0.8, Color32::from_white_alpha(70))));
            }
        }
        // front (newest) trace highlighted
        let nr = rows - 1;
        let pts: Vec<Pos2> = verts[nr * cols..(nr + 1) * cols].iter().map(|v| v.pos).collect();
        painter.add(Shape::line(pts, Stroke::new(1.3, Color32::WHITE)));

        // frequency labels on the front edge
        let z_front = if near_new { 1.0 } else { -1.0 };
        let (flo, fhi) = (self.u_to_f(flo_u), self.u_to_f(fhi_u));
        for (f, label) in self.freq_ticks(flo, fhi) {
            if !label {
                continue;
            }
            let u = self.f_to_u(f);
            let x = (u - flo_u) / (fhi_u - flo_u) * 2.0 - 1.0;
            let (p, _) = proj(x, 0.0, z_front);
            painter.line_segment([p, p + vec2(0.0, 4.0)], st);
            painter.text(p + vec2(0.0, 5.0), Align2::CENTER_TOP, fmt_hz_short(f), FontId::proportional(10.0), TXT);
        }
        // cursors along the floor
        for (name, c, f) in [("A", CUR_A, self.cur_a), ("B", CUR_B, self.cur_b)] {
            if let Some(f) = f {
                let u = self.f_to_u(f);
                if u < flo_u || u > fhi_u {
                    continue;
                }
                let x = (u - flo_u) / (fhi_u - flo_u) * 2.0 - 1.0;
                let (a, _) = proj(x, 0.0, -1.0);
                let (b, _) = proj(x, 0.0, 1.0);
                painter.line_segment([a, b], Stroke::new(1.5, c));
                painter.text(if near_new { b } else { a } + vec2(0.0, -4.0), Align2::CENTER_BOTTOM, format!("{name} {}", fmt_hz(f)), FontId::proportional(10.0), c);
            }
        }
        let secs = rows as f32 / d3.rate;
        painter.text(
            rect.left_bottom() + vec2(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            format!("{:.0}..{:.0} dB   depth {:.1}s   drag: orbit/tilt  right-drag: pan  wheel: zoom  shift+wheel: freq", self.col_lo, self.col_hi, secs),
            FontId::proportional(10.0),
            TXT,
        );
    }
}
