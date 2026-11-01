//! Glue between the app and the pluggable tools (see `tools/mod.rs`).

use crate::app::App;
use crate::cal::fmt_volts;
use crate::shared::MAXH;
use crate::tools::{self, AudioReq, Ctx, ReportReq, Requests, SettingsIn, SettingsOut, SpecOverlay};
use crate::windows::BUILTIN;
use eframe::egui::{Color32, Ui};

/// Build a `Ctx` from disjoint fields of the app, so the tool list can stay mutably borrowed.
macro_rules! make_ctx {
    ($s:ident, $req:expr) => {{
        let unit_off = [$s.unit_off(0), $s.unit_off(1)];
        let unit_name = $s.unit_name();
        let sr = $s.sr();
        Ctx { sh: &$s.sh, an: &$s.an, sr, meas_ch: $s.meas_ch, unit_off, unit_name, snaps: &$s.snaps, gen_running: $s.gen_handle.is_some(), tg: &$s.tg, out_devs: &$s.gen_devs, graph_on: $s.graph_on, gens: &$s.gens_shared, default_in: $s.devices.get($s.sel_dev).map(|d| d.id.clone()).unwrap_or_default(), sweep: &mut $s.sweep, lib: &mut $s.lib, req: $req }
    }};
}

fn prefix(k: usize) -> String {
    format!("t_{}_", tools::INFO[k].short.to_lowercase())
}

impl App {
    fn apply_requests(&mut self, req: Requests) {
        if req.start_gen && self.gen_handle.is_none() {
            self.start_gen();
        }
        if let Some(code) = req.exit {
            self.save_settings(true);
            self.lib.save_auto();
            std::process::exit(code);
        }
        if let Some(c) = req.gen_cmd {
            if let Some(w) = c.wave {
                self.tg.wave = w;
            }
            if let Some(f) = c.freq {
                self.tg.freq = f;
            }
            if let Some(l) = c.level_db {
                self.tg.level_db = [l, l];
            }
            if let Some(on) = c.on {
                self.tg.on = on;
                if on && self.gen_handle.is_none() {
                    self.start_gen();
                }
            }
        }
        match req.audio {
            AudioReq::Stop => self.audio = None,
            AudioReq::Restart => self.start_selected(),
            AudioReq::None => {}
        }
        if let Some(r) = req.report {
            self.note = self.make_report(r);
        }
        if let Some(n) = req.note {
            self.note = n;
        }
    }

    /// Log-spaced points (max-pooled where a pixel covers several bins) of a spectrum in dB.
    fn pool_spectrum(data: &[f32], sr: f32, n: usize, f0: f32, f1: f32, off: f32, count: usize) -> Vec<(f32, f32)> {
        if data.len() < 4 || n == 0 {
            return Vec::new();
        }
        let bin = sr / n as f32;
        let r = (f1 / f0).powf(1.0 / count as f32);
        (0..count)
            .filter_map(|i| {
                let (a, b) = (f0 * r.powi(i as i32), f0 * r.powi(i as i32 + 1));
                let (ka, kb) = (a / bin, b / bin);
                if ka >= (data.len() - 1) as f32 {
                    return None;
                }
                let v = if kb - ka < 1.0 {
                    let mid = 0.5 * (ka + kb);
                    let k = mid.floor() as usize;
                    let t = mid - k as f32;
                    data[k] * (1.0 - t) + data[(k + 1).min(data.len() - 1)] * t
                } else {
                    data[ka.floor() as usize..=(kb.ceil() as usize).min(data.len() - 1)].iter().cloned().fold(f32::MIN, f32::max)
                };
                Some(((a * b).sqrt(), v + off))
            })
            .collect()
    }

    /// Everything the report can contain right now, in order.
    fn report_sections(&mut self, include: &[String]) -> Vec<crate::report::Section> {
        use crate::report::*;
        let want = |id: &str| include.iter().any(|s| s == id);
        let mut out: Vec<Section> = Vec::new();
        let unit = self.unit_name();
        if self.an.n > 0 && want("Spectrum") {
            let (f0, f1) = (self.u_to_f(self.f_lo).max(10.0), self.u_to_f(self.f_hi).min(self.an.sr * 0.5));
            let mut series: Vec<Series> = Vec::new();
            for ch in 0..2 {
                if !self.show_ch[ch] {
                    continue;
                }
                let name = ["L", "R"][ch];
                let data = if self.show_avg || !self.show_live { &self.an.avg[ch] } else { &self.an.live[ch] };
                let label = if self.show_avg || !self.show_live { "avg" } else { "live" };
                series.push(Series { name: format!("{name} {label}"), color: COLORS[ch], pts: Self::pool_spectrum(data, self.an.sr, self.an.n, f0, f1, self.unit_off(ch), 900) });
            }
            for (i, s) in self.snaps.iter().filter(|s| s.visible).enumerate() {
                series.push(Series { name: s.name.clone(), color: COLORS[(i + 2) % COLORS.len()], pts: Self::pool_spectrum(&s.data, s.sr, s.n, f0, f1, self.unit_off(s.ch.min(1)), 900) });
            }
            let mut blocks = vec![Block::Plot(PlotSpec { title: "Spectrum".into(), xlog: self.p.log_freq, xlabel: "Hz".into(), ylabel: unit.into(), x: (f0, f1), y: (self.db_lo, self.db_hi), series })];
            blocks.insert(0, Block::Text(format!("FFT {} points, {} window, {} Hz sample rate, averaging {:.0}{}.", self.an.n, self.p.window.name(), self.an.sr, self.p.avg_count, if self.smooth_oct > 0 { format!(", smoothing {}", crate::app::SMOOTH_OCT[self.smooth_oct].1) } else { String::new() })));
            out.push(Section { id: "Spectrum", title: "Spectrum".into(), blocks });
        }
        if want("Measurements") {
            let m = self.an.metrics[self.meas_ch];
            let ch = ["Left", "Right"][self.meas_ch];
            let blocks = if m.valid {
                let r = |a: &str, b: String| vec![a.to_string(), b];
                let mut rows = vec![
                    r("Fundamental", format!("{} at {:.1} dBFS", crate::app::fmt_hz(m.fund_hz), m.fund_dbfs)),
                    r("THD", format!("{:.2} dB", m.thd_db)),
                    r("THD+N", format!("{:.2} dB", m.thdn_db)),
                    r("SINAD", format!("{:.2} dB", m.sinad_db)),
                    r("SNR", format!("{:.2} dB", m.snr_db)),
                    r("SFDR", format!("{:.1} dBc", m.sfdr_db)),
                    r("ENOB", format!("{:.2} bits", m.enob)),
                    r("Noise floor", format!("{:.1} dBFS/bin", m.noise_floor_dbfs)),
                    r("RMS / peak", format!("{:.1} / {:.1} dBFS", self.an.rms_db[self.meas_ch], self.an.peak_db[self.meas_ch])),
                ];
                if self.cal.on {
                    let v = 10f32.powf(m.fund_dbfs / 20.0) * self.cal.vpk[self.meas_ch];
                    rows.push(r("Fundamental (voltage)", format!("{} rms", fmt_volts(v / std::f32::consts::SQRT_2))));
                }
                let harm: Vec<Vec<String>> = (2..=self.p.harmonics.min(MAXH)).filter(|&h| m.harm_db[h - 1] > -199.0).map(|h| vec![format!("H{h}"), crate::app::fmt_hz(m.harm_hz[h - 1]), format!("{:.1} dBc", m.harm_db[h - 1])]).collect();
                let mut b = vec![Block::Table { head: vec![format!("{ch} channel"), String::new()], rows }];
                if !harm.is_empty() {
                    b.push(Block::Table { head: vec!["Harmonic".into(), "Frequency".into(), "Level".into()], rows: harm });
                }
                b
            } else {
                vec![Block::Text("No tone detected on the measurement channel.".into())]
            };
            out.push(Section { id: "Measurements", title: "Distortion and noise measurements".into(), blocks });
        }
        let mut req = Requests::default();
        let cx = make_ctx!(self, &mut req);
        for t in &self.tools {
            out.extend(t.report(&cx).into_iter().filter(|s| want(s.id)));
        }
        if want("Traces") && self.lib.items.iter().any(|m| m.visible) {
            let series: Vec<Series> = self.lib.items.iter().filter(|m| m.visible).enumerate().map(|(i, m)| Series { name: m.name.clone(), color: COLORS[i % COLORS.len()], pts: m.f.iter().copied().zip(m.mag.iter().copied()).collect() }).collect();
            let x = (series.iter().map(|s| s.pts[0].0).fold(f32::MAX, f32::min), series.iter().map(|s| s.pts.last().unwrap().0).fold(0.0, f32::max));
            let y = auto_range(&series, 5.0, 20.0);
            out.push(Section { id: "Traces", title: "Stored traces".into(), blocks: vec![Block::Plot(PlotSpec { title: "Stored traces".into(), xlog: true, xlabel: "Hz".into(), ylabel: "dB".into(), x, y, series })] });
        }
        out
    }

    /// Write the report and (optionally) open it. Returns a status message.
    pub fn make_report(&mut self, r: ReportReq) -> String {
        use crate::report::*;
        let sections = self.report_sections(&r.include);
        if sections.is_empty() {
            return "Report: nothing to include yet (measure something first)".into();
        }
        let (stamp, iso) = tools::utc_stamp(tools::now_unix());
        let dev = self.devices.get(self.sel_dev).map(|d| d.label.clone()).unwrap_or_default();
        let mut meta: Vec<(String, String)> = vec![
            ("Date (UTC)".into(), iso),
            ("Input".into(), format!("{dev} - {}", self.sh.status.lock())),
            ("Channels".into(), format!("L = input {}, R = input {}", self.ch_a + 1, self.ch_b + 1)),
            ("Analysis".into(), format!("FFT {}, {}, average {:.0}", self.p.fft_size, self.p.window.name(), self.p.avg_count)),
        ];
        let mic = self.mic[self.mic_angle].as_ref().or(self.mic[0].as_ref()).or(self.mic[1].as_ref());
        meta.push(("Microphone".into(), match mic {
            Some(c) if self.mic_on => format!("calibration file {}", c.name),
            _ => "no calibration file".into(),
        }));
        meta.push(("Level calibration".into(), if self.cal.on { format!("0 dBFS = {} pk (L), {} pk (R); unit {}", fmt_volts(self.cal.vpk[0]), fmt_volts(self.cal.vpk[1]), self.unit.name()) } else { "none (dBFS)".into() }));
        if self.tg.on {
            meta.push(("Generator".into(), format!("{} {:.0} Hz, {:.1} dBFS", self.tg.wave.name(), self.tg.freq, self.tg.level_db[0])));
        }
        meta.push(("Software".into(), format!("VisuScope {}", env!("CARGO_PKG_VERSION"))));
        let html = render_html(&r.title, &r.notes, &meta, &sections);
        let path = r.path.clone().unwrap_or_else(|| tools::docs_dir().join(format!("report_{stamp}.html")));
        match std::fs::write(&path, html) {
            Ok(_) => {
                if r.open {
                    tools::reportui::open_path(&path);
                }
                format!("Report saved: {}", path.display())
            }
            Err(e) => format!("Report failed: {e}"),
        }
    }

    /// Test hook: `VISUSCOPE_TEST_CABLE=1` routes the generator into the virtual cable and listens to
    /// it (nothing audible), `VISUSCOPE_AUTO_SWEEP=1` then runs a quiet sweep measurement by itself.
    pub fn apply_test_hooks(&mut self) {
        if std::env::var_os("VISUSCOPE_TEST_CABLE").is_none() {
            return;
        }
        if let Some(i) = self.devices.iter().position(|d| d.label.contains("CABLE Output")) {
            self.sel_dev = i;
        }
        if let Some(i) = self.gen_devs.iter().position(|d| d.label.contains("CABLE Input")) {
            self.gen_sel = i;
        }
        self.sweep.p.amp_db = -30.0;
        self.sweep.p.secs = 2.0;
        self.sweep.p.tail = 1.0;
    }

    /// Per-frame work of every tool (also those whose window is closed).
    pub fn tools_update(&mut self) {
        if std::env::var_os("VISUSCOPE_AUTO_SWEEP").is_some() && !self.test_sweep_started && self.sh.t0.elapsed().as_secs() >= 6 {
            self.test_sweep_started = true;
            self.sweep.start();
        }
        if let Some((script, report)) = self.autorun.clone() {
            if self.sh.t0.elapsed().as_secs() >= 4 && self.sh.sr.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                self.autorun = None;
                if let Some(k) = (0..tools::TOOL_COUNT).find(|&k| tools::INFO[k].short == "Sequence") {
                    self.wins[BUILTIN + k].open = true;
                    self.tools[k].command(&format!("run\t{script}\t{}", report.unwrap_or_default()));
                }
            }
        }
        let mut req = Requests::default();
        {
            let mut cx = make_ctx!(self, &mut req);
            cx.sweep.tick(cx.sh, cx.gen_running, cx.req);
            for (k, t) in self.tools.iter_mut().enumerate() {
                t.update(&mut cx, self.wins[BUILTIN + k].open);
            }
        }
        if let Some(n) = self.lib.poll() {
            req.note = Some(n);
        }
        self.apply_requests(req);
    }

    pub fn tool_ui(&mut self, ui: &mut Ui, k: usize) {
        let mut req = Requests::default();
        {
            let mut cx = make_ctx!(self, &mut req);
            self.tools[k].ui(ui, &mut cx);
        }
        self.apply_requests(req);
    }

    /// Let the tools paint onto the spectrum plot.
    pub fn tool_overlays(&mut self, ov: &SpecOverlay) {
        let mut req = Requests::default();
        let cx = make_ctx!(self, &mut req);
        for t in &self.tools {
            t.overlay(ov, &cx);
        }
    }

    pub fn tool_statuses(&self) -> Vec<(String, Color32)> {
        self.tools.iter().filter_map(|t| t.status()).collect()
    }

    pub fn tools_save(&self, out: &mut String) {
        for (k, t) in self.tools.iter().enumerate() {
            t.save(&mut SettingsOut { s: out, pre: prefix(k) });
        }
    }

    pub fn tools_load(&mut self, m: &std::collections::HashMap<&str, &str>) {
        for (k, t) in self.tools.iter_mut().enumerate() {
            t.load(&SettingsIn { m, pre: prefix(k) });
        }
    }
}
