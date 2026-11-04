//! The assistant's walk-through: instead of applying a plan at once it asks, step by step, for the input and its channels, the
//! output, a microphone calibration file (or none) and the parameters, shows a summary, and only then sets everything up in that
//! order: devices, calibration, parameters, the routing patch, and last the windows and their layout.

use crate::app::App;
use crate::assistant::{Action, Plan, Run};
use crate::tonegen::{NoiseKind, Wave};
use crate::tools::dialogs::{self, Pending};
use eframe::egui::{self, Color32, RichText, Ui};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Step {
    Input,
    Output,
    Calibration,
    Params,
    Review,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Step::Input => "Input",
            Step::Output => "Output",
            Step::Calibration => "Microphone calibration",
            Step::Params => "Parameters",
            Step::Review => "Review",
        }
    }
}

pub struct Wizard {
    pub plan: Plan,
    pub steps: Vec<Step>,
    pub at: usize,
    pub in_dev: usize,
    pub in_l: usize,
    pub in_r: usize,
    /// index in the list of output devices (the first one is the default output)
    pub out_dev: usize,
    pub cal: [Option<PathBuf>; 2],
    pub cal_dlg: Option<(usize, Pending)>,
    /// sweep: lowest and highest frequency, length (s), level (dBFS), averages
    pub sweep: Option<(f32, f32, f32, f32, usize)>,
    /// generator tone: wave, noise kind, frequency, level
    pub tone: Option<(Wave, Option<NoiseKind>, f32, f32)>,
    pub dist_mode: Option<usize>,
    pub use_preset: bool,
    pub start: bool,
}

fn needs_output(plan: &Plan) -> bool {
    plan.runs.iter().any(|r| matches!(r, Run::Sweep | Run::Stepped(_))) || plan.actions.iter().any(|a| matches!(a, Action::Tone { .. } | Action::Monitor)) || plan.preset.is_some_and(|p| p.uses_output())
}

fn wants_calibration(plan: &Plan) -> bool {
    plan.actions.iter().any(|a| matches!(a, Action::Windows(w) if w.iter().any(|n| matches!(*n, "Spectrum" | "Response" | "Impulse" | "SPL" | "Octave" | "Transfer" | "Measure"))))
}

impl App {
    /// Start the walk-through for a plan.
    pub fn wizard_start(&mut self, plan: Plan) {
        let mut steps = vec![Step::Input];
        if needs_output(&plan) {
            steps.push(Step::Output);
        }
        if wants_calibration(&plan) {
            steps.push(Step::Calibration);
        }
        let sweep_def = self.sweep.p.clone();
        let sweep = plan.actions.iter().find_map(|a| match a {
            Action::Sweep { f0, f1, secs, level, averages } => Some((f0.unwrap_or(sweep_def.f0), f1.unwrap_or(sweep_def.f1), secs.unwrap_or(sweep_def.secs), level.unwrap_or(sweep_def.amp_db), averages.unwrap_or(sweep_def.averages))),
            _ => None,
        });
        let sweep = sweep.or_else(|| plan.runs.contains(&Run::Sweep).then_some((sweep_def.f0, sweep_def.f1, sweep_def.secs, sweep_def.amp_db, sweep_def.averages)));
        let tone = plan.actions.iter().find_map(|a| match a {
            Action::Tone { wave, noise, freq, level } => Some((*wave, *noise, freq.unwrap_or(self.tg.freq), level.unwrap_or(self.tg.level_db[0]))),
            _ => None,
        });
        let dist_mode = plan.actions.iter().find_map(|a| if let Action::DistMode(m) = a { Some(*m) } else { None });
        if sweep.is_some() || tone.is_some() || dist_mode.is_some() {
            steps.push(Step::Params);
        }
        steps.push(Step::Review);
        let (in_dev, in_ch) = plan.actions.iter().find_map(|a| if let Action::Input { dev, ch, .. } = a { Some((*dev, *ch)) } else { None }).unwrap_or((None, None));
        let (l, r) = match in_ch {
            Some(c) => (c, c),
            None => (self.ch_a, self.ch_b),
        };
        self.assist.wiz = Some(Wizard {
            use_preset: self.graph_on && plan.preset.is_some(),
            start: !plan.runs.is_empty(),
            plan,
            steps,
            at: 0,
            in_dev: in_dev.unwrap_or(self.sel_dev),
            in_l: l + 1,
            in_r: r + 1,
            out_dev: self.gen_sel,
            cal: [None, None],
            cal_dlg: None,
            sweep,
            tone,
            dist_mode,
        });
    }

    /// Every frame (from the assistant's tick): a chosen calibration file arrives.
    pub fn wizard_poll(&mut self) {
        if let Some(w) = self.assist.wiz.as_mut() {
            if let Some((slot, d)) = &w.cal_dlg {
                if let Some(res) = d.take() {
                    let slot = *slot;
                    w.cal_dlg = None;
                    if let Some(p) = res {
                        w.cal[slot] = Some(p);
                    }
                }
            }
        }
    }

    /// The plan the walk-through ends in: the chosen devices and values, in the order they are applied.
    fn wizard_plan(&self, w: &Wizard) -> Plan {
        let mut p = Plan { title: w.plan.title.clone(), runs: w.plan.runs.clone(), notes: Vec::new(), actions: Vec::new(), preset: w.plan.preset };
        // 1. devices
        let label = self.devices.get(w.in_dev).map(|d| d.label.clone()).unwrap_or_default();
        p.actions.push(Action::Pair { dev: w.in_dev, label, l: w.in_l.max(1) - 1, r: w.in_r.max(1) - 1 });
        if w.steps.contains(&Step::Output) {
            p.actions.push(Action::OutputDev(w.out_dev));
        }
        // 2. calibration
        for (slot, f) in w.cal.iter().enumerate() {
            if let Some(f) = f {
                p.actions.push(Action::Cal { slot, path: f.to_string_lossy().to_string() });
            }
        }
        // 3. parameters
        if let Some((f0, f1, secs, level, averages)) = w.sweep {
            p.actions.push(Action::Sweep { f0: Some(f0), f1: Some(f1), secs: Some(secs), level: Some(level), averages: Some(averages) });
        }
        if let Some(m) = w.dist_mode {
            p.actions.push(Action::DistMode(m));
        }
        if let Some((wave, noise, freq, level)) = w.tone {
            p.actions.push(Action::Tone { wave, noise, freq: Some(freq), level: Some(level) });
        }
        if w.plan.actions.contains(&Action::Monitor) {
            p.actions.push(Action::Monitor);
        }
        // 4. the routing patch, 5. the windows and their layout
        if w.use_preset {
            if let Some(pr) = w.plan.preset {
                p.actions.push(Action::Preset(pr));
            }
        }
        if let Some(a) = w.plan.actions.iter().find(|a| matches!(a, Action::Windows(_))) {
            p.actions.push(a.clone());
        }
        p
    }

    /// The walk-through inside the assistant's window.
    pub fn wizard_ui(&mut self, ui: &mut Ui) {
        let Some(mut w) = self.assist.wiz.take() else { return };
        let step = w.steps[w.at.min(w.steps.len() - 1)];
        ui.label(RichText::new(format!("Step {} of {}: {}", w.at + 1, w.steps.len(), step.title())).strong());
        ui.label(RichText::new(&w.plan.title).small().weak());
        ui.separator();
        let (mut back, mut next, mut cancel, mut finish) = (false, false, false, false);
        match step {
            Step::Input => {
                ui.label("Which input?");
                let cur = self.devices.get(w.in_dev).map(|d| d.label.clone()).unwrap_or_else(|| "no input device".into());
                egui::ComboBox::from_id_salt("wz_in").width(ui.available_width() - 8.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    for (i, d) in self.devices.iter().enumerate() {
                        ui.selectable_value(&mut w.in_dev, i, &d.label);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Left / A channel");
                    ui.add(egui::DragValue::new(&mut w.in_l).range(1..=32));
                    ui.label("Right / B channel");
                    ui.add(egui::DragValue::new(&mut w.in_r).range(1..=32));
                    if crate::mapping::small_btn(ui, "same for both").on_hover_text("One microphone: use its channel for left and right").clicked() {
                        w.in_r = w.in_l;
                    }
                });
                ui.label(RichText::new("Channels are numbered from 1. A one-microphone measurement uses the same channel for both.").small().weak());
            }
            Step::Output => {
                ui.label("Which output? (it plays the sweep or the tone, and the monitor)");
                let cur = self.gen_devs.get(w.out_dev).map(|d| d.label.clone()).unwrap_or_else(|| "default output".into());
                egui::ComboBox::from_id_salt("wz_out").width(ui.available_width() - 8.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    for (i, d) in self.gen_devs.iter().enumerate() {
                        ui.selectable_value(&mut w.out_dev, i, &d.label);
                    }
                });
                if w.plan.runs.contains(&Run::Sweep) {
                    ui.label(RichText::new("Connect this output to the device under test, and that device to the input (or choose a [Loopback] input).").small().color(Color32::from_rgb(255, 190, 100)));
                }
            }
            Step::Calibration => {
                ui.label("A microphone calibration file corrects the spectrum views and measurements. Skip it if you have none or do not need one.");
                for (slot, name) in ["Calibration file (0 degrees)", "Calibration file (90 degrees, optional)"].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        let shown = w.cal[slot].as_ref().map(|p| p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).or_else(|| Some(self.mic_paths[slot].clone()).filter(|s| !s.is_empty()).map(|s| format!("{} (already loaded)", std::path::Path::new(&s).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(s))));
                        if ui.add_enabled(w.cal_dlg.is_none(), egui::Button::new(name)).clicked() {
                            w.cal_dlg = Some((slot, dialogs::open("Microphone calibration file", "Calibration", &["txt", "cal", "csv", "frd"])));
                        }
                        ui.label(RichText::new(shown.unwrap_or_else(|| "none".into())).small().weak());
                        if w.cal[slot].is_some() && crate::mapping::small_btn(ui, "x").clicked() {
                            w.cal[slot] = None;
                        }
                    });
                }
            }
            Step::Params => {
                if let Some((f0, f1, secs, level, avg)) = w.sweep.as_mut() {
                    ui.label(RichText::new("Sweep").strong());
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(f0).range(5.0..=2000.0).speed(1.0).suffix(" Hz"));
                        ui.label("to");
                        ui.add(egui::DragValue::new(f1).range(1000.0..=96000.0).speed(10.0).suffix(" Hz"));
                    });
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(secs).range(0.5..=12.0).speed(0.1).suffix(" s long"));
                        ui.add(egui::DragValue::new(level).range(-60.0..=0.0).speed(0.2).suffix(" dBFS"));
                        ui.add(egui::DragValue::new(avg).range(1..=64).suffix(" averages"));
                    });
                }
                if let Some(m) = w.dist_mode.as_mut() {
                    ui.label(RichText::new("Distortion measurement").strong());
                    egui::ComboBox::from_id_salt("wz_dist").selected_text(["THD vs frequency", "THD vs level", "IMD vs level", "Swept harmonics (from the sweep)"][(*m).min(3)]).show_ui(ui, |ui| {
                        for (i, n) in ["THD vs frequency", "THD vs level", "IMD vs level", "Swept harmonics (from the sweep)"].iter().enumerate() {
                            ui.selectable_value(m, i, *n);
                        }
                    });
                }
                if let Some((wave, noise, freq, level)) = w.tone.as_mut() {
                    ui.label(RichText::new("Generator").strong());
                    ui.horizontal(|ui| {
                        let name = noise.map(|n| format!("{n:?} noise")).unwrap_or_else(|| wave.name().to_string());
                        egui::ComboBox::from_id_salt("wz_wave").width(130.0).selected_text(name).show_ui(ui, |ui| {
                            for wv in [Wave::Sine, Wave::Square, Wave::Triangle, Wave::Saw] {
                                if ui.selectable_label(noise.is_none() && *wave == wv, wv.name()).clicked() {
                                    *wave = wv;
                                    *noise = None;
                                }
                            }
                            for (n, label) in [(NoiseKind::White, "White noise"), (NoiseKind::Pink, "Pink noise"), (NoiseKind::Brown, "Brown noise")] {
                                if ui.selectable_label(*noise == Some(n), label).clicked() {
                                    *wave = Wave::Noise;
                                    *noise = Some(n);
                                }
                            }
                        });
                        ui.add_enabled(*wave != Wave::Noise, egui::DragValue::new(freq).range(1.0..=96000.0).speed(1.0).suffix(" Hz"));
                        ui.add(egui::DragValue::new(level).range(-90.0..=0.0).speed(0.2).suffix(" dBFS"));
                    });
                }
            }
            Step::Review => {
                let p = self.wizard_plan(&w);
                for a in &p.actions {
                    ui.label(RichText::new(format!("• {}", a.describe())).small());
                }
                if let Some(pr) = w.plan.preset {
                    ui.separator();
                    ui.checkbox(&mut w.use_preset, format!("Put this routing patch into the node editor: {}", pr.name())).on_hover_text("Switches the routing graph on and replaces the current patch (Ctrl+Z in the node editor brings the old one back)");
                }
                if !w.plan.runs.is_empty() {
                    ui.checkbox(&mut w.start, "Start the measurement when everything is set up");
                }
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(w.at > 0, egui::Button::new("Back")).clicked() {
                back = true;
            }
            if step == Step::Review {
                if crate::mapping::btn(ui, RichText::new("Set up").strong()).clicked() {
                    finish = true;
                }
            } else {
                if step == Step::Calibration && w.cal.iter().all(|c| c.is_none()) && crate::mapping::btn(ui, "Skip").clicked() {
                    next = true;
                }
                if crate::mapping::btn(ui, RichText::new("Next").strong()).clicked() {
                    next = true;
                }
            }
            if crate::mapping::btn(ui, "Cancel").clicked() {
                cancel = true;
            }
        });
        if cancel {
            return;
        }
        if back {
            w.at = w.at.saturating_sub(1);
        }
        if next {
            w.at = (w.at + 1).min(w.steps.len() - 1);
        }
        if finish {
            let plan = self.wizard_plan(&w);
            let start = w.start;
            self.assist.plan = Some(plan.clone());
            self.assist_apply(&plan, start);
            return;
        }
        self.assist.wiz = Some(w);
    }
}
