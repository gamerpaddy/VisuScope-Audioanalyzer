//! Pluggable analysis tools.
//!
//! Every tool owns its state, draws exactly one window and may paint onto the spectrum. The app only
//! knows the `Tool` trait: adding a tool means writing a module, adding it to `all()` and `INFO`.

use crate::app::Snapshot;
use crate::shared::*;
use eframe::egui::{Color32, Painter, Rect, Ui};
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;

pub mod decode;
pub mod distortion;
pub mod daw;
pub mod dialogs;
pub mod eqtool;
pub mod impulse;
pub mod loudness;
pub mod logger;
pub mod monitor;
pub mod octave;
pub mod peaks;
pub mod profiler;
pub mod plot;
pub mod recorder;
pub mod reportui;
pub mod reference;
pub mod response;
pub mod room;
pub mod stats;
pub mod spl;
pub mod sequence;
pub mod sweep;
pub mod traces;
pub mod tap;
pub mod transfer;
pub mod wavio;

pub const TOOL_COUNT: usize = 18;
/// Index of the routing graph's window in `INFO` (it only exists while the graph is switched on).
pub const ROUTING: usize = 16;

pub struct ToolInfo {
    pub short: &'static str,
    pub title: &'static str,
    /// tiling priority: >= 1.5 is laid out as a visual window, less goes to the side column
    pub weight: f32,
    pub tip: &'static str,
}

/// Same order as `all()`.
pub const INFO: [ToolInfo; TOOL_COUNT] = [
    ToolInfo { short: "Recorder", title: "Recorder & file player", weight: 0.9, tip: "Record the input to WAV, or play a WAV file through the analysers" },
    ToolInfo { short: "Octave", title: "Octave analyser", weight: 2.4, tip: "1/1 to 1/24 octave band levels with A/C weighting and a target curve" },
    ToolInfo { short: "Response", title: "Frequency response", weight: 2.4, tip: "Sweep measurement: magnitude, phase, group delay, coherence" },
    ToolInfo { short: "Impulse", title: "Impulse response & RT60", weight: 2.0, tip: "Impulse response, decay curve, RT60 / EDT, clarity" },
    ToolInfo { short: "Peaks", title: "Peaks & tuner", weight: 1.0, tip: "Strongest spectral peaks with note names, plus a tuner" },
    ToolInfo { short: "Loudness", title: "Loudness (EBU R128)", weight: 1.2, tip: "LUFS momentary / short-term / integrated, LRA, true peak, PPM and VU" },
    ToolInfo { short: "Stats", title: "Signal statistics", weight: 1.6, tip: "Sample histogram, clipping, bit-depth use and long-term trends" },
    ToolInfo { short: "Logger", title: "Logger & alarms", weight: 0.8, tip: "Log measurements to CSV and raise alarms on thresholds" },
    ToolInfo { short: "Reference", title: "Reference & limit masks", weight: 1.0, tip: "Pass/fail masks, reference curves and an A/B difference trace" },
    ToolInfo { short: "Transfer", title: "Live transfer function", weight: 2.4, tip: "Magnitude, phase and coherence between a reference and a measured channel while music or noise plays" },
    ToolInfo { short: "SPL", title: "Sound level meter", weight: 1.6, tip: "dB SPL with A/C/Z and Fast/Slow/Impulse weighting, Leq, Lmax, peak, L10/L50/L90" },
    ToolInfo { short: "Distortion", title: "Distortion sweeps", weight: 2.4, tip: "THD vs frequency and level, IMD vs level and swept harmonic analysis" },
    ToolInfo { short: "Sequence", title: "Test sequence", weight: 1.6, tip: "Scripted tests: drive the generator, measure, sweep and check against limits (pass/fail)" },
    ToolInfo { short: "Report", title: "Report & export", weight: 1.2, tip: "Create an HTML / PDF measurement report and export responses (FRD, REW text, impulse WAV)" },
    ToolInfo { short: "EQ", title: "EQ design", weight: 2.0, tip: "Fit parametric filters to a measurement, preview, export Equalizer APO / biquads / FIR" },
    ToolInfo { short: "Timeline", title: "Timeline (multitrack)", weight: 4.0, tip: "Multitrack recorder and editor: record several sources and channels at once, load, edit and analyse tracks" },
    ToolInfo { short: "Routing", title: "Routing (node editor)", weight: 8.0, tip: "Connect every input and output like a patch bay: split signals to the analysers, the timeline or output devices, with effects in between" },
    ToolInfo { short: "Profiler", title: "Profiler (latency & interrupts)", weight: 1.5, tip: "System latency, kernel DPC / interrupt time and audio callback timing; shows what caused a crackle" },
];

/// How the Tools menu groups the tools (indices into `INFO`).
pub const GROUPS: [(&str, &[usize]); 4] = [
    ("Measure", &[2, 3, 11, 10, 5]),
    ("Analyse", &[1, 4, 6, 9, 8, 17]),
    ("Test & design", &[12, 14]),
    ("Record & report", &[15, 16, 0, 7, 13]),
];

pub fn all() -> Vec<Box<dyn Tool>> {
    let v: Vec<Box<dyn Tool>> = vec![
        Box::new(recorder::Recorder::new()),
        Box::new(octave::Octave::new()),
        Box::new(response::Response::new()),
        Box::new(impulse::Impulse::new()),
        Box::new(peaks::Peaks::new()),
        Box::new(loudness::Loudness::new()),
        Box::new(stats::Stats::new()),
        Box::new(logger::Logger::new()),
        Box::new(reference::Reference::new()),
        Box::new(transfer::LiveTf::new()),
        Box::new(spl::Spl::new()),
        Box::new(distortion::Distortion::new()),
        Box::new(sequence::Sequence::new()),
        Box::new(reportui::ReportTool::new()),
        Box::new(eqtool::EqTool::new()),
        Box::new(daw::Daw::new()),
        Box::new(crate::graph::editor::Routing::new()),
        Box::new(profiler::Profiler::new()),
    ];
    debug_assert_eq!(v.len(), TOOL_COUNT);
    v
}

#[derive(Default, PartialEq, Clone, Copy)]
pub enum AudioReq {
    #[default]
    None,
    /// stop live capture (a file is about to feed the analysers)
    Stop,
    /// (re)start live capture
    Restart,
}

/// Change of the generator settings requested by a tool (`None` = leave as is).
#[derive(Default, Clone)]
pub struct GenCmd {
    pub wave: Option<crate::tonegen::Wave>,
    pub freq: Option<f32>,
    /// both channels
    pub level_db: Option<f32>,
    pub on: Option<bool>,
}

/// A measurement report to create.
pub struct ReportReq {
    pub title: String,
    pub notes: String,
    pub include: Vec<String>,
    pub open: bool,
    /// file to write instead of a time-stamped one in Documents\VisuScope
    pub path: Option<std::path::PathBuf>,
}

/// What tools may ask the app to do.
#[derive(Default)]
pub struct Requests {
    pub report: Option<ReportReq>,
    /// quit the program with this exit code (headless runs)
    pub exit: Option<i32>,
    pub gen_cmd: Option<GenCmd>,
    pub start_gen: bool,
    pub audio: AudioReq,
    pub note: Option<String>,
    /// audio files to open in the timeline (also opens its window)
    pub send_to_daw: Vec<std::path::PathBuf>,
}

/// Read-only view of the app state plus the few things tools may change.
pub struct Ctx<'a> {
    pub sh: &'a Arc<Shared>,
    pub an: &'a Analysis,
    pub sr: f32,
    pub meas_ch: usize,
    pub unit_off: [f32; 2],
    pub unit_name: &'static str,
    pub snaps: &'a [Snapshot],
    pub gen_running: bool,
    /// current generator settings
    pub tg: &'a crate::tonegen::GenParams,
    /// output devices (default first), for tools that play sound
    pub out_devs: &'a [crate::audio::DevInfo],
    /// the routing graph (node editor) is on: inputs and outputs are set there
    pub graph_on: bool,
    /// all generators (tab 1 is the one the measurement tools use)
    pub gens: &'a Arc<parking_lot::Mutex<Vec<crate::tonegen::GenParams>>>,
    /// id of the input device chosen for the fixed input path (the graph starts from it)
    pub default_in: String,
    pub sweep: &'a mut sweep::Sweep,
    pub lib: &'a mut traces::Library,
    pub req: &'a mut Requests,
}

/// Painter and axis mapping of the spectrum window, handed to tools that draw onto it.
pub struct SpecOverlay<'a> {
    pub p: &'a Painter,
    pub plot: Rect,
    pub x_of_f: &'a dyn Fn(f32) -> f32,
    pub y_of_db: &'a dyn Fn(f32) -> f32,
}

pub trait Tool {
    /// Called every frame, also while the window is closed (`visible == false`).
    fn update(&mut self, _cx: &mut Ctx, _visible: bool) {}
    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx);
    fn overlay(&self, _ov: &SpecOverlay, _cx: &Ctx) {}
    /// Short status-bar message (recording, alarms ...).
    fn status(&self) -> Option<(String, Color32)> {
        None
    }
    /// What this tool can contribute to a measurement report (empty when it has nothing yet).
    fn report(&self, _cx: &Ctx) -> Vec<crate::report::Section> {
        Vec::new()
    }
    /// Text command from the application (command line runs).
    fn command(&mut self, _c: &str) {}
    fn save(&self, _o: &mut SettingsOut) {}
    fn load(&mut self, _i: &SettingsIn) {}
}

// ------------------------------------------------------------------ settings

pub struct SettingsOut<'a> {
    pub s: &'a mut String,
    pub pre: String,
}

impl SettingsOut<'_> {
    pub fn put(&mut self, k: &str, v: impl Display) {
        use std::fmt::Write;
        let _ = writeln!(self.s, "{}{k}={v}", self.pre);
    }
}

pub struct SettingsIn<'a> {
    pub m: &'a HashMap<&'a str, &'a str>,
    pub pre: String,
}

impl SettingsIn<'_> {
    pub fn get<T: FromStr>(&self, k: &str) -> Option<T> {
        self.m.get(format!("{}{k}", self.pre).as_str()).and_then(|v| v.trim().parse().ok())
    }
    pub fn str(&self, k: &str) -> Option<String> {
        self.m.get(format!("{}{k}", self.pre).as_str()).map(|v| v.to_string())
    }
    /// Overwrite `dst` when the key exists and parses.
    pub fn set<T: FromStr>(&self, k: &str, dst: &mut T) {
        if let Some(v) = self.get(k) {
            *dst = v;
        }
    }
}

// ------------------------------------------------------------------ small shared helpers

pub fn fmt_time(secs: f32) -> String {
    let s = secs.max(0.0);
    let (m, r) = ((s / 60.0).floor() as u32, s % 60.0);
    if m >= 60 { format!("{}:{:02}:{:04.1}", m / 60, m % 60, r) } else { format!("{m}:{r:04.1}") }
}

/// Spectrum value (dB) at frequency `f`, linearly interpolated between bins.
pub fn spec_at(an: &Analysis, data: &[f32], f: f32) -> Option<f32> {
    if an.n == 0 || data.len() < 2 || f <= 0.0 {
        return None;
    }
    let b = f / (an.sr / an.n as f32);
    if b < 0.0 || b > (data.len() - 1) as f32 {
        return None;
    }
    let k = b.floor() as usize;
    let fr = b - k as f32;
    let k1 = (k + 1).min(data.len() - 1);
    Some(data[k] * (1.0 - fr) + data[k1] * fr)
}

pub fn note_of(f: f32, a4: f32) -> (String, f32) {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let semis = 69.0 + 12.0 * (f.max(1e-3) / a4).log2();
    let nearest = semis.round();
    let cents = (semis - nearest) * 100.0;
    let n = nearest as i32;
    (format!("{}{}", N[n.rem_euclid(12) as usize], n.div_euclid(12) - 1), cents)
}

pub fn docs_dir() -> std::path::PathBuf {
    let base = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    let d = base.join("Documents").join("VisuScope");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// `YYYYMMDD_HHMMSS` and ISO-8601 timestamps (UTC) without a date/time dependency.
pub fn utc_stamp(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86400) as i64;
    let rem = unix_secs % 86400;
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (format!("{y:04}{m:02}{d:02}_{h:02}{mi:02}{s:02}"), format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"))
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_groups_cover_every_tool_once() {
        assert_eq!(INFO.len(), TOOL_COUNT);
        assert_eq!(all().len(), TOOL_COUNT);
        let mut seen = vec![0; TOOL_COUNT];
        for (_, g) in GROUPS.iter() {
            for &k in g.iter() {
                seen[k] += 1;
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
    }

    #[test]
    fn notes_and_dates() {
        let (n, c) = note_of(440.0, 440.0);
        assert_eq!(n, "A4");
        assert!(c.abs() < 0.01);
        let (n, c) = note_of(261.63, 440.0);
        assert_eq!(n, "C4");
        assert!(c.abs() < 1.0);
        assert_eq!(utc_stamp(0).1, "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_700_000_000).1, "2023-11-14T22:13:20Z");
    }
}
