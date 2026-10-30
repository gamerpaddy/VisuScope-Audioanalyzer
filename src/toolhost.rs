//! Glue between the app and the pluggable tools (see `tools/mod.rs`).

use crate::app::App;
use crate::tools::{self, AudioReq, Ctx, Requests, SettingsIn, SettingsOut, SpecOverlay};
use crate::windows::BUILTIN;
use eframe::egui::{Color32, Ui};

/// Build a `Ctx` from disjoint fields of the app, so the tool list can stay mutably borrowed.
macro_rules! make_ctx {
    ($s:ident, $req:expr) => {{
        let unit_off = [$s.unit_off(0), $s.unit_off(1)];
        let unit_name = $s.unit_name();
        let sr = $s.sr();
        Ctx { sh: &$s.sh, an: &$s.an, sr, meas_ch: $s.meas_ch, unit_off, unit_name, snaps: &$s.snaps, gen_running: $s.gen_handle.is_some(), out_devs: &$s.gen_devs, sweep: &mut $s.sweep, req: $req }
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
        match req.audio {
            AudioReq::Stop => self.audio = None,
            AudioReq::Restart => self.start_selected(),
            AudioReq::None => {}
        }
        if let Some(n) = req.note {
            self.note = n;
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
        let mut req = Requests::default();
        {
            let mut cx = make_ctx!(self, &mut req);
            cx.sweep.tick(cx.sh, cx.gen_running, cx.req);
            for (k, t) in self.tools.iter_mut().enumerate() {
                t.update(&mut cx, self.wins[BUILTIN + k].open);
            }
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
