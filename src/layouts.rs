//! Layout presets (built-in and user-saved), pop-out windows and keyboard shortcuts.

use crate::app::App;
use crate::windows::{win_short, WinState, BUILTIN, GAP, WIN_COUNT};
use eframe::egui::{self, vec2, Rect, Ui};

/// The arrangement of all windows at one moment.
#[derive(Clone, Copy)]
pub struct LaySnap {
    wins: [WinState; WIN_COUNT],
    popped: [bool; WIN_COUNT],
}

impl LaySnap {
    /// Equal up to a pixel, closed windows ignored.
    fn same(&self, o: &LaySnap) -> bool {
        let q = |r: Option<Rect>| r.map(|r| [r.min.x.round() as i32, r.min.y.round() as i32, r.max.x.round() as i32, r.max.y.round() as i32]);
        (0..WIN_COUNT).all(|i| {
            let (a, b) = (&self.wins[i], &o.wins[i]);
            a.open == b.open && (!a.open || (a.collapsed == b.collapsed && self.popped[i] == o.popped[i] && q(a.rect) == q(b.rect)))
        })
    }
}

#[derive(Default)]
pub struct LayHist {
    cur: Option<LaySnap>,
    undo: Vec<LaySnap>,
    redo: Vec<LaySnap>,
    size: Option<egui::Vec2>,
}

const fn t(k: usize) -> usize {
    BUILTIN + k
}
// tool windows (index into `tools::INFO`)
const REC: usize = t(0);
const OCT: usize = t(1);
const RESP: usize = t(2);
const IMP: usize = t(3);
const PEAKS: usize = t(4);
const LOUD: usize = t(5);
const STATS: usize = t(6);
const LOG: usize = t(7);
const REFN: usize = t(8);
const SPL: usize = t(10);
const DIST: usize = t(11);
const EQ: usize = t(14);
const DAW: usize = t(15);
// built-in windows
const SPEC: usize = 0;
const SPG: usize = 1;
const STEREO: usize = 2;
const D3: usize = 3;
const SCOPE: usize = 4;
const MEAS: usize = 5;
const GEN: usize = 6;

/// A built-in preset: the open windows and, optionally, where each goes (x, y, w, h as fractions of the
/// area, same order as `wins`). Without rects the windows are tiled by importance.
struct Preset {
    group: &'static str,
    name: &'static str,
    wins: &'static [usize],
    rects: &'static [[f32; 4]],
}

const fn p(group: &'static str, name: &'static str, wins: &'static [usize], rects: &'static [[f32; 4]]) -> Preset {
    Preset { group, name, wins, rects }
}

/// Ctrl+1..9 pick the first nine.
const PRESETS: [Preset; 24] = [
    p("Classic", "Overview", &[0, 1, 2, 3, 4, 5], &[]),
    p("Classic", "Analysis", &[SPEC, SPG, MEAS, OCT, PEAKS], &[]),
    p("Classic", "Scope & generator", &[SCOPE, SPEC, GEN, MEAS], &[]),
    p("Classic", "Mastering", &[SPEC, STEREO, OCT, LOUD], &[]),
    p("Classic", "Acoustics lab", &[GEN, RESP, IMP, SPEC, MEAS], &[]),
    p("Classic", "Monitoring", &[SPEC, MEAS, STATS, LOG, REFN], &[]),
    p("Timeline", "Studio", &[DAW, SPEC, SPG, LOUD], &[[0.0, 0.46, 1.0, 0.54], [0.0, 0.0, 0.40, 0.46], [0.40, 0.0, 0.35, 0.46], [0.75, 0.0, 0.25, 0.46]]),
    p("Timeline", "Record & monitor", &[DAW, SCOPE, SPEC, STEREO], &[[0.0, 0.0, 1.0, 0.60], [0.0, 0.60, 0.34, 0.40], [0.34, 0.60, 0.38, 0.40], [0.72, 0.60, 0.28, 0.40]]),
    p("Timeline", "Mix & master", &[DAW, SPEC, OCT, LOUD, STEREO], &[[0.0, 0.52, 1.0, 0.48], [0.0, 0.0, 0.32, 0.52], [0.32, 0.0, 0.28, 0.52], [0.60, 0.0, 0.20, 0.52], [0.80, 0.0, 0.20, 0.52]]),
    p("Timeline", "Edit & measure", &[DAW, SPEC, MEAS, STATS], &[[0.0, 0.0, 0.70, 1.0], [0.70, 0.0, 0.30, 0.40], [0.70, 0.40, 0.30, 0.30], [0.70, 0.70, 0.30, 0.30]]),
    p("Timeline", "Timeline + spectrum", &[DAW, SPEC], &[[0.0, 0.0, 1.0, 0.68], [0.0, 0.68, 1.0, 0.32]]),
    p("Timeline", "Timeline only", &[DAW], &[[0.0, 0.0, 1.0, 1.0]]),
    p("Views", "Spectrum + spectrogram", &[SPEC, SPG], &[[0.0, 0.0, 1.0, 0.55], [0.0, 0.55, 1.0, 0.45]]),
    p("Views", "Quad view", &[SPEC, SPG, SCOPE, STEREO], &[[0.0, 0.0, 0.5, 0.5], [0.5, 0.0, 0.5, 0.5], [0.0, 0.5, 0.5, 0.5], [0.5, 0.5, 0.5, 0.5]]),
    p("Views", "Waterfall 3D", &[SPEC, D3, SPG], &[[0.0, 0.0, 0.45, 1.0], [0.45, 0.0, 0.55, 0.60], [0.45, 0.60, 0.55, 0.40]]),
    p("Views", "Spectrum only", &[SPEC], &[[0.0, 0.0, 1.0, 1.0]]),
    p("Views", "Tuner", &[PEAKS, SPEC, SCOPE], &[[0.0, 0.0, 0.35, 1.0], [0.35, 0.0, 0.65, 0.55], [0.35, 0.55, 0.65, 0.45]]),
    p("Views", "Loudness check", &[LOUD, STATS, SPL, STEREO], &[[0.0, 0.0, 0.5, 0.6], [0.5, 0.0, 0.5, 0.6], [0.0, 0.6, 0.5, 0.4], [0.5, 0.6, 0.5, 0.4]]),
    p("Acoustics", "Speaker measurement", &[RESP, IMP, SPEC, GEN, MEAS], &[[0.0, 0.0, 0.62, 0.62], [0.62, 0.0, 0.38, 0.62], [0.0, 0.62, 0.40, 0.38], [0.40, 0.62, 0.30, 0.38], [0.70, 0.62, 0.30, 0.38]]),
    p("Acoustics", "Room EQ", &[RESP, EQ, REFN, IMP], &[[0.0, 0.0, 0.55, 0.55], [0.55, 0.0, 0.45, 0.55], [0.0, 0.55, 0.40, 0.45], [0.40, 0.55, 0.60, 0.45]]),
    p("Acoustics", "Distortion lab", &[DIST, SPEC, MEAS, GEN], &[[0.0, 0.0, 0.60, 1.0], [0.60, 0.0, 0.40, 0.50], [0.60, 0.50, 0.20, 0.50], [0.80, 0.50, 0.20, 0.50]]),
    p("Acoustics", "Impulse & RT60", &[IMP, RESP, GEN], &[[0.0, 0.0, 0.6, 1.0], [0.6, 0.0, 0.4, 0.65], [0.6, 0.65, 0.4, 0.35]]),
    p("More", "Recorder + spectrum", &[REC, SPEC, SPG], &[[0.0, 0.0, 0.34, 1.0], [0.34, 0.0, 0.66, 0.5], [0.34, 0.5, 0.66, 0.5]]),
    p("More", "Everything", &[0, 1, 2, 3, 4, 5, OCT, LOUD, DAW], &[]),
];

#[derive(Clone)]
pub struct UserPreset {
    pub name: String,
    pub open: Vec<bool>,
    /// x, y, w, h as fractions of the window area
    pub rects: Vec<Option<[f32; 4]>>,
}

impl UserPreset {
    fn encode(&self) -> String {
        let open: String = self.open.iter().map(|&o| if o { '1' } else { '0' }).collect();
        let rects: Vec<String> = self.rects.iter().map(|r| r.map(|r| format!("{:.4},{:.4},{:.4},{:.4}", r[0], r[1], r[2], r[3])).unwrap_or_else(|| "-".into())).collect();
        format!("{}|{}|{}", self.name, open, rects.join(";"))
    }

    fn decode(s: &str) -> Option<Self> {
        let mut p = s.split('|');
        let (name, open, rects) = (p.next()?, p.next()?, p.next()?);
        let mut open: Vec<bool> = open.chars().map(|c| c == '1').collect();
        let mut rects: Vec<Option<[f32; 4]>> = rects
            .split(';')
            .map(|r| {
                let v: Vec<f32> = r.split(',').filter_map(|x| x.parse().ok()).collect();
                (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
            })
            .collect();
        open.resize(WIN_COUNT, false);
        rects.resize(WIN_COUNT, None);
        (!name.is_empty()).then(|| Self { name: name.to_string(), open, rects })
    }
}

impl App {
    pub fn presets_save(&self, out: &mut String) {
        use std::fmt::Write;
        for (i, p) in self.presets.iter().enumerate() {
            let _ = writeln!(out, "preset_{i}={}", p.encode());
        }
    }

    pub fn presets_load(&mut self, m: &std::collections::HashMap<&str, &str>) {
        self.presets.clear();
        for i in 0..32 {
            match m.get(format!("preset_{i}").as_str()).and_then(|s| UserPreset::decode(s)) {
                Some(p) => self.presets.push(p),
                None => break,
            }
        }
    }

    fn apply_builtin(&mut self, k: usize) {
        let pr = &PRESETS[k];
        for i in 0..WIN_COUNT {
            let slot = pr.wins.iter().position(|&w| w == i);
            self.wins[i].open = slot.is_some();
            self.wins[i].prev = None;
            self.wins[i].collapsed = false;
            if let (Some(s), Some(area)) = (slot, self.area) {
                let Some(f) = pr.rects.get(s) else { continue };
                let r = Rect::from_min_size(area.min + vec2(f[0] * area.width(), f[1] * area.height()), vec2(f[2] * area.width(), f[3] * area.height()));
                self.wins[i].rect = Some(r.shrink(GAP * 0.5));
                self.popped[i] = false;
            }
        }
        if pr.rects.is_empty() || self.area.is_none() {
            self.need_tile = true;
        } else {
            // the arrangement is the preset's: do not re-tile because the set of open windows changed
            self.open_mask = self.current_mask();
        }
    }

    fn apply_user(&mut self, k: usize) {
        let (Some(p), Some(area)) = (self.presets.get(k).cloned(), self.area) else { return };
        for i in 0..WIN_COUNT {
            self.wins[i].open = p.open[i];
            self.wins[i].prev = None;
            self.wins[i].collapsed = false;
            if let (true, Some(r)) = (p.open[i], p.rects[i]) {
                self.wins[i].rect = Some(Rect::from_min_size(area.min + vec2(r[0] * area.width(), r[1] * area.height()), vec2(r[2] * area.width(), r[3] * area.height())));
            }
        }
        // the arrangement is the user's: do not re-tile because the set of open windows changed
        self.open_mask = self.current_mask();
    }

    fn save_current_preset(&mut self) {
        let Some(area) = self.area else { return };
        let name: String = self.preset_name.trim().chars().filter(|c| !matches!(c, '|' | '=' | '\n' | '\r')).collect();
        if name.is_empty() {
            return;
        }
        let rects = (0..WIN_COUNT)
            .map(|i| {
                self.wins[i].rect.filter(|_| self.wins[i].open).map(|r| {
                    let rel = r.min - area.min;
                    [rel.x / area.width(), rel.y / area.height(), r.width() / area.width(), r.height() / area.height()]
                })
            })
            .collect();
        let preset = UserPreset { name: name.clone(), open: (0..WIN_COUNT).map(|i| self.wins[i].open).collect(), rects };
        match self.presets.iter().position(|p| p.name == name) {
            Some(i) => self.presets[i] = preset,
            None => self.presets.push(preset),
        }
        self.note = format!("Saved layout \"{name}\"");
    }

    /// Ctrl+1..9 switches to the n-th preset (built-in first, then saved ones).
    pub fn handle_hotkeys(&mut self, ctx: &egui::Context) {
        use egui::Key::*;
        const KEYS: [egui::Key; 9] = [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9];
        let ctrl_alt = egui::Modifiers::COMMAND | egui::Modifiers::ALT;
        if ctx.input_mut(|i| i.consume_key(ctrl_alt, Z)) {
            self.layout_undo();
        }
        if ctx.input_mut(|i| i.consume_key(ctrl_alt, Y)) {
            self.layout_redo();
        }
        let hit = ctx.input(|i| i.modifiers.command && !i.modifiers.shift && !i.modifiers.alt).then(|| ctx.input(|i| KEYS.iter().position(|k| i.key_pressed(*k)))).flatten();
        if let Some(n) = hit {
            self.apply_preset_index(n);
        }
    }

    pub fn apply_preset_index(&mut self, n: usize) {
        if n < PRESETS.len() {
            self.apply_builtin(n);
        } else if n - PRESETS.len() < self.presets.len() {
            self.apply_user(n - PRESETS.len());
        }
    }

    // ------------------------------------------------------------------ undo / redo of the arrangement

    fn lay_snap(&self) -> LaySnap {
        LaySnap { wins: self.wins, popped: self.popped }
    }

    pub fn lay_can_undo(&self) -> bool {
        !self.lay.undo.is_empty()
    }

    pub fn lay_can_redo(&self) -> bool {
        !self.lay.redo.is_empty()
    }

    /// Called every frame: a finished change of the arrangement becomes one undo step.
    pub fn track_layout(&mut self) {
        if !self.layout_ready || self.settle > 0 || self.win_drag.is_some() || self.need_tile {
            return;
        }
        let now = self.lay_snap();
        let size = self.area.map(|a| a.size());
        // the application window or the side bar was resized: not a layout change of the user's
        if self.lay.size != size {
            self.lay.size = size;
            self.lay.cur = Some(now);
            return;
        }
        match self.lay.cur {
            Some(c) if !c.same(&now) => {
                self.lay.undo.push(c);
                if self.lay.undo.len() > 60 {
                    self.lay.undo.remove(0);
                }
                self.lay.redo.clear();
                self.lay.cur = Some(now);
            }
            None => self.lay.cur = Some(now),
            _ => {}
        }
    }

    fn lay_restore(&mut self, s: LaySnap) {
        self.wins = s.wins;
        self.popped = s.popped;
        self.open_mask = self.current_mask();
        self.need_tile = false;
        self.lay.cur = Some(s);
    }

    pub fn layout_undo(&mut self) {
        if let Some(prev) = self.lay.undo.pop() {
            if let Some(c) = self.lay.cur {
                self.lay.redo.push(c);
            }
            self.lay_restore(prev);
        }
    }

    pub fn layout_redo(&mut self) {
        if let Some(next) = self.lay.redo.pop() {
            if let Some(c) = self.lay.cur {
                self.lay.undo.push(c);
            }
            self.lay_restore(next);
        }
    }

    pub fn layouts_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("Layouts", |ui| {
            if ui.add_enabled(self.lay_can_undo(), egui::Button::new("Undo layout change").shortcut_text("Ctrl+Alt+Z")).clicked() {
                self.layout_undo();
                ui.close();
            }
            if ui.add_enabled(self.lay_can_redo(), egui::Button::new("Redo layout change").shortcut_text("Ctrl+Alt+Y")).clicked() {
                self.layout_redo();
                ui.close();
            }
            ui.separator();
            let mut group = "";
            for (k, pr) in PRESETS.iter().enumerate() {
                if pr.group != group {
                    group = pr.group;
                    if k > 0 {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new(group).small().weak());
                }
                let mut b = egui::Button::new(pr.name);
                if k < 9 {
                    b = b.shortcut_text(format!("Ctrl+{}", k + 1));
                }
                if ui.add(b).clicked() {
                    self.apply_builtin(k);
                    ui.close();
                }
            }
            if !self.presets.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("Saved").small().weak());
            }
            let mut delete = None;
            for k in 0..self.presets.len() {
                let n = PRESETS.len() + k;
                ui.horizontal(|ui| {
                    let mut b = egui::Button::new(&self.presets[k].name);
                    if n < 9 {
                        b = b.shortcut_text(format!("Ctrl+{}", n + 1));
                    }
                    if ui.add(b).clicked() {
                        self.apply_user(k);
                        ui.close();
                    }
                    if ui.small_button("x").on_hover_text("Delete this layout").clicked() {
                        delete = Some(k);
                    }
                });
            }
            if let Some(k) = delete {
                self.presets.remove(k);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("layout name").desired_width(110.0));
                if ui.add_enabled(!self.preset_name.trim().is_empty(), egui::Button::new("Save current")).clicked() {
                    self.save_current_preset();
                    ui.close();
                }
            });
            ui.separator();
            ui.menu_button("Pop out to own window", |ui| {
                ui.label(egui::RichText::new("Drag the new window to another monitor.\nClosing it docks it back.").small().weak());
                for i in 0..WIN_COUNT {
                    let mut p = self.popped[i] && self.wins[i].open;
                    let label = if i < BUILTIN { win_short(i).to_string() } else { format!("{} (tool)", win_short(i)) };
                    if ui.checkbox(&mut p, label).changed() {
                        self.popped[i] = p;
                        if p {
                            self.wins[i].open = true;
                        }
                        self.need_tile |= self.auto_tile;
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_roundtrip() {
        let mut open = vec![false; WIN_COUNT];
        open[0] = true;
        open[10] = true;
        let mut rects = vec![None; WIN_COUNT];
        rects[0] = Some([0.1, 0.2, 0.5, 0.6]);
        let p = UserPreset { name: "My layout".into(), open, rects };
        let q = UserPreset::decode(&p.encode()).unwrap();
        assert_eq!(q.name, "My layout");
        assert!(q.open[0] && q.open[10] && !q.open[1]);
        assert_eq!(q.rects[0], Some([0.1, 0.2, 0.5, 0.6]));
        assert!(q.rects[1].is_none());
    }

    #[test]
    fn presets_use_valid_windows() {
        for pr in PRESETS.iter() {
            assert!(pr.wins.iter().all(|&i| i < WIN_COUNT), "{}", pr.name);
            assert!(pr.rects.is_empty() || pr.rects.len() == pr.wins.len(), "{}", pr.name);
            for r in pr.rects {
                assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= 1.001 && r[1] + r[3] <= 1.001, "{}", pr.name);
            }
            // no two windows of a preset overlap
            for (a, ra) in pr.rects.iter().enumerate() {
                for rb in pr.rects.iter().skip(a + 1) {
                    let ox = ra[0] < rb[0] + rb[2] - 1e-4 && rb[0] < ra[0] + ra[2] - 1e-4;
                    let oy = ra[1] < rb[1] + rb[3] - 1e-4 && rb[1] < ra[1] + ra[3] - 1e-4;
                    assert!(!(ox && oy), "{} overlaps", pr.name);
                }
            }
        }
    }
}
