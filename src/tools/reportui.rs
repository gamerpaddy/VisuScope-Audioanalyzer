//! Report window: pick what goes into the measurement report (HTML, print to PDF from the browser)
//! and export the last sweep as `.frd`, REW text or an impulse response WAV.

use super::*;
use eframe::egui::{self, RichText};

const SECTIONS: [(&str, &str); 9] = [
    ("Spectrum", "Spectrum"),
    ("Measurements", "Measurements"),
    ("Response", "Response"),
    ("Impulse", "Impulse & room"),
    ("Transfer", "Transfer"),
    ("Distortion", "Distortion"),
    ("SPL", "Sound level"),
    ("Sequence", "Test sequence"),
    ("Traces", "Stored traces"),
];

pub struct ReportTool {
    title: String,
    notes: String,
    include: [bool; 9],
    open_after: bool,
    frd: Option<dialogs::Pending>,
    rew: Option<dialogs::Pending>,
    wav: Option<dialogs::Pending>,
}

impl ReportTool {
    pub fn new() -> Self {
        Self { title: "Audio measurement report".into(), notes: String::new(), include: [true; 9], open_after: true, frd: None, rew: None, wav: None }
    }
}

fn done(p: &mut Option<dialogs::Pending>) -> Option<std::path::PathBuf> {
    let d = p.as_ref()?;
    let res = d.take()?;
    *p = None;
    res
}

impl Tool for ReportTool {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        let note = |r: std::io::Result<()>, p: &std::path::Path| match r {
            Ok(_) => format!("Saved {}", p.display()),
            Err(e) => format!("save failed: {e}"),
        };
        for (kind, path) in [(0, done(&mut self.frd)), (1, done(&mut self.rew)), (2, done(&mut self.wav))] {
            let (Some(path), Some(r)) = (path, &cx.sweep.result) else { continue };
            let n = match kind {
                2 => (|| {
                    let mut w = wavio::WavWriter::create(&path, wavio::Format::Float32, 1, r.sr as u32)?;
                    w.write(&r.ir, &r.ir)?;
                    w.finish()
                })(),
                _ => {
                    let h = &r.h;
                    let top = (r.params.f1 * 1.2).min(r.sr * 0.5);
                    let k1 = ((top / h.df) as usize).min(h.mag_db.len() - 1);
                    let f: Vec<f32> = (1..=k1).map(|k| k as f32 * h.df).collect();
                    let ph: Vec<f32> = h.phase[1..=k1].iter().map(|p| p.to_degrees()).collect();
                    std::fs::write(&path, crate::report::frd_text(&f, &h.mag_db[1..=k1], &ph, kind == 1))
                }
            };
            cx.req.note = Some(note(n, &path));
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        ui.label(RichText::new("Report").strong());
        ui.add(egui::TextEdit::singleline(&mut self.title).desired_width(f32::INFINITY).hint_text("title"));
        ui.add(egui::TextEdit::multiline(&mut self.notes).desired_width(f32::INFINITY).desired_rows(3).hint_text("notes: device under test, setup, operator ..."));
        ui.horizontal_wrapped(|ui| {
            for (i, (_, label)) in SECTIONS.iter().enumerate() {
                ui.checkbox(&mut self.include[i], *label);
            }
        });
        ui.label(RichText::new("Sections without data are left out. The report shows the current state of each window.").small().weak());
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Create report").strong()).min_size(egui::vec2(120.0, 26.0))).clicked() {
                cx.req.report = Some(ReportReq { title: self.title.clone(), notes: self.notes.clone(), include: SECTIONS.iter().zip(self.include).filter(|(_, on)| *on).map(|(s, _)| s.0.to_string()).collect(), open: self.open_after, path: None });
            }
            ui.checkbox(&mut self.open_after, "open in browser");
            if ui.button("Folder").on_hover_text("Reports are saved in Documents\\VisuScope").clicked() {
                open_path(&docs_dir());
            }
        });
        ui.separator();
        ui.label(RichText::new("Export the last sweep").strong());
        let has = cx.sweep.result.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(has && self.frd.is_none(), egui::Button::new("Response .frd")).on_hover_text("frequency, magnitude dB, phase degrees (space separated)").clicked() {
                self.frd = Some(dialogs::save("Export response", "FRD", &["frd"], "response.frd".into()));
            }
            if ui.add_enabled(has && self.rew.is_none(), egui::Button::new("Response .txt")).on_hover_text("same data as comma separated text with a REW style header").clicked() {
                self.rew = Some(dialogs::save("Export response", "Text", &["txt"], "response.txt".into()));
            }
            if ui.add_enabled(has && self.wav.is_none(), egui::Button::new("Impulse .wav")).on_hover_text("impulse response as a 32-bit float mono WAV").clicked() {
                self.wav = Some(dialogs::save("Export impulse response", "WAV", &["wav"], "impulse.wav".into()));
            }
            if !has {
                ui.label(RichText::new("run a sweep in the Response window first").small().weak());
            }
        });
        ui.label(RichText::new("Spectrum snapshots, traces and Response / Impulse CSV exports are in their own windows.").small().weak());
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("open", self.open_after);
        o.put("title", self.title.replace('\n', " "));
        for (i, (id, _)) in SECTIONS.iter().enumerate() {
            o.put(&format!("inc_{id}"), self.include[i]);
        }
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("open", &mut self.open_after);
        if let Some(t) = i.str("title") {
            self.title = t;
        }
        for (k, (id, _)) in SECTIONS.iter().enumerate() {
            i.set(&format!("inc_{id}"), &mut self.include[k]);
        }
    }
}

/// Open a file or folder with the system's default program.
pub fn open_path(p: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}
