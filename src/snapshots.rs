//! Session snapshots: the whole arrangement (windows, tool settings, generator, the routing graph) stored under a
//! name together with a note, so a measurement setup can be put back exactly. Files live in
//! `%APPDATA%\visuscope\snapshots`.

use crate::app::App;
use eframe::egui::{self, RichText, Ui};
use std::path::PathBuf;

pub struct Snap {
    pub note: String,
    pub settings: String,
    pub graph: String,
}

fn dir() -> PathBuf {
    if let Some(p) = std::env::var_os("VISUSCOPE_SNAP_DIR") {
        return p.into();
    }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("visuscope").join("snapshots")
}

/// A name that is safe as a file name (empty when nothing usable is left).
pub fn clean_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')')).take(60).collect();
    let n = n.trim().to_string();
    let up = n.to_uppercase();
    // Windows device names
    if matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL") || ((up.starts_with("COM") || up.starts_with("LPT")) && up.len() == 4 && up[3..].chars().all(|c| c.is_ascii_digit())) {
        return format!("{n} 1");
    }
    n
}

fn esc(s: &str) -> String {
    s.replace('%', "%25").replace('\n', "%0A").replace('\r', "")
}

fn unesc(s: &str) -> String {
    s.replace("%0A", "\n").replace("%25", "%")
}

pub fn encode(s: &Snap) -> String {
    format!("VSNAP 1\nnote={}\n---settings\n{}\n---graph\n{}", esc(&s.note), s.settings.trim_end(), s.graph.trim_end())
}

pub fn decode(text: &str) -> Option<Snap> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "VSNAP 1" {
        return None;
    }
    let note = unesc(lines.next()?.strip_prefix("note=")?);
    let rest: Vec<&str> = lines.collect();
    let si = rest.iter().position(|l| *l == "---settings")?;
    let gi = rest.iter().position(|l| *l == "---graph")?;
    if gi < si {
        return None;
    }
    Some(Snap { note, settings: rest[si + 1..gi].join("\n"), graph: rest[gi + 1..].join("\n") })
}

pub fn list() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir()).into_iter().flatten().flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().to_string()).filter(|_| e.path().extension().is_some_and(|x| x == "vsnap"))).collect();
    v.sort_by_key(|s| s.to_lowercase());
    v
}

pub fn save(name: &str, s: &Snap) -> Result<(), String> {
    let name = clean_name(name);
    if name.is_empty() {
        return Err("give the snapshot a name".into());
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    std::fs::write(dir().join(format!("{name}.vsnap")), encode(s)).map_err(|e| e.to_string())
}

pub fn load(name: &str) -> Result<Snap, String> {
    let text = std::fs::read_to_string(dir().join(format!("{}.vsnap", clean_name(name)))).map_err(|e| e.to_string())?;
    decode(&text).ok_or_else(|| "this is not a VisuScope snapshot".to_string())
}

pub fn delete(name: &str) {
    let _ = std::fs::remove_file(dir().join(format!("{}.vsnap", clean_name(name))));
}

impl App {
    /// The "Snapshots" menu of the top bar.
    pub fn snapshots_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("Snapshots", |ui| {
            ui.set_min_width(260.0);
            ui.label(RichText::new("Whole session: windows, tool settings, generators, routing").small().weak());
            let names = list();
            if names.is_empty() {
                ui.label(RichText::new("none yet: name the current setup below and save it").small().weak());
            }
            let mut restore: Option<String> = None;
            let mut remove: Option<String> = None;
            for n in &names {
                ui.horizontal(|ui| {
                    if ui.button(n).on_hover_text("Restore this snapshot (the current state is replaced)").clicked() {
                        restore = Some(n.clone());
                    }
                    if self.snap_del.as_deref() == Some(n.as_str()) {
                        if ui.small_button("really delete").clicked() {
                            remove = Some(n.clone());
                        }
                    } else if ui.small_button("x").on_hover_text("Delete this snapshot").clicked() {
                        self.snap_del = Some(n.clone());
                    }
                });
            }
            if let Some(n) = remove {
                delete(&n);
                self.snap_del = None;
            }
            if let Some(n) = restore {
                match load(&n) {
                    Ok(s) => {
                        self.restore_snapshot(&s);
                        self.snap_name = n.clone();
                        self.snap_note = s.note;
                        self.snap_msg = format!("Restored \"{n}\"");
                        ui.close();
                    }
                    Err(e) => self.snap_msg = format!("could not load: {e}"),
                }
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.snap_name).hint_text("snapshot name").desired_width(f32::INFINITY));
            ui.add(egui::TextEdit::multiline(&mut self.snap_note).hint_text("notes: device under test, setup, what to compare ...").desired_rows(3).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                if ui.add_enabled(!clean_name(&self.snap_name).is_empty(), egui::Button::new("Save snapshot")).clicked() {
                    let snap = Snap { note: self.snap_note.clone(), settings: self.settings_string(), graph: crate::graph::bus().graph_text.lock().clone() };
                    self.snap_msg = match save(&self.snap_name, &snap) {
                        Ok(()) => format!("Saved \"{}\"", clean_name(&self.snap_name)),
                        Err(e) => format!("could not save: {e}"),
                    };
                }
                ui.label(RichText::new(&self.snap_msg).small().weak());
            });
        });
    }

    fn restore_snapshot(&mut self, s: &Snap) {
        self.load_settings_from(&s.settings);
        if !s.graph.trim().is_empty() {
            *crate::graph::bus().graph_restore.lock() = Some(s.graph.clone());
        }
        self.save_settings(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_survives_the_file_format_with_notes_in_any_form() {
        let s = Snap { note: "line one\nline 100% two | three".into(), settings: "fft=16384\nwindow=Hann".into(), graph: "VSGRAPH 1\nnode 1|in|0|0|0||1,2".into() };
        let d = decode(&encode(&s)).unwrap();
        assert_eq!(d.note, s.note);
        assert_eq!(d.settings, s.settings);
        assert_eq!(d.graph, s.graph);
        assert!(decode("nonsense").is_none());
        assert!(decode("VSNAP 1\nnote=x\n---graph\n---settings\n").is_none());
        assert!(decode("VSNAP 1").is_none());
    }

    #[test]
    fn names_are_made_safe_and_the_folder_round_trips() {
        assert_eq!(clean_name("  a/b\\c:d*?.txt "), "abcdtxt");
        assert_eq!(clean_name("CON"), "CON 1");
        assert_eq!(clean_name("///"), "");
        let dir = std::env::temp_dir().join(format!("visuscope_snap_{}", std::process::id()));
        // SAFETY: the tests of this module are the only users of the variable
        unsafe { std::env::set_var("VISUSCOPE_SNAP_DIR", &dir) };
        let s = Snap { note: "n".into(), settings: "a=1".into(), graph: "g".into() };
        save("Bench 1", &s).unwrap();
        save("amp (low)", &s).unwrap();
        assert!(save("", &s).is_err());
        assert_eq!(list(), vec!["amp (low)".to_string(), "Bench 1".to_string()]);
        assert_eq!(load("Bench 1").unwrap().settings, "a=1");
        delete("Bench 1");
        assert_eq!(list().len(), 1);
        assert!(load("Bench 1").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
