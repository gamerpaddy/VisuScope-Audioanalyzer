//! Automated test sequences: a small line-based script drives the generator, measures, runs sweeps
//! and checks the results against limits. Also runs headless-style from the command line:
//! `visuscope --run test.vss --report result.txt` (exit code 0 = pass, 1 = fail, 2 = error).
//!
//! ```text
//! tone 1000 -20            generator: sine, Hz, dBFS
//! imd smpte -10            generator: smpte / ccif twin tone
//! off                      generator off
//! wait 1.5                 seconds
//! set avg 1.0              measurement time in seconds (default 1)
//! measure thdn < -80       level freq thd thdn snr sinad sfdr enob noise rms peak imd
//! measure level in -20.5..-19.5
//! sweep                    run the frequency response sweep
//! response 100..10000 -3..3 [abs|norm 1000]     magnitude band over a frequency range
//! rt60 < 0.6               T30 (or T20) of the last sweep
//! report C:\\tests\\r.html    write an HTML report of the current results
//! ```

use super::distortion::imd;
use super::*;
use crate::tonegen::Wave;
use eframe::egui::{self, Color32, RichText};
use std::time::Instant;

const EXAMPLE: &str = "# Loop the generator output into the input first.\nset avg 1.0\ntone 1000 -20\nmeasure level in -20.5..-19.5\nmeasure thdn < -70\nmeasure snr > 70\nmeasure freq in 990..1010\ntone 100 -20\nmeasure thd < -70\nsweep\nresponse 100..10000 -3..3\noff\n";
const METRICS: [&str; 12] = ["level", "freq", "thd", "thdn", "snr", "sinad", "sfdr", "enob", "noise", "rms", "peak", "imd"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cond {
    Lt(f32),
    Gt(f32),
    In(f32, f32),
}

impl Cond {
    fn ok(self, v: f32) -> bool {
        v.is_finite()
            && match self {
                Cond::Lt(x) => v < x,
                Cond::Gt(x) => v > x,
                Cond::In(a, b) => v >= a && v <= b,
            }
    }

    fn text(self) -> String {
        match self {
            Cond::Lt(x) => format!("< {x}"),
            Cond::Gt(x) => format!("> {x}"),
            Cond::In(a, b) => format!("in {a}..{b}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Tone(f32, f32),
    Imd(bool, f32),
    Off,
    Wait(f32),
    Avg(f32),
    Measure(usize, Cond),
    Sweep,
    Response { f: (f32, f32), db: (f32, f32), norm: Option<f32> },
    Rt60(Cond),
    Report(String),
}

#[derive(Clone, Debug)]
struct Step {
    line: usize,
    text: String,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Pass,
    Fail,
    Info,
    Error,
}

#[derive(Clone)]
struct Outcome {
    line: usize,
    text: String,
    value: String,
    status: Status,
}

fn num(s: &str) -> Option<f32> {
    s.trim().trim_end_matches(|c: char| c.is_alphabetic() || c == '%').parse().ok()
}

fn range(s: &str) -> Option<(f32, f32)> {
    let (a, b) = s.split_once("..")?;
    Some((num(a)?, num(b)?))
}

fn cond(w: &[&str]) -> Result<Cond, String> {
    match w {
        ["<", v] => num(v).map(Cond::Lt).ok_or_else(|| "bad number".into()),
        [">", v] => num(v).map(Cond::Gt).ok_or_else(|| "bad number".into()),
        ["in", r] | [r] => range(r).map(|(a, b)| Cond::In(a, b)).ok_or_else(|| "expected a..b".into()),
        _ => Err("expected < value, > value or in a..b".into()),
    }
}

fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        let err = |m: &str| format!("line {}: {m}", i + 1);
        let kind = match w[0].to_lowercase().as_str() {
            "tone" if w.len() == 3 => Kind::Tone(num(w[1]).ok_or_else(|| err("bad frequency"))?, num(w[2]).ok_or_else(|| err("bad level"))?),
            "imd" if w.len() == 3 && matches!(w[1], "smpte" | "ccif") => Kind::Imd(w[1] == "smpte", num(w[2]).ok_or_else(|| err("bad level"))?),
            "off" => Kind::Off,
            "wait" if w.len() == 2 => Kind::Wait(num(w[1]).ok_or_else(|| err("bad time"))?.clamp(0.0, 600.0)),
            "set" if w.len() == 3 && w[1] == "avg" => Kind::Avg(num(w[2]).ok_or_else(|| err("bad time"))?.clamp(0.2, 60.0)),
            "measure" if w.len() >= 3 => {
                let m = METRICS.iter().position(|m| *m == w[1].to_lowercase()).ok_or_else(|| err(&format!("unknown metric {} (use {})", w[1], METRICS.join(" "))))?;
                Kind::Measure(m, cond(&w[2..]).map_err(|e| err(&e))?)
            }
            "sweep" => Kind::Sweep,
            "response" if w.len() >= 3 => {
                let norm = match w.get(3).map(|s| s.to_lowercase()).as_deref() {
                    None | Some("norm") => Some(w.get(4).and_then(|v| num(v)).unwrap_or(1000.0)),
                    Some("abs") => None,
                    _ => return Err(err("expected abs or norm <Hz>")),
                };
                Kind::Response { f: range(w[1]).ok_or_else(|| err("expected frequency range a..b"))?, db: range(w[2]).ok_or_else(|| err("expected dB range a..b"))?, norm }
            }
            "report" if w.len() >= 2 => Kind::Report(line[w[0].len()..].trim().to_string()),
            "rt60" if w.len() >= 2 => Kind::Rt60(cond(&w[1..]).map_err(|e| err(&e))?),
            other => return Err(err(&format!("unknown or incomplete command '{other}'"))),
        };
        out.push(Step { line: i + 1, text: line.to_string(), kind });
    }
    if out.is_empty() {
        return Err("the script has no steps".into());
    }
    Ok(out)
}

#[derive(Default)]
struct Acc {
    n: u32,
    sum: f64,
    max: f32,
}

enum Run {
    Idle,
    Next,
    Wait(Instant, f32),
    Measure(Instant, Acc),
    Sweeping(Instant),
}

struct Saved {
    wave: Wave,
    freq: f32,
    level: f32,
    on: bool,
}

pub struct Sequence {
    text: String,
    saved_text: String,
    steps: Vec<Step>,
    idx: usize,
    run: Run,
    results: Vec<Outcome>,
    avg_s: f32,
    t_change: Instant,
    cur_freq: Option<f32>,
    cur_smpte: Option<bool>,
    saved: Option<Saved>,
    last_seq: u64,
    msg: String,
    load: Option<dialogs::Pending>,
    save: Option<dialogs::Pending>,
    report: Option<String>,
    headless: bool,
    pending: bool,
    started: Option<Instant>,
}

fn script_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("sequence.vss")
}

impl Sequence {
    pub fn new() -> Self {
        let text = std::fs::read_to_string(script_path()).unwrap_or_else(|_| EXAMPLE.to_string());
        Self {
            saved_text: text.clone(),
            text,
            steps: Vec::new(),
            idx: 0,
            run: Run::Idle,
            results: Vec::new(),
            avg_s: 1.0,
            t_change: Instant::now(),
            cur_freq: None,
            cur_smpte: None,
            saved: None,
            last_seq: 0,
            msg: String::new(),
            load: None,
            save: None,
            report: None,
            headless: false,
            pending: false,
            started: None,
        }
    }

    fn running(&self) -> bool {
        !matches!(self.run, Run::Idle)
    }

    fn start(&mut self, cx: &mut Ctx) {
        match parse(&self.text) {
            Err(e) => {
                self.msg = e;
                self.results.clear();
                self.results.push(Outcome { line: 0, text: "script".into(), value: self.msg.clone(), status: Status::Error });
            }
            Ok(steps) => {
                if cx.sr <= 0.0 {
                    self.msg = "No input running".into();
                    self.results = vec![Outcome { line: 0, text: "input".into(), value: self.msg.clone(), status: Status::Error }];
                    return;
                }
                self.steps = steps;
                self.idx = 0;
                self.results.clear();
                self.msg.clear();
                self.avg_s = 1.0;
                self.cur_freq = None;
                self.cur_smpte = None;
                self.saved = Some(Saved { wave: cx.tg.wave, freq: cx.tg.freq, level: cx.tg.level_db[0], on: cx.tg.on });
                self.t_change = Instant::now();
                self.started = Some(Instant::now());
                self.run = Run::Next;
            }
        }
    }

    fn finish(&mut self, cx: &mut Ctx, aborted: bool) {
        if let Some(s) = self.saved.take() {
            cx.req.gen_cmd = Some(GenCmd { wave: Some(s.wave), freq: Some(s.freq), level_db: Some(s.level), on: Some(s.on) });
        }
        if aborted {
            cx.sweep.cancel();
        }
        self.run = Run::Idle;
        let verdict = self.verdict();
        self.msg = if aborted { "stopped".into() } else { verdict.to_string() };
        if let Some(rep) = self.report.take() {
            let _ = std::fs::write(&rep, self.report_text(cx));
        }
        if self.headless {
            cx.req.exit = Some(match verdict {
                "PASS" => 0,
                "FAIL" => 1,
                _ => 2,
            });
        }
    }

    fn verdict(&self) -> &'static str {
        if self.results.iter().any(|r| r.status == Status::Error) {
            "ERROR"
        } else if self.results.iter().any(|r| r.status == Status::Fail) {
            "FAIL"
        } else {
            "PASS"
        }
    }

    fn report_text(&self, cx: &Ctx) -> String {
        let (_, iso) = utc_stamp(now_unix());
        let mut s = format!("VisuScope test sequence\nTime: {iso}\nInput: {}\nResult: {}\n\n", cx.sh.status.lock(), self.verdict());
        for r in &self.results {
            let st = match r.status {
                Status::Pass => "PASS ",
                Status::Fail => "FAIL ",
                Status::Info => "     ",
                Status::Error => "ERROR",
            };
            s += &format!("{st}  line {:>3}  {:<34}  {}\n", r.line, r.text, r.value);
        }
        s
    }

    fn push(&mut self, st: &Step, value: String, status: Status) {
        crate::log::log(&format!("sequence: line {} '{}' -> {value}", st.line, st.text));
        self.results.push(Outcome { line: st.line, text: st.text.clone(), value, status });
    }

    fn metric(cx: &Ctx, m: usize, ch: usize) -> Option<f32> {
        let x = cx.an.metrics[ch];
        Some(match m {
            0 => x.fund_dbfs,
            1 => x.fund_hz,
            2 => x.thd_db,
            3 => x.thdn_db,
            4 => x.snr_db,
            5 => x.sinad_db,
            6 => x.sfdr_db,
            7 => x.enob,
            8 => x.noise_floor_dbfs,
            9 => cx.an.rms_db[ch],
            _ => cx.an.peak_db[ch],
        })
        .filter(|v| v.is_finite())
    }

    fn step(&mut self, cx: &mut Ctx) {
        let ch = cx.meas_ch;
        match std::mem::replace(&mut self.run, Run::Idle) {
            Run::Idle => {}
            Run::Next => {
                if self.idx >= self.steps.len() {
                    self.finish(cx, false);
                    return;
                }
                let st = self.steps[self.idx].clone();
                crate::log::log(&format!("sequence: step {} '{}'", self.idx + 1, st.text));
                match st.kind.clone() {
                    Kind::Tone(f, l) => {
                        cx.req.gen_cmd = Some(GenCmd { wave: Some(Wave::Sine), freq: Some(f), level_db: Some(l), on: Some(true) });
                        self.cur_freq = Some(f);
                        self.cur_smpte = None;
                        self.t_change = Instant::now();
                        self.push(&st, "generator set".into(), Status::Info);
                        self.idx += 1;
                        self.run = Run::Next;
                    }
                    Kind::Imd(smpte, l) => {
                        cx.req.gen_cmd = Some(GenCmd { wave: Some(if smpte { Wave::Smpte } else { Wave::Ccif }), freq: None, level_db: Some(l), on: Some(true) });
                        self.cur_freq = None;
                        self.cur_smpte = Some(smpte);
                        self.t_change = Instant::now();
                        self.push(&st, "generator set".into(), Status::Info);
                        self.idx += 1;
                        self.run = Run::Next;
                    }
                    Kind::Off => {
                        cx.req.gen_cmd = Some(GenCmd { on: Some(false), ..Default::default() });
                        self.t_change = Instant::now();
                        self.push(&st, "generator off".into(), Status::Info);
                        self.idx += 1;
                        self.run = Run::Next;
                    }
                    Kind::Avg(s) => {
                        self.avg_s = s;
                        self.idx += 1;
                        self.run = Run::Next;
                    }
                    Kind::Wait(s) => self.run = Run::Wait(Instant::now(), s),
                    Kind::Measure(..) => {
                        self.last_seq = cx.an.seq;
                        self.run = Run::Measure(Instant::now(), Acc::default());
                    }
                    Kind::Sweep => {
                        // a tone left on would add to the sweep
                        cx.req.gen_cmd = Some(GenCmd { on: Some(false), ..Default::default() });
                        cx.sweep.start();
                        self.run = Run::Sweeping(Instant::now());
                    }
                    Kind::Response { f, db, norm } => {
                        self.idx += 1;
                        self.run = Run::Next;
                        let Some(r) = &cx.sweep.result else {
                            self.push(&st, "no sweep yet: add a 'sweep' step first".into(), Status::Error);
                            return;
                        };
                        let h = &r.h;
                        // 1/6 octave power smoothing: single bins of a sweep measurement are noisy
                        let mut pre = vec![0f64; h.mag_db.len() + 1];
                        for (i, d) in h.mag_db.iter().enumerate() {
                            pre[i + 1] = pre[i] + 10f64.powf(*d as f64 / 10.0);
                        }
                        let n = h.mag_db.len();
                        let sm = |k: usize| -> f32 {
                            let (a, b) = (((k as f32 / 1.0595).floor() as usize).min(n - 1), (((k as f32 * 1.0595).ceil() as usize) + 1).min(n));
                            (10.0 * ((pre[b.max(a + 1)] - pre[a]) / (b.max(a + 1) - a) as f64).max(1e-30).log10()) as f32
                        };
                        let off = norm.map(|fr| sm(((fr / h.df).round() as usize).clamp(1, n - 1))).unwrap_or(0.0);
                        let (k0, k1) = (((f.0 / h.df).ceil() as usize).max(1), ((f.1 / h.df).floor() as usize).min(n - 1));
                        if k1 <= k0 {
                            self.push(&st, "frequency range outside the measurement".into(), Status::Error);
                            return;
                        }
                        crate::log::log(&format!("sequence: response {}", [100.0f32, 300.0, 1000.0, 3000.0, 6000.0, 10000.0, 15000.0].iter().map(|&fr| format!("{fr}Hz={:.1}", sm(((fr / h.df).round() as usize).clamp(1, n - 1)) - off)).collect::<Vec<_>>().join(" ")));
                        let (mut worst, mut wf) = (0f32, f.0);
                        let mut ok = true;
                        // evaluate on a log grid so every octave counts the same
                        let steps = 400;
                        for i in 0..steps {
                            let fr = f.0 * (f.1 / f.0).powf(i as f32 / (steps - 1) as f32);
                            let k = ((fr / h.df).round() as usize).clamp(k0, k1);
                            let v = sm(k) - off;
                            let dev = if v > db.1 { v - db.1 } else if v < db.0 { db.0 - v } else { 0.0 };
                            if dev > worst {
                                worst = dev;
                                wf = k as f32 * h.df;
                            }
                            ok &= dev == 0.0;
                        }
                        let v = if ok { "within limits".to_string() } else { format!("out by {worst:.2} dB at {}", crate::app::fmt_hz(wf)) };
                        self.push(&st, v, if ok { Status::Pass } else { Status::Fail });
                    }
                    Kind::Report(path) => {
                        cx.req.report = Some(ReportReq { title: format!("Test sequence report ({})", utc_stamp(now_unix()).1), notes: String::new(), include: ["Spectrum", "Measurements", "Response", "Impulse", "Transfer", "Distortion", "SPL", "Traces"].iter().map(|s| s.to_string()).collect(), open: false, path: Some(path.clone().into()) });
                        self.push(&st, format!("report written to {path}"), Status::Info);
                        self.idx += 1;
                        self.run = Run::Next;
                    }
                    Kind::Rt60(c) => {
                        self.idx += 1;
                        self.run = Run::Next;
                        let Some(r) = &cx.sweep.result else {
                            self.push(&st, "no sweep yet: add a 'sweep' step first".into(), Status::Error);
                            return;
                        };
                        match super::impulse::analyse(&r.ir, r.sr).and_then(|a| a.3.t30.map(|t| (t, "T30")).or(a.3.t20.map(|t| (t, "T20")))) {
                            Some((t, n)) => self.push(&st, format!("{n} = {t:.2} s"), if c.ok(t) { Status::Pass } else { Status::Fail }),
                            None => self.push(&st, "decay too short or too noisy".into(), Status::Error),
                        }
                    }
                }
            }
            Run::Wait(t0, s) => {
                if t0.elapsed().as_secs_f32() >= s {
                    self.idx += 1;
                    self.run = Run::Next;
                } else {
                    self.run = Run::Wait(t0, s);
                }
            }
            Run::Sweeping(t0) => {
                let st = self.steps[self.idx].clone();
                if cx.sweep.busy() {
                    self.run = Run::Sweeping(t0);
                    return;
                }
                if t0.elapsed().as_secs_f32() < 1.5 && matches!(cx.sweep.state, super::sweep::State::Idle) {
                    // not started yet
                    self.run = Run::Sweeping(t0);
                    return;
                }
                match &cx.sweep.state {
                    super::sweep::State::Failed(e) => {
                        let e = e.clone();
                        self.push(&st, e, Status::Error);
                    }
                    _ => self.push(&st, "sweep done".into(), Status::Info),
                }
                self.idx += 1;
                self.run = Run::Next;
            }
            Run::Measure(t0, mut acc) => {
                let st = self.steps[self.idx].clone();
                let Kind::Measure(m, c) = st.kind.clone() else { return };
                // the measurement window must be refilled after the last generator change
                let settle = cx.an.meas_n as f32 / cx.sr.max(1.0) + 0.8; // window refill plus the metric smoothing
                let ready = self.t_change.elapsed().as_secs_f32() >= settle;
                if !ready {
                    self.run = Run::Measure(Instant::now(), Acc::default());
                    return;
                }
                if cx.an.seq != self.last_seq {
                    self.last_seq = cx.an.seq;
                    let mx = cx.an.metrics[ch];
                    let sample = if m == 11 {
                        match self.cur_smpte {
                            Some(s) => imd(cx.an, ch, s).map(|v| v.0),
                            None => None,
                        }
                    } else if m >= 9 || (mx.valid && self.cur_freq.map_or(true, |f| (mx.fund_hz - f).abs() < f * 0.05)) {
                        Self::metric(cx, m, ch)
                    } else {
                        None
                    };
                    if let Some(v) = sample {
                        acc.n += 1;
                        // dB quantities average in power
                        acc.sum += if matches!(m, 1 | 7) { v as f64 } else { 10f64.powf(v as f64 / 10.0) };
                        acc.max = if acc.n == 1 { v } else { acc.max.max(v) };
                    }
                }
                let el = t0.elapsed().as_secs_f32();
                if el >= self.avg_s && acc.n >= 3 {
                    let n = acc.n as f64;
                    let v = match m {
                        1 | 7 => (acc.sum / n) as f32,
                        10 => acc.max,
                        _ => (10.0 * (acc.sum / n).max(1e-30).log10()) as f32,
                    };
                    let unit = ["dBFS", "Hz", "dB", "dB", "dB", "dB", "dBc", "bits", "dBFS", "dBFS", "dBFS", "dB"][m];
                    let ok = c.ok(v);
                    self.push(&st, format!("{v:.2} {unit}   (limit {})", c.text()), if ok { Status::Pass } else { Status::Fail });
                    self.idx += 1;
                    self.run = Run::Next;
                } else if el >= self.avg_s + 6.0 {
                    let why = if m == 11 && self.cur_smpte.is_none() { "no 'imd' step before this measurement" } else { "no valid signal (is the generator fed back into the input?)" };
                    self.push(&st, why.into(), Status::Error);
                    self.idx += 1;
                    self.run = Run::Next;
                } else {
                    self.run = Run::Measure(t0, acc);
                }
            }
        }
    }
}

impl Tool for Sequence {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        if let Some(d) = &self.load {
            if let Some(res) = d.take() {
                self.load = None;
                if let Some(p) = res {
                    match std::fs::read_to_string(&p) {
                        Ok(t) => self.text = t,
                        Err(e) => self.msg = format!("load failed: {e}"),
                    }
                }
            }
        }
        if let Some(d) = &self.save {
            if let Some(res) = d.take() {
                self.save = None;
                if let Some(p) = res {
                    let body = if p.extension().is_some_and(|e| e == "txt" || e == "csv") && !self.results.is_empty() { self.report_text(cx) } else { self.text.clone() };
                    cx.req.note = Some(match std::fs::write(&p, body) {
                        Ok(_) => format!("Saved {}", p.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
        }
        if self.text != self.saved_text {
            if let Some(d) = script_path().parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if std::fs::write(script_path(), &self.text).is_ok() {
                self.saved_text = self.text.clone();
            }
        }
        if self.pending && !self.running() {
            self.pending = false;
            crate::log::log("sequence: starting from the command line");
            if self.results.is_empty() {
                self.start(cx);
            }
            if !self.running() {
                // script or input error: report and quit
                self.finish(cx, false);
            }
        }
        if !visible && self.running() && !self.headless {
            self.finish(cx, true);
        }
        if self.running() {
            self.step(cx);
            // a headless run must never hang
            if self.headless && self.started.is_some_and(|t| t.elapsed().as_secs() > 1800) {
                self.results.push(Outcome { line: 0, text: "timeout".into(), value: "sequence took longer than 30 minutes".into(), status: Status::Error });
                self.finish(cx, true);
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let busy = self.running();
        ui.horizontal_wrapped(|ui| {
            if ui.add(egui::Button::new(RichText::new(if busy { "Stop" } else { "Run" }).strong()).min_size(egui::vec2(80.0, 26.0))).clicked() {
                if busy {
                    self.finish(cx, true);
                } else {
                    self.start(cx);
                }
            }
            if busy {
                ui.add(egui::ProgressBar::new(self.idx as f32 / self.steps.len().max(1) as f32).desired_width(120.0).text(format!("step {}/{}", (self.idx + 1).min(self.steps.len()), self.steps.len())));
            } else if !self.results.is_empty() {
                let v = self.verdict();
                let col = if v == "PASS" { Color32::from_rgb(110, 230, 140) } else { Color32::from_rgb(255, 120, 100) };
                ui.label(RichText::new(v).strong().size(16.0).color(col));
            }
            ui.add_enabled_ui(!busy, |ui| {
                if ui.add_enabled(self.load.is_none(), egui::Button::new("Load...")).clicked() {
                    self.load = Some(dialogs::open("Load test sequence", "Sequence", &["vss", "txt"]));
                }
                if ui.add_enabled(self.save.is_none(), egui::Button::new("Save...")).on_hover_text("Save as .vss to keep the script; as .txt to save the result report").clicked() {
                    self.save = Some(dialogs::save("Save sequence or report", "Sequence / report", &["vss", "txt"], "sequence.vss".into()));
                }
                if ui.button("Example").clicked() {
                    self.text = EXAMPLE.to_string();
                }
            });
            if !self.msg.is_empty() && !busy {
                ui.label(RichText::new(&self.msg).small().weak());
            }
        });
        let h = (ui.available_height() * 0.5).clamp(90.0, 400.0);
        egui::ScrollArea::vertical().id_salt("seq_script").max_height(h).show(ui, |ui| {
            ui.add_enabled(!busy, egui::TextEdit::multiline(&mut self.text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).desired_rows(8).hint_text("tone 1000 -20\nmeasure thdn < -80"));
        });
        egui::CollapsingHeader::new("Commands").default_open(false).show(ui, |ui| {
            ui.label(RichText::new("tone <Hz> <dBFS>   imd smpte|ccif <dBFS>   off   wait <s>   set avg <s>\nmeasure <metric> < v | > v | in a..b     metrics: level freq thd thdn snr sinad sfdr enob noise rms peak imd\nsweep    response <Hz a..b> <dB a..b> [abs | norm <Hz>]    rt60 < v    report <file.html>\nLines starting with # are comments. Command line: visuscope --run file.vss --report result.txt").small().monospace());
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("seq_results").auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("seq_grid").num_columns(4).spacing([10.0, 3.0]).striped(true).show(ui, |ui| {
                for r in &self.results {
                    let (t, c) = match r.status {
                        Status::Pass => ("PASS", Color32::from_rgb(110, 230, 140)),
                        Status::Fail => ("FAIL", Color32::from_rgb(255, 120, 100)),
                        Status::Error => ("ERROR", Color32::from_rgb(255, 170, 60)),
                        Status::Info => ("", Color32::GRAY),
                    };
                    ui.label(RichText::new(if r.line > 0 { format!("{}", r.line) } else { String::new() }).small().weak());
                    ui.label(RichText::new(t).strong().color(c));
                    ui.label(RichText::new(&r.text).monospace());
                    ui.label(RichText::new(&r.value).color(if r.status == Status::Info { Color32::GRAY } else { c }));
                    ui.end_row();
                }
            });
        });
    }

    fn report(&self, _cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        if self.results.is_empty() {
            return Vec::new();
        }
        let v = self.verdict();
        let rows = self
            .results
            .iter()
            .map(|r| {
                let st = match r.status {
                    Status::Pass => "PASS",
                    Status::Fail => "FAIL",
                    Status::Info => "",
                    Status::Error => "ERROR",
                };
                vec![r.line.to_string(), st.to_string(), r.text.clone(), r.value.clone()]
            })
            .collect();
        vec![Section {
            id: "Sequence",
            title: "Test sequence".into(),
            blocks: vec![Block::Verdict(v == "PASS", format!("Result: {v}")), Block::Table { head: ["Line", "", "Step", "Result"].iter().map(|s| s.to_string()).collect(), rows }],
        }]
    }

    fn status(&self) -> Option<(String, Color32)> {
        self.running().then(|| (format!("sequence step {}/{}", (self.idx + 1).min(self.steps.len()), self.steps.len()), Color32::from_rgb(255, 190, 80)))
    }

    /// `run\t<script>\t<report>` from the command line.
    fn command(&mut self, c: &str) {
        let p: Vec<&str> = c.split('\t').collect();
        if p.first() == Some(&"run") && p.len() >= 2 {
            match std::fs::read_to_string(p[1]) {
                Ok(t) => {
                    self.text = t;
                    self.report = p.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
                    self.headless = true;
                    self.pending = true;
                }
                Err(e) => {
                    self.msg = format!("cannot read {}: {e}", p[1]);
                    self.results = vec![Outcome { line: 0, text: p[1].into(), value: self.msg.clone(), status: Status::Error }];
                    self.report = p.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
                    self.headless = true;
                    self.pending = true;
                }
            }
        }
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("avg", self.avg_s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_script() {
        let s = parse("# c\ntone 1000 -20 # level\nmeasure thdn < -80\nmeasure level in -20.5..-19.5\nresponse 100..10000 -3..3 norm 1000\nrt60 < 0.6\nimd smpte -10\nset avg 2\nsweep\noff\nwait 1.5\n").unwrap();
        assert_eq!(s.len(), 10);
        assert_eq!(s[0].kind, Kind::Tone(1000.0, -20.0));
        assert_eq!(s[1].kind, Kind::Measure(3, Cond::Lt(-80.0)));
        assert_eq!(s[2].kind, Kind::Measure(0, Cond::In(-20.5, -19.5)));
        assert_eq!(s[3].kind, Kind::Response { f: (100.0, 10000.0), db: (-3.0, 3.0), norm: Some(1000.0) });
        assert_eq!(s[1].line, 3);
        assert!(parse("measure bogus < 1").unwrap_err().contains("unknown metric"));
        assert!(parse("tone abc -20").is_err());
        assert!(parse("# nothing").is_err());
        assert!(Cond::In(-1.0, 1.0).ok(0.5) && !Cond::Lt(-80.0).ok(-70.0) && !Cond::Gt(0.0).ok(f32::NAN));
    }
}
