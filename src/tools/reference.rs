//! Reference curves, pass/fail limit masks and an A/B difference trace.

use super::dialogs::{self, Pending};
use super::plot::{canvas, heading, inset, Plot};
use super::*;
use crate::app::CH_COLORS;
use eframe::egui::{self, epaint, Align2, Color32, FontId, Pos2, RichText, Stroke};
use parking_lot::Mutex;
use std::time::Instant;

const PALETTE: [Color32; 6] = [
    Color32::from_rgb(255, 200, 80),
    Color32::from_rgb(120, 230, 150),
    Color32::from_rgb(230, 130, 230),
    Color32::from_rgb(255, 130, 110),
    Color32::from_rgb(130, 170, 255),
    Color32::from_rgb(200, 200, 200),
];

#[derive(Clone)]
struct RefCurve {
    name: String,
    /// (Hz, dBFS), ascending
    pts: Vec<(f32, f32)>,
    /// absolute limits per point (3-column CSV); otherwise +/- `tol` around the curve
    limits: Option<Vec<(f32, f32)>>,
    color: Color32,
    show: bool,
    tol: f32,
    offset: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Src {
    Live,
    Ref(usize),
    Snap(usize),
}

pub struct Reference {
    refs: Vec<RefCurve>,
    mask: Option<usize>,
    ch: usize,
    a: Src,
    b: Src,
    open: Option<Pending>,
    save: Option<(Pending, usize)>,
    import: Arc<Mutex<Option<Result<RefCurve, String>>>>,
    err: String,
    fails: Vec<(f32, f32)>,
    worst: (f32, f32),
    fail_pct: f32,
    checked: bool,
    last: Instant,
    y_range: f32,
}

fn interp(pts: &[(f32, f32)], f: f32) -> Option<f32> {
    if pts.len() < 2 || f < pts[0].0 || f > pts[pts.len() - 1].0 {
        return None;
    }
    let i = pts.partition_point(|p| p.0 < f).clamp(1, pts.len() - 1);
    let (a, b) = (pts[i - 1], pts[i]);
    let t = ((f / a.0).ln() / (b.0 / a.0).ln().max(1e-9)).clamp(0.0, 1.0);
    Some(a.1 + (b.1 - a.1) * t)
}

pub fn parse_csv(text: &str) -> Result<(Vec<(f32, f32)>, Option<Vec<(f32, f32)>>), String> {
    let (mut pts, mut lim) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let l = line.trim();
        if !l.chars().next().map(|c| c.is_ascii_digit() || c == '-' || c == '+' || c == '.').unwrap_or(false) {
            continue;
        }
        let semi = l.contains(';');
        let fields: Vec<f32> = if semi { l.split(';').collect::<Vec<_>>() } else if l.contains('\t') { l.split('\t').collect() } else { l.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).collect() }
            .into_iter()
            .filter_map(|s| s.trim().replace(',', ".").parse().ok())
            .collect();
        if fields.len() >= 2 && fields[0] > 0.0 {
            pts.push((fields[0], fields[1]));
            if fields.len() >= 3 {
                lim.push((fields[1], fields[2]));
            }
        }
    }
    if pts.len() < 2 {
        return Err("need at least two lines of frequency, level".into());
    }
    let mut idx: Vec<usize> = (0..pts.len()).collect();
    idx.sort_by(|&a, &b| pts[a].0.total_cmp(&pts[b].0));
    let sorted: Vec<_> = idx.iter().map(|&i| pts[i]).collect();
    let limits = (lim.len() == pts.len()).then(|| idx.iter().map(|&i| lim[i]).collect());
    if let Some(l) = &limits {
        // 3 columns: f, lower, upper
        let l: &Vec<(f32, f32)> = l;
        return Ok((l.iter().zip(&sorted).map(|(lo_hi, p)| (p.0, 0.5 * (lo_hi.0 + lo_hi.1))).collect(), Some(l.clone())));
    }
    Ok((sorted, None))
}

impl Reference {
    pub fn new() -> Self {
        Self { refs: Vec::new(), mask: None, ch: 0, a: Src::Live, b: Src::Live, open: None, save: None, import: Arc::new(Mutex::new(None)), err: String::new(), fails: Vec::new(), worst: (0.0, 0.0), fail_pct: 0.0, checked: false, last: Instant::now(), y_range: 20.0 }
    }

    fn color(&self) -> Color32 {
        PALETTE[self.refs.len() % PALETTE.len()]
    }

    fn from_spectrum(&mut self, name: String, f: impl Fn(f32) -> Option<f32>) {
        let pts: Vec<(f32, f32)> = (0..400).filter_map(|i| {
            let fr = 20.0 * 1000f32.powf(i as f32 / 399.0);
            f(fr).map(|v| (fr, v))
        }).collect();
        if pts.len() > 2 {
            self.refs.push(RefCurve { name, pts, limits: None, color: self.color(), show: true, tol: 3.0, offset: 0.0 });
        }
    }

    fn eval(&self, s: Src, cx: &Ctx, f: f32) -> Option<f32> {
        match s {
            Src::Live => spec_at(cx.an, &cx.an.avg[self.ch], f),
            Src::Ref(i) => self.refs.get(i).and_then(|r| interp(&r.pts, f).map(|v| v + r.offset)),
            Src::Snap(i) => cx.snaps.get(i).and_then(|s| {
                let b = f / (s.sr / s.n as f32);
                let k = b as usize;
                (k + 1 < s.data.len()).then(|| s.data[k] + (s.data[k + 1] - s.data[k]) * (b - k as f32))
            }),
        }
    }

    fn src_name(&self, s: Src, cx: &Ctx) -> String {
        match s {
            Src::Live => "live (avg)".into(),
            Src::Ref(i) => self.refs.get(i).map(|r| r.name.clone()).unwrap_or_else(|| "-".into()),
            Src::Snap(i) => cx.snaps.get(i).map(|s| format!("snap {}", s.name)).unwrap_or_else(|| "-".into()),
        }
    }

    /// Limits (lo, hi) of the mask at `f`.
    fn limits_at(&self, f: f32) -> Option<(f32, f32)> {
        let r = self.refs.get(self.mask?)?;
        match &r.limits {
            Some(l) => {
                let lo: Vec<_> = r.pts.iter().zip(l).map(|(p, l)| (p.0, l.0)).collect();
                let hi: Vec<_> = r.pts.iter().zip(l).map(|(p, l)| (p.0, l.1)).collect();
                Some((interp(&lo, f)? + r.offset, interp(&hi, f)? + r.offset))
            }
            None => {
                let c = interp(&r.pts, f)? + r.offset;
                Some((c - r.tol, c + r.tol))
            }
        }
    }

    fn check(&mut self, cx: &Ctx) {
        self.fails.clear();
        self.fail_pct = 0.0;
        self.checked = false;
        let Some(r) = self.mask.and_then(|i| self.refs.get(i)) else { return };
        if cx.an.n == 0 {
            return;
        }
        let (f0, f1) = (r.pts[0].0.max(10.0), r.pts[r.pts.len() - 1].0.min(cx.sr * 0.5));
        if f1 <= f0 * 1.05 {
            return;
        }
        let n = 500;
        let (mut bad, mut worst) = (0, (0f32, 0f32));
        let mut start: Option<f32> = None;
        let mut prev = f0;
        for i in 0..n {
            let f = f0 * (f1 / f0).powf(i as f32 / (n - 1) as f32);
            let (Some(v), Some((lo, hi))) = (spec_at(cx.an, &cx.an.avg[self.ch], f), self.limits_at(f)) else { continue };
            let dev = if v > hi { v - hi } else if v < lo { v - lo } else { 0.0 };
            if dev != 0.0 {
                bad += 1;
                if dev.abs() > worst.0.abs() {
                    worst = (dev, f);
                }
                start.get_or_insert(f);
            } else if let Some(s) = start.take() {
                self.fails.push((s, prev));
            }
            prev = f;
        }
        if let Some(s) = start {
            self.fails.push((s, prev));
        }
        self.checked = true;
        self.worst = worst;
        self.fail_pct = bad as f32 / n as f32 * 100.0;
    }
}

impl Tool for Reference {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        if let Some(d) = &self.open {
            if let Some(res) = d.take() {
                self.open = None;
                if let Some(path) = res {
                    let (slot, color) = (self.import.clone(), self.color());
                    std::thread::spawn(move || {
                        let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| parse_csv(&t)).map(|(pts, limits)| RefCurve {
                            name: path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                            pts,
                            limits,
                            color,
                            show: true,
                            tol: 3.0,
                            offset: 0.0,
                        });
                        *slot.lock() = Some(r);
                    });
                }
            }
        }
        if let Some(r) = self.import.try_lock().and_then(|mut g| g.take()) {
            match r {
                Ok(c) => {
                    if c.limits.is_some() && self.mask.is_none() {
                        self.mask = Some(self.refs.len());
                    }
                    self.refs.push(c);
                    self.err.clear();
                }
                Err(e) => self.err = format!("import failed: {e}"),
            }
        }
        if let Some((d, i)) = &self.save {
            let i = *i;
            if let Some(res) = d.take() {
                self.save = None;
                if let (Some(path), Some(r)) = (res, self.refs.get(i)) {
                    let mut s = String::from("freq_hz,level_db\n");
                    for p in &r.pts {
                        s += &format!("{:.3},{:.3}\n", p.0, p.1 + r.offset);
                    }
                    cx.req.note = Some(match std::fs::write(&path, s) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
        if self.mask.is_some() && self.last.elapsed().as_millis() > 100 {
            self.last = Instant::now();
            self.check(cx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.ch, 0, RichText::new("L").color(CH_COLORS[0]));
            ui.selectable_value(&mut self.ch, 1, RichText::new("R").color(CH_COLORS[1]));
            if ui.button("Capture live").on_hover_text("Store the averaged spectrum as a reference curve").clicked() {
                let n = self.refs.len() + 1;
                let ch = self.ch;
                self.from_spectrum(format!("capture {n}"), |f| spec_at(cx.an, &cx.an.avg[ch], f));
            }
            egui::ComboBox::from_id_salt("ref_snap").width(110.0).selected_text("From snapshot").show_ui(ui, |ui| {
                for (i, s) in cx.snaps.iter().enumerate() {
                    if ui.selectable_label(false, &s.name).clicked() {
                        let src = Src::Snap(i);
                        let name = s.name.clone();
                        let me = &*self;
                        let pts: Vec<(f32, f32)> = (0..400).filter_map(|k| {
                            let f = 20.0 * 1000f32.powf(k as f32 / 399.0);
                            me.eval(src, cx, f).map(|v| (f, v))
                        }).collect();
                        if pts.len() > 2 {
                            let color = self.color();
                            self.refs.push(RefCurve { name, pts, limits: None, color, show: true, tol: 3.0, offset: 0.0 });
                        }
                    }
                }
                if cx.snaps.is_empty() {
                    ui.label(RichText::new("no snapshots yet").weak());
                }
            });
            if ui.add_enabled(self.open.is_none(), egui::Button::new("Import CSV...")).on_hover_text("2 columns: frequency, level (a reference curve)\n3 columns: frequency, lower limit, upper limit (a mask)").clicked() {
                self.open = Some(dialogs::open("Import reference", "CSV / text", &["csv", "txt"]));
            }
        });
        if !self.err.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 120, 100), &self.err);
        }
        // list
        let mut remove: Option<usize> = None;
        if self.refs.is_empty() {
            ui.label(RichText::new("No references yet. Capture the live spectrum, take one from a snapshot or import a CSV.").small().weak());
        } else {
            egui::Grid::new("ref_grid").num_columns(7).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, r) in self.refs.iter_mut().enumerate() {
                    ui.checkbox(&mut r.show, "");
                    ui.label(RichText::new(&r.name).color(r.color));
                    let is_mask = self.mask == Some(i);
                    if ui.selectable_label(is_mask, "mask").on_hover_text("Use as pass/fail limit mask").clicked() {
                        self.mask = if is_mask { None } else { Some(i) };
                    }
                    ui.add_enabled(r.limits.is_none(), egui::DragValue::new(&mut r.tol).range(0.1..=60.0).speed(0.1).prefix("+/-").suffix(" dB"));
                    ui.add(egui::DragValue::new(&mut r.offset).range(-200.0..=200.0).speed(0.1).prefix("shift ").suffix(" dB"));
                    if ui.small_button("save").clicked() && self.save.is_none() {
                        self.save = Some((dialogs::save("Save reference", "CSV", &["csv"], format!("{}.csv", r.name)), i));
                    }
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        }
        if let Some(i) = remove {
            self.refs.remove(i);
            self.mask = match self.mask {
                Some(m) if m == i => None,
                Some(m) if m > i => Some(m - 1),
                m => m,
            };
            let fix = |s: Src| match s {
                Src::Ref(k) if k == i => Src::Live,
                Src::Ref(k) if k > i => Src::Ref(k - 1),
                s => s,
            };
            self.a = fix(self.a);
            self.b = fix(self.b);
        }
        // verdict
        if self.mask.is_some() {
            if self.checked {
                let pass = self.fails.is_empty();
                let (txt, col) = if pass { ("PASS".to_string(), Color32::from_rgb(90, 220, 120)) } else { (format!("FAIL   {:.0} % of the band, worst {:+.1} dB at {}", self.fail_pct, self.worst.0, crate::app::fmt_hz(self.worst.1)), Color32::from_rgb(245, 100, 90)) };
                ui.label(RichText::new(txt).size(18.0).strong().color(col));
            } else {
                ui.label(RichText::new("waiting for audio...").weak());
            }
        }
        ui.separator();
        // A/B difference
        heading(ui, "Difference (A - B)");
        ui.horizontal_wrapped(|ui| {
            let mut pick = |ui: &mut Ui, id: &str, which: &mut Src| {
                let txt = self.src_name(*which, cx);
                egui::ComboBox::from_id_salt(id).width(120.0).selected_text(txt).show_ui(ui, |ui| {
                    ui.selectable_value(which, Src::Live, "live (avg)");
                    for (i, r) in self.refs.iter().enumerate() {
                        ui.selectable_value(which, Src::Ref(i), &r.name);
                    }
                    for (i, s) in cx.snaps.iter().enumerate() {
                        ui.selectable_value(which, Src::Snap(i), format!("snap {}", s.name));
                    }
                });
            };
            let (mut a, mut b) = (self.a, self.b);
            ui.label("A");
            pick(ui, "ref_a", &mut a);
            ui.label("B");
            pick(ui, "ref_b", &mut b);
            self.a = a;
            self.b = b;
            if ui.button("swap").clicked() {
                std::mem::swap(&mut self.a, &mut self.b);
            }
            if ui.button("Align live to ref at 1 kHz").on_hover_text("Shift the selected mask / reference so it meets the live level at 1 kHz").clicked() {
                if let (Some(i), Some(live)) = (self.mask.or(match self.b { Src::Ref(k) => Some(k), _ => None }), spec_at(cx.an, &cx.an.avg[self.ch], 1000.0)) {
                    if let Some(r) = self.refs.get_mut(i) {
                        if let Some(v) = interp(&r.pts, 1000.0) {
                            r.offset = live - v;
                        }
                    }
                }
            }
            ui.add(egui::DragValue::new(&mut self.y_range).range(2.0..=100.0).speed(0.2).prefix("+/-").suffix(" dB"));
        });
        let (resp, painter, rect) = canvas(ui, 110.0);
        let pr = inset(rect);
        if pr.width() < 40.0 || pr.height() < 30.0 || cx.an.n == 0 {
            return;
        }
        let nyq = cx.sr * 0.5;
        let plot = Plot::new(pr, 20.0, nyq.min(20000.0), -self.y_range, self.y_range, true);
        plot.grid_y(&painter, &|v| format!("{v:+.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        let diff = |f: f32| -> Option<f32> { Some(self.eval(self.a, cx, f)? - self.eval(self.b, cx, f)?) };
        let n = pr.width() as usize;
        let pts: Vec<(f32, f32)> = (0..n).filter_map(|i| {
            let f = plot.fx(pr.left() + i as f32 + 0.5);
            diff(f).map(|d| (f, d))
        }).collect();
        // tolerance of the selected reference
        if let Some(r) = self.mask.and_then(|m| self.refs.get(m)) {
            if r.limits.is_none() {
                for s in [-1.0, 1.0] {
                    let y = plot.ty(s * r.tol);
                    painter.with_clip_rect(pr).line_segment([Pos2::new(pr.left(), y), Pos2::new(pr.right(), y)], Stroke::new(1.0, Color32::from_rgb(255, 120, 100).gamma_multiply(0.7)));
                }
            }
        }
        plot.line(&painter, pts.iter().copied(), Stroke::new(1.5, Color32::from_rgb(120, 220, 255)));
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, "dB", FontId::proportional(9.0), crate::views::TXT);
        if let Some(h) = resp.hover_pos().filter(|p| pr.contains(*p)) {
            let f = plot.fx(h.x);
            if let Some(d) = diff(f) {
                plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), format!("{}   {d:+.2} dB", crate::app::fmt_hz(f)), Color32::WHITE, Align2::RIGHT_TOP);
            }
        }
    }

    fn overlay(&self, ov: &SpecOverlay, cx: &Ctx) {
        let off = cx.unit_off[self.ch];
        let clip = ov.p;
        // failing regions first, behind the curves
        for &(a, b) in &self.fails {
            let (x0, x1) = ((ov.x_of_f)(a), (ov.x_of_f)(b));
            if x1 < ov.plot.left() || x0 > ov.plot.right() {
                continue;
            }
            let r = egui::Rect::from_min_max(Pos2::new(x0.max(ov.plot.left()), ov.plot.top()), Pos2::new(x1.min(ov.plot.right()).max(x0 + 2.0), ov.plot.bottom()));
            clip.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255, 60, 60, 28));
        }
        for (i, r) in self.refs.iter().enumerate() {
            if !r.show {
                continue;
            }
            let line: Vec<Pos2> = r.pts.iter().map(|p| Pos2::new((ov.x_of_f)(p.0), (ov.y_of_db)(p.1 + r.offset + off))).collect();
            if self.mask == Some(i) {
                // band between the limits
                let lim: Vec<(f32, f32)> = match &r.limits {
                    Some(l) => l.to_vec(),
                    None => r.pts.iter().map(|p| (p.1 - r.tol, p.1 + r.tol)).collect(),
                };
                let mut mesh = epaint::Mesh::default();
                let fill = Color32::from_rgba_unmultiplied(r.color.r(), r.color.g(), r.color.b(), 30);
                let (mut lo_l, mut hi_l) = (Vec::new(), Vec::new());
                for (p, l) in r.pts.iter().zip(&lim) {
                    let x = (ov.x_of_f)(p.0);
                    let (yl, yh) = ((ov.y_of_db)(l.0 + r.offset + off), (ov.y_of_db)(l.1 + r.offset + off));
                    mesh.vertices.push(epaint::Vertex { pos: Pos2::new(x, yh), uv: epaint::WHITE_UV, color: fill });
                    mesh.vertices.push(epaint::Vertex { pos: Pos2::new(x, yl), uv: epaint::WHITE_UV, color: fill });
                    lo_l.push(Pos2::new(x, yl));
                    hi_l.push(Pos2::new(x, yh));
                }
                for k in 0..(r.pts.len() as u32).saturating_sub(1) {
                    let a = k * 2;
                    mesh.indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
                }
                clip.add(egui::Shape::mesh(mesh));
                clip.add(egui::Shape::line(lo_l, Stroke::new(1.0, r.color.gamma_multiply(0.8))));
                clip.add(egui::Shape::line(hi_l, Stroke::new(1.0, r.color.gamma_multiply(0.8))));
            } else {
                clip.add(egui::Shape::line(line, Stroke::new(1.3, r.color)));
            }
        }
    }

    fn status(&self) -> Option<(String, Color32)> {
        if self.mask.is_some() && self.checked {
            return Some(if self.fails.is_empty() { ("mask PASS".into(), Color32::from_rgb(110, 210, 130)) } else { (format!("mask FAIL {:.0}%", self.fail_pct), Color32::from_rgb(245, 100, 90)) });
        }
        None
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("ch", self.ch);
        o.put("y_range", self.y_range);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("ch", &mut self.ch);
        i.set("y_range", &mut self.y_range);
        self.ch = self.ch.min(1);
        self.y_range = self.y_range.clamp(2.0, 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_variants() {
        let (p, l) = parse_csv("freq,level\n100,-3\n1000,0.5\n10000,-6\n").unwrap();
        assert_eq!(p.len(), 3);
        assert!(l.is_none());
        let (p, l) = parse_csv("20;-5,5;-3,5\n20000;-2;2\n").unwrap();
        assert_eq!(p.len(), 2);
        let l = l.unwrap();
        assert_eq!(l[0], (-5.5, -3.5));
        assert!((p[0].1 + 4.5).abs() < 1e-6);
        assert!(parse_csv("nothing here").is_err());
        assert!((interp(&[(100.0, 0.0), (10000.0, -20.0)], 1000.0).unwrap() + 10.0).abs() < 1e-3);
    }
}
