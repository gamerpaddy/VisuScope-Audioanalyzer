//! Layout presets (built-in and user-saved), pop-out windows and keyboard shortcuts.

use crate::app::App;
use crate::windows::{win_short, BUILTIN, WIN_COUNT};
use eframe::egui::{self, vec2, Rect, Ui};

/// Built-in presets: name and the windows that are open (see `windows::win_short` for the indices).
const PRESETS: [(&str, &[usize]); 6] = [
    ("Overview", &[0, 1, 2, 3, 4, 5]),
    ("Analysis", &[0, 1, 5, 9, 12]),
    ("Scope & generator", &[4, 0, 6, 5]),
    ("Mastering", &[0, 2, 9, 13]),
    ("Acoustics lab", &[6, 10, 11, 0, 5]),
    ("Monitoring", &[0, 5, 14, 15, 16]),
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
        for i in 0..WIN_COUNT {
            self.wins[i].open = PRESETS[k].1.contains(&i);
            self.wins[i].prev = None;
        }
        self.need_tile = true;
    }

    fn apply_user(&mut self, k: usize) {
        let (Some(p), Some(area)) = (self.presets.get(k).cloned(), self.area) else { return };
        for i in 0..WIN_COUNT {
            self.wins[i].open = p.open[i];
            self.wins[i].prev = None;
            if let (true, Some(r)) = (p.open[i], p.rects[i]) {
                self.wins[i].rect = Some(Rect::from_min_size(area.min + vec2(r[0] * area.width(), r[1] * area.height()), vec2(r[2] * area.width(), r[3] * area.height())));
                self.wins[i].epoch += 1;
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
        let hit = ctx.input(|i| i.modifiers.command && !i.modifiers.shift).then(|| ctx.input(|i| KEYS.iter().position(|k| i.key_pressed(*k)))).flatten();
        if let Some(n) = hit {
            self.apply_preset_index(n);
        }
    }

    fn apply_preset_index(&mut self, n: usize) {
        if n < PRESETS.len() {
            self.apply_builtin(n);
        } else if n - PRESETS.len() < self.presets.len() {
            self.apply_user(n - PRESETS.len());
        }
    }

    pub fn layouts_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("Layouts", |ui| {
            for (k, (name, _)) in PRESETS.iter().enumerate() {
                if ui.add(egui::Button::new(*name).shortcut_text(format!("Ctrl+{}", k + 1))).clicked() {
                    self.apply_builtin(k);
                    ui.close();
                }
            }
            if !self.presets.is_empty() {
                ui.separator();
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
        for (_, set) in PRESETS {
            assert!(set.iter().all(|&i| i < WIN_COUNT));
        }
    }
}
