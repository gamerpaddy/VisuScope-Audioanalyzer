//! Options dialog: settings that are not needed while measuring, kept out of the side bar and the windows.

use crate::app::*;
use eframe::egui::{self, RichText};

impl App {
    pub fn options_ui(&mut self, ctx: &egui::Context) {
        if !self.opts_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Options").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(360.0).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new("Windows").strong());
            ui.checkbox(&mut self.snap_on, "Snap windows to each other and to the edges").on_hover_text("Ctrl while dragging moves a window freely. Alt while resizing leaves the neighbours alone.");
            ui.checkbox(&mut self.auto_tile, "Re-tile automatically when a window is opened or closed");
            ui.horizontal(|ui| {
                if ui.add_enabled(self.lay_can_undo(), egui::Button::new("Undo layout change")).on_hover_text("Ctrl+Alt+Z").clicked() {
                    self.layout_undo();
                }
                if ui.add_enabled(self.lay_can_redo(), egui::Button::new("Redo layout change")).on_hover_text("Ctrl+Alt+Y").clicked() {
                    self.layout_redo();
                }
            });
            ui.separator();
            ui.label(RichText::new("Performance").strong());
            ui.add(egui::Slider::new(&mut self.fps_cap, 15.0..=241.0).text("UI frame rate cap").custom_formatter(|v, _| if v > 240.0 { "max".into() } else { format!("{v:.0}") }));
            ui.add(egui::Slider::new(&mut self.readout_hz, 0.5..=10.0).text("Readout update rate (Hz)"));
            ui.checkbox(&mut self.readout_hold, "Hold the numeric readouts");
            ui.separator();
            ui.label(RichText::new("Files").strong());
            ui.horizontal(|ui| {
                let dir = crate::tools::dialogs::last_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "none yet (Documents)".into());
                ui.label(RichText::new(format!("Open / save dialogs start in: {dir}")).small());
                if ui.small_button("Forget").clicked() {
                    crate::tools::dialogs::forget_dir();
                }
            });
            ui.separator();
            ui.label(RichText::new(format!("VisuScope {}", crate::VERSION_FULL)).small().weak());
        });
        self.opts_open = open;
    }
}
