//! Library of stored measurements (frequency response curves) shared by the Response and Transfer
//! windows: store, name, recolour, overlay, show relative to one trace, average, save and load.
//! The library survives restarts (`traces.txt` next to the settings).

use super::*;
use eframe::egui::{self, RichText};

pub const PALETTE: [Color32; 8] = [
    Color32::from_rgb(255, 214, 10),
    Color32::from_rgb(120, 255, 120),
    Color32::from_rgb(255, 140, 60),
    Color32::from_rgb(190, 130, 255),
    Color32::from_rgb(255, 255, 255),
    Color32::from_rgb(255, 100, 160),
    Color32::from_rgb(90, 170, 255),
    Color32::from_rgb(160, 220, 80),
];

#[derive(Clone)]
pub struct Meas {
    pub name: String,
    pub color: Color32,
    pub visible: bool,
    pub f: Vec<f32>,
    /// dB, may contain NaN where there was no data
    pub mag: Vec<f32>,
    /// degrees, empty when unknown
    pub phase: Vec<f32>,
}

impl Meas {
    /// dB at `freq`, interpolated on a log axis; NaN outside the stored range.
    pub fn at(&self, freq: f32) -> f32 {
        let f = &self.f;
        if f.len() < 2 || freq < f[0] || freq > *f.last().unwrap() {
            return f32::NAN;
        }
        let j = f.partition_point(|&x| x < freq).clamp(1, f.len() - 1);
        let t = (freq.ln() - f[j - 1].ln()) / (f[j].ln() - f[j - 1].ln()).max(1e-9);
        self.mag[j - 1] + t * (self.mag[j] - self.mag[j - 1])
    }
}

#[derive(Default)]
pub struct Library {
    pub items: Vec<Meas>,
    /// show everything relative to this trace
    pub rel: Option<usize>,
    pub dirty: bool,
    counter: usize,
    save_dlg: Option<dialogs::Pending>,
    load_dlg: Option<dialogs::Pending>,
}

fn auto_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("traces.txt")
}

/// Smooth a dB curve (NaN-tolerant) over 1/`n` octave on a log grid with `ppo` points per octave.
pub fn smooth_db(v: &[f32], ppo: f32, n: f32) -> Vec<f32> {
    if n <= 0.0 {
        return v.to_vec();
    }
    let clean: Vec<f32> = v.iter().map(|x| if x.is_finite() { *x } else { -300.0 }).collect();
    let mut out = super::response::smooth_log(&clean, ppo, n);
    for (o, x) in out.iter_mut().zip(v) {
        if !x.is_finite() {
            *o = f32::NAN;
        }
    }
    out
}

impl Library {
    pub fn add(&mut self, name: Option<String>, f: Vec<f32>, mag: Vec<f32>, phase: Vec<f32>) {
        self.counter += 1;
        let id = self.counter;
        self.items.push(Meas { name: name.unwrap_or_else(|| format!("Trace {id}")), color: PALETTE[(id - 1) % PALETTE.len()], visible: true, f, mag, phase });
        self.dirty = true;
    }

    /// Overlay curves and the reference (if any) on the frequency grid `f`, smoothed 1/`n` octave.
    /// Returns (name, colour, dB values) per visible trace, already made relative to the reference.
    pub fn layers(&self, f: &[f32], ppo: f32, n: f32) -> (Vec<(String, Color32, Vec<f32>)>, Option<Vec<f32>>) {
        let curve = |m: &Meas| -> Vec<f32> {
            let raw: Vec<f32> = f.iter().map(|&x| m.at(x)).collect();
            smooth_db(&raw, ppo, n)
        };
        let rel = self.rel.and_then(|i| self.items.get(i)).map(curve);
        let sub = |v: Vec<f32>| -> Vec<f32> {
            match &rel {
                Some(r) => v.iter().zip(r).map(|(a, b)| a - b).collect(),
                None => v,
            }
        };
        let layers = self.items.iter().filter(|m| m.visible).map(|m| (m.name.clone(), m.color, sub(curve(m)))).collect();
        (layers, rel)
    }

    fn average_visible(&mut self) {
        let vis: Vec<&Meas> = self.items.iter().filter(|m| m.visible).collect();
        if vis.len() < 2 {
            return;
        }
        // common grid: the widest log range, 900 points
        let f0 = vis.iter().map(|m| m.f[0]).fold(f32::MAX, f32::min);
        let f1 = vis.iter().map(|m| *m.f.last().unwrap()).fold(0.0, f32::max);
        let f: Vec<f32> = (0..900).map(|i| f0 * (f1 / f0).powf(i as f32 / 899.0)).collect();
        let mag: Vec<f32> = f
            .iter()
            .map(|&x| {
                let v: Vec<f32> = vis.iter().map(|m| m.at(x)).filter(|v| v.is_finite()).collect();
                if v.is_empty() { f32::NAN } else { 10.0 * (v.iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / v.len() as f32).log10() }
            })
            .collect();
        let name = format!("Average of {}", vis.len());
        self.add(Some(name), f, mag, Vec::new());
    }

    // ---------------------------------------------------------------- file format

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for m in &self.items {
            s += &format!("#trace;{};{:02x}{:02x}{:02x};{}\n", m.name.replace(';', ","), m.color.r(), m.color.g(), m.color.b(), m.visible as u8);
            for i in 0..m.f.len() {
                let ph = m.phase.get(i).map(|p| format!("{p:.2}")).unwrap_or_default();
                s += &format!("{:.3},{:.3},{ph}\n", m.f[i], m.mag[i]);
            }
        }
        s
    }

    /// Parse the library format; plain "frequency dB [phase]" tables load as one trace named `fallback`.
    pub fn parse(text: &str, fallback: &str) -> Vec<Meas> {
        if !text.contains("#trace;") {
            return match crate::cal::parse_mic_cal(fallback, text) {
                Some(c) => vec![Meas { name: c.name, color: PALETTE[0], visible: true, f: c.f, mag: c.db, phase: Vec::new() }],
                None => Vec::new(),
            };
        }
        let mut out: Vec<Meas> = Vec::new();
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("#trace;") {
                let p: Vec<&str> = h.split(';').collect();
                let c = p.get(2).and_then(|c| u32::from_str_radix(c, 16).ok()).unwrap_or(0xffffff);
                out.push(Meas {
                    name: p.first().unwrap_or(&"Trace").to_string(),
                    color: Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8),
                    visible: p.get(3).map(|v| *v != "0").unwrap_or(true),
                    f: Vec::new(),
                    mag: Vec::new(),
                    phase: Vec::new(),
                });
            } else if let Some(m) = out.last_mut() {
                let mut it = line.split(',');
                let (Some(a), Some(b)) = (it.next().and_then(|v| v.trim().parse::<f32>().ok()), it.next().and_then(|v| v.trim().parse::<f32>().ok().or(Some(f32::NAN)))) else { continue };
                m.f.push(a);
                m.mag.push(b);
                if let Some(p) = it.next().and_then(|v| v.trim().parse::<f32>().ok()) {
                    m.phase.resize(m.f.len() - 1, 0.0);
                    m.phase.push(p);
                }
            }
        }
        out.retain(|m| m.f.len() >= 2 && m.f.windows(2).all(|w| w[1] > w[0]));
        out
    }

    pub fn load_auto(&mut self) {
        if let Ok(t) = std::fs::read_to_string(auto_path()) {
            self.items = Self::parse(&t, "trace");
            self.counter = self.items.len();
        }
    }

    pub fn save_auto(&mut self) {
        if !self.dirty {
            return;
        }
        let p = auto_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if std::fs::write(p, self.to_text()).is_ok() {
            self.dirty = false;
        }
    }

    /// Collect finished file dialogs; returns a status message.
    pub fn poll(&mut self) -> Option<String> {
        let mut note = None;
        if let Some(d) = &self.save_dlg {
            if let Some(res) = d.take() {
                self.save_dlg = None;
                if let Some(path) = res {
                    note = Some(match std::fs::write(&path, self.to_text()) {
                        Ok(_) => format!("Saved {} traces to {}", self.items.len(), path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
        if let Some(d) = &self.load_dlg {
            if let Some(res) = d.take() {
                self.load_dlg = None;
                if let Some(path) = res {
                    let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let new = std::fs::read(&path).map(|b| Self::parse(&String::from_utf8_lossy(&b), &name)).unwrap_or_default();
                    note = Some(if new.is_empty() { format!("{}: no curves found", path.display()) } else { format!("Loaded {} trace(s)", new.len()) });
                    for m in new {
                        self.counter += 1;
                        self.items.push(m);
                        self.dirty = true;
                    }
                }
            }
        }
        note
    }

    /// Compact trace manager: `store` is the curve the host window can store right now.
    pub fn ui(&mut self, ui: &mut Ui, store: Option<(Vec<f32>, Vec<f32>, Vec<f32>)>) {
        let n = self.items.len();
        let title = if n > 0 { format!("Traces ({n})") } else { "Traces".to_string() };
        ui.horizontal(|ui| {
            if ui.add_enabled(store.is_some(), egui::Button::new("Store")).on_hover_text("Keep the current curve (unsmoothed) as a named trace to overlay and compare").clicked() {
                if let Some((f, m, p)) = store {
                    self.add(None, f, m, p);
                }
            }
            if let Some(r) = self.rel {
                let name = self.items.get(r).map(|m| m.name.clone()).unwrap_or_default();
                ui.label(RichText::new(format!("relative to {name}")).small().color(Color32::from_rgb(255, 214, 10)));
                if ui.small_button("absolute").clicked() {
                    self.rel = None;
                }
            }
        });
        egui::CollapsingHeader::new(title).default_open(false).id_salt("traces_header").show(ui, |ui| {
            let mut del = None;
            let mut dirty = false;
            for (i, m) in self.items.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    dirty |= ui.checkbox(&mut m.visible, "").changed();
                    dirty |= ui.color_edit_button_srgba(&mut m.color).changed();
                    dirty |= ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(120.0).text_color(m.color)).changed();
                    if ui.selectable_label(self.rel == Some(i), "ref").on_hover_text("Show every curve relative to this one (A - B)").clicked() {
                        self.rel = if self.rel == Some(i) { None } else { Some(i) };
                    }
                    if ui.small_button("x").clicked() {
                        del = Some(i);
                    }
                });
            }
            if let Some(i) = del {
                self.items.remove(i);
                self.rel = match self.rel {
                    Some(r) if r == i => None,
                    Some(r) if r > i => Some(r - 1),
                    r => r,
                };
                dirty = true;
            }
            self.dirty |= dirty;
            ui.horizontal(|ui| {
                if ui.add_enabled(self.items.iter().filter(|m| m.visible).count() >= 2, egui::Button::new("Average visible")).on_hover_text("New trace: power average of the visible traces").clicked() {
                    self.average_visible();
                }
                if ui.add_enabled(self.save_dlg.is_none() && n > 0, egui::Button::new("Save...")).clicked() {
                    self.save_dlg = Some(dialogs::save("Save traces", "Traces", &["txt"], "traces.txt".into()));
                }
                if ui.add_enabled(self.load_dlg.is_none(), egui::Button::new("Load...")).on_hover_text("Traces file, or any text table of frequency and dB (REW export, microphone calibration ...)").clicked() {
                    self.load_dlg = Some(dialogs::open("Load traces", "Traces", &["txt", "cal", "csv", "frd"]));
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(gain: f32) -> (Vec<f32>, Vec<f32>) {
        let f: Vec<f32> = (0..100).map(|i| 20.0 * 1000f32.powf(i as f32 / 99.0)).collect();
        let m = f.iter().map(|x| gain + (x.log10() - 2.0)).collect();
        (f, m)
    }

    #[test]
    fn roundtrip_relative_and_average() {
        let mut l = Library::default();
        let (f, m1) = curve(0.0);
        let (_, m2) = curve(6.0);
        l.add(Some("a".into()), f.clone(), m1, vec![]);
        l.add(Some("b;c".into()), f.clone(), m2, f.iter().map(|_| 12.5).collect());
        let back = Library::parse(&l.to_text(), "x");
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].name, "b,c");
        assert_eq!(back[1].phase.len(), back[1].f.len());
        assert!(back[0].phase.is_empty());
        l.rel = Some(0);
        let (layers, rel) = l.layers(&f[10..90], 8.0, 0.0);
        assert!(rel.is_some());
        assert!(layers[1].2.iter().all(|d| (d - 6.0).abs() < 0.01));
        assert!(layers[0].2.iter().all(|d| d.abs() < 0.01));
        l.average_visible();
        let avg = &l.items[2];
        assert!((avg.at(1000.0) - (l.items[0].at(1000.0) + 3.96)).abs() < 0.2);
    }
}
