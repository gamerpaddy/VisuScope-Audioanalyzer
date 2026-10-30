//! CSV logger for the measurements, plus threshold alarms that show up in the status bar.

use super::plot::heading;
use super::*;
use eframe::egui::{self, vec2, Color32, RichText, Sense};
use std::collections::VecDeque;
use std::io::Write;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Clip,
    Silence,
    ThdN,
    High,
    Low,
    Sfdr,
}

struct Rule {
    kind: Kind,
    on: bool,
    thr: f32,
    hold: f32,
    // runtime
    since: Option<Instant>,
    active: bool,
}

impl Rule {
    fn new(kind: Kind, on: bool, thr: f32, hold: f32) -> Self {
        Self { kind, on, thr, hold, since: None, active: false }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Clip => "Clipping (peak above)",
            Kind::Silence => "Silence (RMS below)",
            Kind::ThdN => "THD+N above",
            Kind::High => "Level above (RMS)",
            Kind::Low => "Level below (RMS)",
            Kind::Sfdr => "SFDR below",
        }
    }

    fn unit(&self) -> &'static str {
        match self.kind {
            Kind::ThdN | Kind::Sfdr => " dB",
            _ => " dBFS",
        }
    }

    fn short(&self) -> &'static str {
        match self.kind {
            Kind::Clip => "Clipping",
            Kind::Silence => "Silence",
            Kind::ThdN => "THD+N",
            Kind::High => "Level high",
            Kind::Low => "Level low",
            Kind::Sfdr => "SFDR",
        }
    }
}

struct LogFile {
    w: std::io::BufWriter<std::fs::File>,
    path: std::path::PathBuf,
    rows: u64,
    start: Instant,
    last: Instant,
}

pub struct Logger {
    interval: f32,
    log: Option<LogFile>,
    err: String,
    rules: Vec<Rule>,
    events: VecDeque<(String, String)>,
}

impl Logger {
    pub fn new() -> Self {
        Self {
            interval: 1.0,
            log: None,
            err: String::new(),
            rules: vec![
                Rule::new(Kind::Clip, true, -0.3, 0.0),
                Rule::new(Kind::Silence, false, -80.0, 5.0),
                Rule::new(Kind::ThdN, false, -60.0, 1.0),
                Rule::new(Kind::High, false, -6.0, 1.0),
                Rule::new(Kind::Low, false, -50.0, 2.0),
                Rule::new(Kind::Sfdr, false, 60.0, 1.0),
            ],
            events: VecDeque::new(),
        }
    }

    fn start(&mut self) {
        let (stamp, _) = utc_stamp(now_unix());
        let dir = docs_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("log_{stamp}.csv"));
        match std::fs::File::create(&path) {
            Ok(f) => {
                let mut w = std::io::BufWriter::new(f);
                let _ = writeln!(w, "utc,t_s,freq_hz,fund_dbfs,rms_l_dbfs,rms_r_dbfs,peak_l_dbfs,peak_r_dbfs,thd_db,thdn_db,snr_db,sinad_db,enob,sfdr_db,noise_floor_dbfs,alarms");
                self.log = Some(LogFile { w, path, rows: 0, start: Instant::now(), last: Instant::now() - std::time::Duration::from_secs(3600) });
                self.err.clear();
            }
            Err(e) => self.err = format!("cannot create log: {e}"),
        }
    }

    fn eval(&mut self, cx: &Ctx) {
        let an = cx.an;
        if an.n == 0 {
            return;
        }
        let m = an.metrics[cx.meas_ch.min(1)];
        let peak = an.peak_db[0].max(an.peak_db[1]);
        let rms = an.rms_db[0].max(an.rms_db[1]);
        for r in &mut self.rules {
            let cond = r.on
                && match r.kind {
                    Kind::Clip => peak >= r.thr,
                    Kind::Silence => rms <= r.thr,
                    Kind::High => rms >= r.thr,
                    Kind::Low => rms <= r.thr && rms > -199.0,
                    Kind::ThdN => m.valid && m.thdn_db >= r.thr,
                    Kind::Sfdr => m.valid && m.sfdr_db <= r.thr,
                };
            if cond {
                let t = *r.since.get_or_insert_with(Instant::now);
                if !r.active && t.elapsed().as_secs_f32() >= r.hold {
                    r.active = true;
                    let (_, iso) = utc_stamp(now_unix());
                    self.events.push_front((iso, format!("{} ON", r.short())));
                }
            } else {
                r.since = None;
                if r.active {
                    r.active = false;
                    let (_, iso) = utc_stamp(now_unix());
                    self.events.push_front((iso, format!("{} cleared", r.short())));
                }
            }
        }
        self.events.truncate(200);
    }

    fn active_names(&self) -> Vec<&'static str> {
        self.rules.iter().filter(|r| r.active).map(|r| r.short()).collect()
    }
}

impl Tool for Logger {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        self.eval(cx);
        let alarms = self.active_names().join("+");
        let Some(lf) = &mut self.log else { return };
        if lf.last.elapsed().as_secs_f32() < self.interval || cx.an.n == 0 {
            return;
        }
        lf.last = Instant::now();
        let an = cx.an;
        let m = an.metrics[cx.meas_ch.min(1)];
        let (_, iso) = utc_stamp(now_unix());
        let f = |v: f32| if v.is_finite() { format!("{v:.3}") } else { String::new() };
        let mv = |ok: bool, v: f32| if ok { f(v) } else { String::new() };
        let line = format!(
            "{iso},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{alarms}",
            lf.start.elapsed().as_secs_f32(),
            mv(m.valid, m.fund_hz),
            mv(m.valid, m.fund_dbfs),
            f(an.rms_db[0]),
            f(an.rms_db[1]),
            f(an.peak_db[0]),
            f(an.peak_db[1]),
            mv(m.valid, m.thd_db),
            mv(m.valid, m.thdn_db),
            mv(m.valid, m.snr_db),
            mv(m.valid, m.sinad_db),
            mv(m.valid, m.enob),
            mv(m.valid, m.sfdr_db),
            mv(m.valid, m.noise_floor_dbfs),
        );
        if writeln!(lf.w, "{line}").and_then(|_| lf.w.flush()).is_ok() {
            lf.rows += 1;
        } else {
            self.err = "write failed, logging stopped".into();
            self.log = None;
        }
    }

    fn ui(&mut self, ui: &mut Ui, _cx: &mut Ctx) {
        ui.spacing_mut().item_spacing.y = 6.0;
        heading(ui, "Log to CSV");
        ui.horizontal(|ui| {
            let on = self.log.is_some();
            let b = egui::Button::new(RichText::new(if on { "Stop logging" } else { "Start logging" }).strong()).fill(if on { Color32::from_rgb(40, 110, 70) } else { Color32::from_gray(55) }).min_size(vec2(120.0, 24.0));
            if ui.add(b).clicked() {
                if on {
                    self.log = None;
                } else {
                    self.start();
                }
            }
            ui.add(egui::DragValue::new(&mut self.interval).range(0.1..=3600.0).speed(0.1).prefix("every ").suffix(" s"));
        });
        if let Some(lf) = &self.log {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} rows  {}", lf.rows, lf.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())).small().monospace());
                if ui.small_button("Show folder").clicked() {
                    let _ = std::process::Command::new("explorer").arg(docs_dir().join("logs")).spawn();
                }
            });
        }
        if !self.err.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 120, 100), &self.err);
        }
        ui.separator();
        heading(ui, "Alarms");
        egui::Grid::new("alarm_grid").num_columns(5).spacing([10.0, 5.0]).show(ui, |ui| {
            for r in &mut self.rules {
                let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                let col = if r.active { Color32::from_rgb(255, 60, 60) } else if r.on { Color32::from_rgb(50, 120, 70) } else { Color32::from_gray(50) };
                ui.painter().circle_filled(rect.center(), 5.5, col);
                let (name, unit) = (r.name(), r.unit());
                ui.checkbox(&mut r.on, name);
                ui.add(egui::DragValue::new(&mut r.thr).range(-200.0..=200.0).speed(0.2).suffix(unit));
                ui.add(egui::DragValue::new(&mut r.hold).range(0.0..=3600.0).speed(0.1).prefix("for ").suffix(" s"));
                ui.label("");
                ui.end_row();
            }
        });
        ui.separator();
        heading(ui, "Events");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.events.is_empty() {
                ui.label(RichText::new("none").weak());
            }
            for (t, e) in &self.events {
                ui.label(RichText::new(format!("{t}   {e}")).monospace().small());
            }
        });
    }

    fn status(&self) -> Option<(String, Color32)> {
        let a = self.active_names();
        if !a.is_empty() {
            return Some((format!("ALARM: {}", a.join(", ")), Color32::from_rgb(255, 80, 80)));
        }
        self.log.as_ref().map(|l| (format!("LOG {} rows", l.rows), Color32::from_rgb(140, 200, 160)))
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("interval", self.interval);
        for (i, r) in self.rules.iter().enumerate() {
            o.put(&format!("r{i}"), format!("{},{},{}", r.on, r.thr, r.hold));
        }
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("interval", &mut self.interval);
        self.interval = self.interval.clamp(0.1, 3600.0);
        for (k, r) in self.rules.iter_mut().enumerate() {
            if let Some(s) = i.str(&format!("r{k}")) {
                let v: Vec<&str> = s.split(',').collect();
                if v.len() == 3 {
                    r.on = v[0].parse().unwrap_or(r.on);
                    r.thr = v[1].parse().unwrap_or(r.thr);
                    r.hold = v[2].parse().unwrap_or(r.hold);
                }
            }
        }
    }
}
