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
    /// counts every change of the history, so a picture made from it can tell it is out of date
    ver: u64,
    /// the surface of the last frame: the data changes at `rate` rows per second, the window is redrawn far more often
    surface: Option<Surface>,
}

/// Everything the geometry of the surface depends on.
#[derive(Clone, PartialEq)]
struct SurfaceKey {
    ver: u64,
    rows: usize,
    cols: usize,
    wire: bool,
    view: [f32; 14],
    lut: [Color32; 256],
}

/// The projected vertices and the meshes (far -> near) of the surface.
struct Surface {
    key: SurfaceKey,
    verts: Vec<epaint::Vertex>,
    /// each mesh and the row of its far edge (for the wire frame)
    groups: Vec<(std::sync::Arc<epaint::Mesh>, usize)>,
}

/// Triangles of the cells of `n_rows` rows of `cols` vertices, strips and cells in the order they are painted
/// (far to near, so that the near ones cover the far ones).
fn strip_indices(n_rows: usize, cols: usize, near_new: bool, x_asc: bool) -> Vec<u32> {
    let mut idx = Vec::with_capacity((n_rows - 1) * (cols - 1) * 6);
    for k in 0..n_rows - 1 {
        let s = if near_new { k } else { n_rows - 2 - k };
        for j in 0..cols - 1 {
            let c = if x_asc { j } else { cols - 2 - j };
            let a = (s * cols + c) as u32;
            let b = a + 1;
            let cc = ((s + 1) * cols + c) as u32;
            let d = cc + 1;
            idx.extend_from_slice(&[a, b, cc, b, d, cc]);
        }
    }
    idx
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
            ver: 0,
            surface: None,
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
        self.ver = self.ver.wrapping_add(1);
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
        self.ver = self.ver.wrapping_add(1);
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

        let rows = self.d3.rows.min(D3_MAX).min(self.d3.count);
        if rows < 3 {
            painter.text(rect.center(), Align2::CENTER_CENTER, "collecting data...", FontId::proportional(14.0), TXT);
            return;
        }
        let cols = self.d3.cols.max(8);
        let (flo_u, fhi_u) = (self.f_lo, self.f_hi);
        let (hs, wire, rate) = (self.d3.hscale, self.d3.wire, self.d3.rate);

        // camera
        let (sy, cy) = self.d3.yaw.to_radians().sin_cos();
        let (sp, cp) = self.d3.pitch.to_radians().sin_cos();
        const DIST: f32 = 3.4;
        let scale = rect.width().min(rect.height()) * 0.42 * self.d3.zoom;
        let center = rect.center() + self.d3.pan * (rect.width().min(rect.height()) * 0.5) + vec2(0.0, 0.0);
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

        // The surface only changes with the history (`rate` rows per second), the view or the colours, but the window is redrawn
        // far more often: rebuild the vertices and the triangles (up to a million and a half indices) only when something changed.
        let key = SurfaceKey {
            ver: self.d3.ver,
            rows,
            cols,
            wire,
            view: [
                self.d3.yaw, self.d3.pitch, self.d3.zoom, self.d3.pan.x, self.d3.pan.y, hs, flo_u, fhi_u, self.col_lo, self.col_hi,
                rect.min.x, rect.min.y, rect.width(), rect.height(),
            ],
            lut: self.dyn_lut,
        };
        if self.d3.surface.as_ref().is_none_or(|s| s.key != key) {
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
            let d3 = &self.d3;
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

            // groups far -> near
            let group = if wire { 4 } else { rows };
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            let mut t0 = 0;
            while t0 < rows - 1 {
                let t1 = (t0 + group).min(rows - 1);
                ranges.push((t0, t1));
                t0 = t1;
            }
            if !near_new {
                ranges.reverse();
            }
            let groups = ranges
                .into_iter()
                .map(|(t0, t1)| {
                    let mut mesh = epaint::Mesh::default();
                    mesh.vertices.extend_from_slice(&verts[t0 * cols..(t1 + 1) * cols]);
                    mesh.indices = strip_indices(t1 - t0 + 1, cols, near_new, x_asc);
                    (std::sync::Arc::new(mesh), if near_new { t0 } else { t1 })
                })
                .collect();
            self.d3.surface = Some(Surface { key, verts, groups });
        }
        let Some(surf) = self.d3.surface.as_ref() else { return };
        for (mesh, far) in &surf.groups {
            painter.add(Shape::mesh(mesh.clone()));
            if wire {
                let pts: Vec<Pos2> = surf.verts[far * cols..(far + 1) * cols].iter().map(|v| v.pos).collect();
                painter.add(Shape::line(pts, Stroke::new(0.8, Color32::from_white_alpha(70))));
            }
        }
        // front (newest) trace highlighted
        let nr = rows - 1;
        let pts: Vec<Pos2> = surf.verts[nr * cols..(nr + 1) * cols].iter().map(|v| v.pos).collect();
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
        let secs = rows as f32 / rate;
        painter.text(
            rect.left_bottom() + vec2(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            format!("{:.0}..{:.0} dB   depth {:.1}s   drag: orbit/tilt  right-drag: pan  wheel: zoom  shift+wheel: freq", self.col_lo, self.col_hi, secs),
            FontId::proportional(10.0),
            TXT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The triangles as the panel built them before the surface was cached (the reference).
    fn old_indices(n_rows: usize, cols: usize, near_new: bool, x_asc: bool) -> Vec<u32> {
        let mut indices = Vec::new();
        let strips: Vec<usize> = if near_new { (0..n_rows - 1).collect() } else { (0..n_rows - 1).rev().collect() };
        for s in strips {
            let cells: Vec<usize> = if x_asc { (0..cols - 1).collect() } else { (0..cols - 1).rev().collect() };
            for c in cells {
                let a = (s * cols + c) as u32;
                let b = a + 1;
                let cc = ((s + 1) * cols + c) as u32;
                let d = cc + 1;
                indices.extend_from_slice(&[a, b, cc, b, d, cc]);
            }
        }
        indices
    }

    #[test]
    fn strip_indices_equal_the_old_per_cell_loop() {
        for (n_rows, cols) in [(2, 2), (3, 8), (4, 64), (17, 100)] {
            for near_new in [false, true] {
                for x_asc in [false, true] {
                    assert_eq!(strip_indices(n_rows, cols, near_new, x_asc), old_indices(n_rows, cols, near_new, x_asc));
                }
            }
        }
    }

    #[test]
    fn the_surface_key_changes_with_the_history_and_the_view() {
        let mut v = View3d::new();
        let key = |v: &View3d| SurfaceKey { ver: v.ver, rows: 8, cols: 8, wire: false, view: [v.yaw, v.pitch, v.zoom, 0.0, 0.0, 0.8, 0.0, 1.0, -90.0, 0.0, 0.0, 0.0, 100.0, 100.0], lut: [Color32::BLACK; 256] };
        let k0 = key(&v);
        assert!(k0 == key(&v));
        // a row at the 24 rows/s of the default: a column per call at 24 columns/s
        v.push(&vec![1u8; ROWS], 24.0);
        assert!(k0 != key(&v), "a new row makes the surface out of date");
        let k1 = key(&v);
        v.push(&vec![1u8; ROWS], 1000.0);
        assert!(k1 == key(&v), "a column that is not taken into the history leaves it alone");
        v.clear();
        assert!(k1 != key(&v));
        v.yaw += 1.0;
        assert!(key(&v) != k1);
    }

    #[test]
    #[ignore]
    fn bench_surface_indices() {
        let t = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..20 {
            n += old_indices(256, 1024, true, true).len();
        }
        eprintln!("old, rebuilt every frame: {:?} per frame ({n})", t.elapsed() / 20);
        let t = std::time::Instant::now();
        for _ in 0..20 {
            n += strip_indices(256, 1024, true, true).len();
        }
        eprintln!("new strip_indices (only when the data changes): {:?} per build ({n})", t.elapsed() / 20);
    }
}
