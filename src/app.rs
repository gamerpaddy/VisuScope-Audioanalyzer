use crate::audio::{self, AudioHandle, DevInfo};
use crate::cal::{fmt_volts, Calib, Unit, DBU_REF_V};
use crate::colormap::Cmap;
use crate::dsp::{self, Window};
use crate::layouts::UserPreset;
use crate::tonegen::{GenHandle, GenParams};
use crate::tools::{self, sweep::Sweep, Tool};
use crate::scope::ScopeState;
use crate::windows::{WinState, WIN_COUNT};
use crate::shared::*;
use crate::stereo::StereoState;
use crate::view3d::View3d;
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui};
use std::sync::Arc;
use std::time::Instant;

pub const SG_W: usize = 3072;
pub const WF_H: usize = 768;

pub struct Snapshot {
    pub name: String,
    pub color: Color32,
    pub data: Vec<f32>,
    pub sr: f32,
    pub n: usize,
    pub visible: bool,
    pub ch: usize,
}

#[derive(Default, Clone, Copy)]
pub struct Act {
    pub zx: f32,
    pub zy: f32,
    pub px: f32,
    pub py: f32,
    pub tilt: f32,
    pub rot: f32,
    pub reset: bool,
}

pub struct App {
    pub sh: Arc<Shared>,
    pub audio: Option<AudioHandle>,
    pub devices: Vec<DevInfo>,
    pub sel_dev: usize,
    pub pair: usize,
    pub p: Params,
    pub p_sent: Params,
    pub an: Analysis,
    pub an_seq: u64,
    pub epoch: u32,

    // shared frequency view (normalised axis) and spectrum dB view
    pub f_lo: f32,
    pub f_hi: f32,
    pub db_lo: f32,
    pub db_hi: f32,
    pub show_live: bool,
    pub show_avg: bool,
    pub show_max: bool,
    pub show_min: bool,
    pub show_ch: [bool; 2],
    pub fill: bool,
    pub line_w: f32,

    pub cmap: Cmap,
    pub lut: [Color32; 256],
    pub dyn_lut: [Color32; 256],
    pub col_lo: f32,
    pub col_hi: f32,
    pub recolor: bool,
    pub last_recolor: Instant,

    pub sg_tex: TextureHandle,
    pub sg_hist: Vec<u8>,
    pub sg_head: usize,
    pub sg_span: usize,
    pub sg_back: usize,
    pub wf_tex: TextureHandle,
    pub wf_hist: Vec<u8>,
    pub wf_head: usize,
    pub wf_span: usize,
    pub wf_back: usize,
    pub d3: View3d,
    pub stereo: StereoState,

    pub cur_a: Option<f32>,
    pub cur_b: Option<f32>,
    pub snap_peak: bool,
    pub snaps: Vec<Snapshot>,
    pub snap_src: usize,
    pub wins: [WinState; WIN_COUNT],
    pub need_tile: bool,
    /// re-tile automatically whenever a window is opened or closed
    pub auto_tile: bool,
    pub open_mask: u32,
    pub settle: i32,
    pub saved_area: Option<egui::Vec2>,
    pub area: Option<egui::Rect>,
    pub layout_ready: bool,
    pub max_req: Option<usize>,
    pub sized: bool,
    pub size_frames: u32,
    pub center_at: Option<u32>,
    pub scan_slot: Arc<parking_lot::Mutex<Option<(Vec<DevInfo>, Vec<DevInfo>)>>>,
    pub scanning: bool,
    pub tg: GenParams,
    pub gen_sent: GenParams,
    pub gen_shared: Arc<parking_lot::Mutex<GenParams>>,
    pub gen_devs: Vec<DevInfo>,
    pub gen_sel: usize,
    pub gen_handle: Option<GenHandle>,
    pub gen_status: Arc<parking_lot::Mutex<String>>,
    pub gen_link: bool,
    pub meas_ch: usize,
    pub note: String,
    pub shot_note: Arc<parking_lot::Mutex<String>>,
    pub ms: [f32; WIN_COUNT + 1],
    pub prof: bool,
    pub prof_t: Instant,
    pub frames: u32,
    pub ui_fps: f32,
    pub fps_cap: f32,
    pub rotated: bool,
    pub sm_metrics: [Metrics; 2],
    pub sm_rms: [f32; 2],
    pub sm_peak: [f32; 2],
    pub sm_dc: [f32; 2],
    /// corr fast, corr slow, balance, mid, side (throttled for reading)
    pub sm_st: [f32; 5],
    pub sm_t: Instant,
    pub readout_hz: f32,
    pub readout_hold: bool,
    last_saved: String,
    last_save_t: Instant,
    pub scope: ScopeState,
    pub cal: Calib,
    pub unit: Unit,
    pub tools: Vec<Box<dyn Tool>>,
    pub sweep: Sweep,
    pub popped: [bool; WIN_COUNT],
    pub presets: Vec<UserPreset>,
    pub preset_name: String,
    pub test_sweep_started: bool,
}

/// If the UI thread stops responding for 3 s, write what every thread was doing to `visuscope_hang.log`.
fn spawn_watchdog(sh: Arc<Shared>) {
    use std::sync::atomic::Ordering::Relaxed;
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            let mut reported = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if sh.quit.load(Relaxed) {
                    return;
                }
                let beat = sh.ui_beat.load(Relaxed);
                if beat == 0 {
                    continue;
                }
                let now = sh.t0.elapsed().as_millis() as u64;
                let stalled = now.saturating_sub(beat);
                if stalled > 3000 && !reported {
                    reported = true;
                    let status = sh.status.try_lock().map(|s| s.clone()).unwrap_or_else(|| "<locked>".into());
                    let ring = sh.ring.try_lock().map(|r| r.total.to_string()).unwrap_or_else(|| "<locked>".into());
                    let msg = format!(
                        "UI thread stalled for {stalled} ms at t={now} ms\nUI phase marker: {} (1 start, 2 params, 3 analysis pulled, 4 columns, 5 stereo, 6 bars, 7 windows, 8 end)\naudio status: {status}\nsample rate {} Hz, xruns {}\nDSP heartbeat age {} ms\nring total {ring}\n",
                        sh.ui_phase.load(Relaxed),
                        sh.sr.load(Relaxed),
                        sh.xruns.load(Relaxed),
                        now.saturating_sub(sh.dsp_beat.load(Relaxed)),
                    );
                    crate::log::log(&format!("HANG: {msg}"));
                    let _ = std::fs::write(crate::log::path().with_file_name("visuscope_hang.log"), msg);
                } else if stalled < 1000 && reported {
                    reported = false;
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open("visuscope_hang.log") {
                        use std::io::Write;
                        let _ = writeln!(f, "UI recovered at t={now} ms");
                        crate::log::log("UI recovered after stall");
                    }
                }
            }
        })
        .ok();
}

fn settings_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("settings.txt")
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let sh = Arc::new(Shared::new());
        dsp::spawn(sh.clone());
        spawn_watchdog(sh.clone());
        let devices = audio::list_devices();
        for d in &devices {
            crate::log::log(&format!("device: {} | {}", d.id, d.label));
        }
        let cmap = Cmap::Inferno;
        let lut = cmap.lut();
        let ctx = &cc.egui_ctx;
        let sg_tex = ctx.load_texture("spectrogram", ColorImage::filled([SG_W, ROWS], lut[0]), TextureOptions::LINEAR);
        let wf_tex = ctx.load_texture("waterfall", ColorImage::filled([ROWS, WF_H], lut[0]), TextureOptions::LINEAR);
        let mut app = Self {
            sh: sh.clone(),
            audio: None,
            devices,
            sel_dev: 0,
            pair: 0,
            p: Params::default(),
            p_sent: Params::default(),
            an: Analysis::default(),
            an_seq: 0,
            epoch: 0,
            f_lo: 0.0,
            f_hi: 1.0,
            db_lo: -130.0,
            db_hi: 5.0,
            show_live: true,
            show_avg: true,
            show_max: false,
            show_min: false,
            show_ch: [true, true],
            fill: true,
            line_w: 1.2,
            cmap,
            lut,
            dyn_lut: lut,
            col_lo: -120.0,
            col_hi: -10.0,
            recolor: false,
            last_recolor: Instant::now(),
            sg_tex,
            sg_hist: vec![0; SG_W * ROWS],
            sg_head: 0,
            sg_span: SG_W / 2,
            sg_back: 0,
            wf_tex,
            wf_hist: vec![0; WF_H * ROWS],
            wf_head: 0,
            wf_span: WF_H / 2,
            wf_back: 0,
            d3: View3d::new(),
            stereo: StereoState::new(),
            cur_a: None,
            cur_b: None,
            snap_peak: true,
            snaps: Vec::new(),
            snap_src: 0,
            wins: std::array::from_fn(WinState::new),
            need_tile: false,
            auto_tile: true,
            open_mask: 0,
            settle: 0,
            saved_area: None,
            area: None,
            layout_ready: false,
            max_req: None,
            sized: false,
            size_frames: 0,
            center_at: None,
            scan_slot: Arc::new(parking_lot::Mutex::new(None)),
            scanning: false,
            tg: GenParams::default(),
            gen_sent: GenParams::default(),
            gen_shared: Arc::new(parking_lot::Mutex::new(GenParams::default())),
            gen_devs: crate::tonegen::list_output_devices(),
            gen_sel: 0,
            gen_handle: None,
            gen_status: Arc::new(parking_lot::Mutex::new(String::from("Off"))),
            gen_link: true,
            meas_ch: 0,
            note: String::new(),
            shot_note: Arc::new(parking_lot::Mutex::new(String::new())),
            ms: [0.0; WIN_COUNT + 1],
            prof: std::env::var_os("VISUSCOPE_PROF").is_some(),
            prof_t: Instant::now(),
            frames: 0,
            ui_fps: 0.0,
            fps_cap: 60.0,
            rotated: false,
            sm_metrics: Default::default(),
            sm_rms: [0.0; 2],
            sm_peak: [0.0; 2],
            sm_dc: [0.0; 2],
            sm_st: [0.0; 5],
            sm_t: Instant::now(),
            readout_hz: 3.0,
            readout_hold: false,
            last_saved: String::new(),
            last_save_t: Instant::now(),
            scope: ScopeState::new(),
            cal: Calib::new(),
            unit: Unit::Dbfs,
            tools: tools::all(),
            sweep: Sweep::new(),
            popped: [false; WIN_COUNT],
            presets: Vec::new(),
            preset_name: String::new(),
            test_sweep_started: false,
        };
        app.load_settings();
        app.apply_test_hooks();
        app.rebuild_dyn_lut();
        app.start_selected();
        app
    }

    fn settings_string(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let mut kv = |k: &str, v: &dyn std::fmt::Display| {
            let _ = writeln!(s, "{k}={v}");
        };
        if let Some(d) = self.devices.get(self.sel_dev) {
            kv("device", &d.id);
        }
        kv("pair", &self.pair);
        let p = &self.p;
        kv("fft", &p.fft_size);
        kv("window", &p.window.name());
        kv("update_hz", &p.update_hz);
        kv("avg_count", &p.avg_count);
        kv("cols_per_sec", &p.cols_per_sec);
        kv("spectro_src", &p.spectro_src);
        kv("log_freq", &p.log_freq);
        kv("bw_lo", &p.bw_lo);
        kv("bw_hi", &p.bw_hi);
        kv("harmonics", &p.harmonics);
        kv("manual_fund", &p.manual_fund);
        kv("fund_hz", &p.fund_hz);
        kv("show_live", &self.show_live);
        kv("show_avg", &self.show_avg);
        kv("show_max", &self.show_max);
        kv("show_min", &self.show_min);
        kv("show_l", &self.show_ch[0]);
        kv("show_r", &self.show_ch[1]);
        kv("fill", &self.fill);
        kv("line_w", &self.line_w);
        kv("cmap", &self.cmap.name());
        kv("col_lo", &self.col_lo);
        kv("col_hi", &self.col_hi);
        kv("d3_rows", &self.d3.rows);
        kv("d3_cols", &self.d3.cols);
        kv("d3_rate", &self.d3.rate);
        kv("d3_hscale", &self.d3.hscale);
        kv("d3_wire", &self.d3.wire);
        kv("st_auto", &self.stereo.auto);
        kv("st_gain", &self.stereo.gain_db);
        kv("st_persist", &self.stereo.persist_ms);
        kv("st_bright", &self.stereo.brightness);
        kv("st_hold", &self.stereo.hold);
        kv("rotated", &self.rotated);
        kv("auto_tile", &self.auto_tile);
        kv("meas_ch", &self.meas_ch);
        kv("readout_hz", &self.readout_hz);
        kv("fps_cap", &self.fps_cap);
        kv("snap_peak", &self.snap_peak);
        kv("snap_src", &self.snap_src);
        kv("cal_on", &self.cal.on);
        kv("cal_vpk_l", &self.cal.vpk[0]);
        kv("cal_vpk_r", &self.cal.vpk[1]);
        kv("cal_ref", &self.cal.ref_vrms);
        kv("cal_method", &self.cal.method);
        kv("cal_entry_unit", &self.cal.entry_unit);
        kv("cal_entry_val", &self.cal.entry_val);
        kv("unit", &self.unit.index());
        let sc = &self.scope;
        kv("sc_t_div", &sc.t_div);
        kv("sc_v_div", &sc.v_div);
        kv("sc_v_off", &sc.v_off);
        kv("sc_mode", &sc.mode);
        kv("sc_rising", &sc.rising);
        kv("sc_level", &sc.level);
        kv("sc_pos", &sc.pos);
        kv("sc_hyst", &sc.hyst);
        kv("sc_src", &sc.trig_src);
        kv("sc_hf", &sc.hf_reject);
        kv("sc_hf_hz", &sc.hf_hz);
        kv("sc_l", &sc.show[0]);
        kv("sc_r", &sc.show[1]);
        for (i, w) in self.wins.iter().enumerate() {
            kv(&format!("w{i}_open"), &w.open);
            // stored relative to the central area so the layout can be rescaled to any window size
            if let (Some(r), Some(a)) = (w.rect, self.area) {
                let rel = r.min - a.min;
                kv(&format!("w{i}_rect"), &format!("{:.0},{:.0},{:.0},{:.0}", rel.x, rel.y, r.width(), r.height()));
            }
        }
        if let Some(a) = self.area {
            kv("win_area", &format!("{:.0},{:.0}", a.width(), a.height()));
        }
        self.tools_save(&mut s);
        self.presets_save(&mut s);
        s
    }

    fn load_settings(&mut self) {
        let Ok(text) = std::fs::read_to_string(settings_path()) else { return };
        let m: std::collections::HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        fn get<T: std::str::FromStr>(m: &std::collections::HashMap<&str, &str>, k: &str) -> Option<T> {
            m.get(k).and_then(|v| v.trim().parse().ok())
        }
        if let Some(id) = m.get("device") {
            if let Some(i) = self.devices.iter().position(|d| d.id == *id) {
                self.sel_dev = i;
            }
        }
        if let Some(v) = get::<usize>(&m, "pair") {
            self.pair = v.min(15);
        }
        let p = &mut self.p;
        if let Some(v) = get::<usize>(&m, "fft") {
            if v.is_power_of_two() && (1024..=262144).contains(&v) {
                p.fft_size = v;
            }
        }
        if let Some(w) = m.get("window").and_then(|n| Window::ALL.iter().find(|w| w.name() == *n)) {
            p.window = *w;
        }
        if let Some(v) = get::<f32>(&m, "update_hz") {
            p.update_hz = v.clamp(10.0, 240.0);
        }
        if let Some(v) = get::<f32>(&m, "avg_count") {
            p.avg_count = v.clamp(1.0, 512.0);
        }
        if let Some(v) = get::<f32>(&m, "cols_per_sec") {
            p.cols_per_sec = v.clamp(2.0, 240.0);
        }
        if let Some(v) = get::<u8>(&m, "spectro_src") {
            p.spectro_src = v.min(2);
        }
        if let Some(v) = get(&m, "log_freq") {
            p.log_freq = v;
        }
        if let Some(v) = get::<f32>(&m, "bw_lo") {
            p.bw_lo = v.clamp(1.0, 2000.0);
        }
        if let Some(v) = get::<f32>(&m, "bw_hi") {
            p.bw_hi = v.clamp(100.0, 96000.0);
        }
        if let Some(v) = get::<usize>(&m, "harmonics") {
            p.harmonics = v.clamp(2, MAXH);
        }
        if let Some(v) = get(&m, "manual_fund") {
            p.manual_fund = v;
        }
        if let Some(v) = get::<f32>(&m, "fund_hz") {
            p.fund_hz = v.clamp(10.0, 96000.0);
        }
        macro_rules! set {
            ($field:expr, $key:expr) => {
                if let Some(v) = get(&m, $key) {
                    $field = v;
                }
            };
        }
        set!(self.show_live, "show_live");
        set!(self.show_avg, "show_avg");
        set!(self.show_max, "show_max");
        set!(self.show_min, "show_min");
        set!(self.show_ch[0], "show_l");
        set!(self.show_ch[1], "show_r");
        set!(self.fill, "fill");
        set!(self.d3.wire, "d3_wire");
        set!(self.stereo.auto, "st_auto");
        set!(self.stereo.hold, "st_hold");
        set!(self.rotated, "rotated");
        set!(self.auto_tile, "auto_tile");
        set!(self.snap_peak, "snap_peak");
        if let Some(v) = get::<f32>(&m, "line_w") {
            self.line_w = v.clamp(0.5, 3.0);
        }
        if let Some(c) = m.get("cmap").and_then(|n| Cmap::ALL.iter().find(|c| c.name() == *n)) {
            self.cmap = *c;
            self.lut = c.lut();
        }
        if let Some(v) = get::<f32>(&m, "col_lo") {
            self.col_lo = v.clamp(-160.0, -20.0);
        }
        if let Some(v) = get::<f32>(&m, "col_hi") {
            self.col_hi = v.clamp(-80.0, 10.0).max(self.col_lo + 10.0);
        }
        if let Some(v) = get::<usize>(&m, "d3_rows") {
            self.d3.rows = v.clamp(32, crate::view3d::D3_MAX);
        }
        if let Some(v) = get::<usize>(&m, "d3_cols") {
            self.d3.cols = v.clamp(64, 1024);
        }
        if let Some(v) = get::<f32>(&m, "d3_rate") {
            self.d3.rate = v.clamp(2.0, 60.0);
        }
        if let Some(v) = get::<f32>(&m, "d3_hscale") {
            self.d3.hscale = v.clamp(0.2, 2.0);
        }
        if let Some(v) = get::<f32>(&m, "st_gain") {
            self.stereo.gain_db = v.clamp(-20.0, 60.0);
        }
        if let Some(v) = get::<f32>(&m, "st_bright") {
            self.stereo.brightness = v.clamp(0.1, 8.0);
        }
        if let Some(v) = get::<f32>(&m, "st_persist") {
            self.stereo.persist_ms = v.clamp(5.0, 5000.0);
        }
        if let Some(v) = get::<usize>(&m, "meas_ch") {
            self.meas_ch = v.min(1);
        }
        if let Some(v) = get::<f32>(&m, "readout_hz") {
            self.readout_hz = v.clamp(0.5, 10.0);
        }
        if let Some(v) = get::<f32>(&m, "fps_cap") {
            self.fps_cap = v.clamp(15.0, 241.0);
        }
        if let Some(v) = get::<usize>(&m, "snap_src") {
            self.snap_src = v.min(3);
        }
        set!(self.cal.on, "cal_on");
        for (k, c) in [("cal_vpk_l", 0usize), ("cal_vpk_r", 1)] {
            if let Some(v) = get::<f32>(&m, k) {
                if v.is_finite() && v > 1e-9 {
                    self.cal.vpk[c] = v;
                }
            }
        }
        if let Some(v) = get::<f32>(&m, "cal_ref") {
            self.cal.ref_vrms = v.clamp(1e-6, 1000.0);
        }
        if let Some(v) = get::<u8>(&m, "cal_method") {
            self.cal.method = v.min(1);
        }
        if let Some(v) = get::<u8>(&m, "cal_entry_unit") {
            self.cal.entry_unit = v.min(3);
        }
        set!(self.cal.entry_val, "cal_entry_val");
        if let Some(v) = get::<u8>(&m, "unit") {
            self.unit = Unit::from_index(v);
        }
        if !self.cal.on {
            self.unit = Unit::Dbfs;
        }
        set!(self.scope.rising, "sc_rising");
        set!(self.scope.hf_reject, "sc_hf");
        if let Some(v) = get::<f32>(&m, "sc_hf_hz") {
            self.scope.hf_hz = v.clamp(100.0, 96000.0);
        }
        set!(self.scope.show[0], "sc_l");
        set!(self.scope.show[1], "sc_r");
        set!(self.scope.level, "sc_level");
        set!(self.scope.v_off, "sc_v_off");
        if let Some(v) = get::<f32>(&m, "sc_t_div") {
            self.scope.t_div = v.clamp(1e-6, 20.0);
        }
        if let Some(v) = get::<f32>(&m, "sc_v_div") {
            if v.is_finite() && v > 0.0 {
                self.scope.v_div = v;
            }
        }
        if let Some(v) = get::<u8>(&m, "sc_mode") {
            self.scope.mode = v.min(3);
        }
        if let Some(v) = get::<f32>(&m, "sc_pos") {
            // 0.3 was the old default: treat it as "never changed" so the new 50 % default applies
            if (v - 0.3).abs() > 1e-4 {
                self.scope.pos = v.clamp(0.02, 0.98);
            }
        }
        if let Some(v) = get::<f32>(&m, "sc_hyst") {
            self.scope.hyst = v.clamp(0.0, 20.0);
        }
        if let Some(v) = get::<usize>(&m, "sc_src") {
            self.scope.trig_src = v.min(1);
        }
        // restore the spectrum axis range for the saved unit
        let off = self.unit_off(0);
        self.db_lo += off;
        self.db_hi += off;
        for i in 0..WIN_COUNT {
            if let Some(o) = get::<bool>(&m, &format!("w{i}_open")) {
                self.wins[i].open = o;
            }
            if let Some(r) = m.get(format!("w{i}_rect").as_str()) {
                let v: Vec<f32> = r.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() == 4 && v[2] > 100.0 && v[3] > 80.0 {
                    self.wins[i].rect = Some(egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3])));
                }
            }
        }
        self.tools_load(&m);
        self.presets_load(&m);
        // layouts from older versions stored absolute positions without the area: discard them
        self.saved_area = m.get("win_area").and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (v.len() == 2 && v[0] > 200.0 && v[1] > 150.0).then(|| egui::vec2(v[0], v[1]))
        });
        if self.saved_area.is_none() {
            for w in self.wins.iter_mut() {
                w.rect = None;
            }
        }
        self.last_saved = self.settings_string();
    }

    fn save_settings(&mut self, force: bool) {
        // do not overwrite a saved layout before it has been applied
        if !self.layout_ready {
            return;
        }
        let s = self.settings_string();
        if !force && s == self.last_saved {
            return;
        }
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&path, &s).is_ok() {
            self.last_saved = s;
        }
    }

    /// Enumerate devices on a worker thread: while a driver is reconfiguring, enumeration can block for
    /// seconds, and that must never freeze the UI.
    pub fn request_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let slot = self.scan_slot.clone();
        std::thread::Builder::new()
            .name("scan".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let r = (audio::list_devices(), crate::tonegen::list_output_devices());
                crate::log::log(&format!("scan: {} inputs, {} outputs in {} ms", r.0.len(), r.1.len(), t0.elapsed().as_millis()));
                *slot.lock() = Some(r);
            })
            .ok();
    }

    pub fn poll_scan(&mut self) {
        let done = self.scan_slot.try_lock().and_then(|mut g| g.take());
        if let Some((inputs, outputs)) = done {
            let cur_in = self.devices.get(self.sel_dev).map(|d| d.id.clone());
            let cur_out = self.gen_devs.get(self.gen_sel).map(|d| d.id.clone());
            self.devices = inputs;
            self.gen_devs = outputs;
            self.sel_dev = cur_in.and_then(|id| self.devices.iter().position(|d| d.id == id)).unwrap_or(0);
            self.gen_sel = cur_out.and_then(|id| self.gen_devs.iter().position(|d| d.id == id)).unwrap_or(0);
            self.scanning = false;
        }
    }

    pub fn start_selected(&mut self) {
        crate::log::log("ui: (re)start capture requested");
        self.audio = None;
        if let Some(d) = self.devices.get(self.sel_dev) {
            self.sh.pair.store(self.pair, std::sync::atomic::Ordering::Relaxed);
            self.audio = Some(audio::start(self.sh.clone(), d.id.clone()));
        } else {
            *self.sh.status.lock() = "No audio devices found".into();
        }
    }

    pub fn rebuild_dyn_lut(&mut self) {
        let span = (self.col_hi - self.col_lo).max(1.0);
        for v in 0..256 {
            let db = v as f32 / 255.0 * HIST_DB_RANGE - HIST_DB_RANGE;
            let t = ((db - self.col_lo) / span).clamp(0.0, 1.0);
            self.dyn_lut[v] = self.lut[(t * 255.0) as usize];
        }
        self.recolor = true;
    }

    fn clear_history(&mut self) {
        self.sg_hist.fill(0);
        self.wf_hist.fill(0);
        self.sg_head = 0;
        self.wf_head = 0;
        self.sg_back = 0;
        self.wf_back = 0;
        self.d3.clear();
        self.recolor = true;
    }

    fn refresh_textures(&mut self) {
        let mut px = vec![self.dyn_lut[0]; SG_W * ROWS];
        for x in 0..SG_W {
            let col = &self.sg_hist[x * ROWS..(x + 1) * ROWS];
            for y in 0..ROWS {
                px[y * SG_W + x] = self.dyn_lut[col[y] as usize];
            }
        }
        self.sg_tex.set(ColorImage::new([SG_W, ROWS], px), TextureOptions::LINEAR);
        let px: Vec<Color32> = self.wf_hist.iter().map(|&v| self.dyn_lut[v as usize]).collect();
        self.wf_tex.set(ColorImage::new([ROWS, WF_H], px), TextureOptions::LINEAR);
        self.recolor = false;
        self.last_recolor = Instant::now();
    }

    fn pull_analysis(&mut self) {
        // never block the UI thread on the DSP thread: skip this frame if it is publishing
        let Some(a) = self.sh.analysis.try_lock() else { return };
        if a.seq == self.an_seq {
            return;
        }
        self.an_seq = a.seq;
        let d = &mut self.an;
        d.seq = a.seq;
        d.epoch = a.epoch;
        d.sr = a.sr;
        d.n = a.n;
        for c in 0..2 {
            d.live[c].clone_from(&a.live[c]);
            d.avg[c].clone_from(&a.avg[c]);
            d.max[c].clone_from(&a.max[c]);
            d.min[c].clone_from(&a.min[c]);
        }
        d.corr.clone_from(&a.corr);
        d.metrics = a.metrics;
        d.meas_n = a.meas_n;
        d.scallop_db = a.scallop_db;
        d.peak_tab.clone_from(&a.peak_tab);
        d.rms_db = a.rms_db;
        d.peak_db = a.peak_db;
        d.dc = a.dc;
        d.enbw_bins = a.enbw_bins;
        d.coherent_gain = a.coherent_gain;
        d.dsp_ms = a.dsp_ms;
        d.dsp_fps = a.dsp_fps;
    }

    fn ingest_columns(&mut self) {
        let cols = std::mem::take(&mut *self.sh.columns.lock());
        for (ep, col) in cols {
            if ep != self.epoch {
                if ep > self.epoch {
                    self.clear_history();
                    self.epoch = ep;
                } else {
                    continue;
                }
            }
            if self.recolor {
                // Textures are about to be rebuilt wholesale; just record history.
            }
            let x = self.sg_head;
            for r in 0..ROWS {
                self.sg_hist[x * ROWS + (ROWS - 1 - r)] = col[r];
            }
            if !self.recolor {
                let px: Vec<Color32> = self.sg_hist[x * ROWS..(x + 1) * ROWS].iter().map(|&v| self.dyn_lut[v as usize]).collect();
                self.sg_tex.set_partial([x, 0], ColorImage::new([1, ROWS], px), TextureOptions::LINEAR);
            }
            self.sg_head = (x + 1) % SG_W;
            if self.sg_back > 0 {
                self.sg_back = (self.sg_back + 1).min(SG_W - self.sg_span);
            }

            let y = self.wf_head;
            self.wf_hist[y * ROWS..(y + 1) * ROWS].copy_from_slice(&col);
            if !self.recolor {
                let px: Vec<Color32> = col.iter().map(|&v| self.dyn_lut[v as usize]).collect();
                self.wf_tex.set_partial([0, y], ColorImage::new([ROWS, 1], px), TextureOptions::LINEAR);
            }
            self.wf_head = (y + 1) % WF_H;
            if self.wf_back > 0 {
                self.wf_back = (self.wf_back + 1).min(WF_H - self.wf_span);
            }
            self.d3.push(&col, self.p.cols_per_sec.min(self.p.update_hz));
        }
    }

    pub fn nyq(&self) -> f32 {
        if self.an.sr > 0.0 { self.an.sr * 0.5 } else { 24000.0 }
    }

    pub fn sr(&self) -> f32 {
        if self.an.sr > 0.0 { self.an.sr } else { 48000.0 }
    }

    pub fn u_to_f(&self, u: f32) -> f32 {
        u_to_freq(u, self.p.log_freq, self.sr())
    }

    pub fn f_to_u(&self, f: f32) -> f32 {
        freq_to_u(f, self.p.log_freq, self.sr())
    }

    /// Offset (dB) from dBFS to the selected display unit for a channel.
    pub fn unit_off(&self, ch: usize) -> f32 {
        if !self.cal.on {
            return 0.0;
        }
        let dbv = 20.0 * (self.cal.vpk[ch] / std::f32::consts::SQRT_2).max(1e-12).log10();
        match self.unit {
            Unit::Dbfs => 0.0,
            Unit::Dbv => dbv,
            Unit::Dbu => dbv - 20.0 * DBU_REF_V.log10(),
        }
    }

    pub fn unit_name(&self) -> &'static str {
        if self.cal.on { self.unit.name() } else { "dBFS" }
    }

    /// Run a calibration/unit change and keep the scope and spectrum views visually stable.
    pub fn recal(&mut self, f: impl FnOnce(&mut App)) {
        let (old_scale, old_off) = (self.cal.scale(0).max(1e-12), self.unit_off(0));
        f(self);
        if !self.cal.on {
            self.unit = Unit::Dbfs;
        }
        let ratio = self.cal.scale(0) / old_scale;
        self.scope.v_div *= ratio;
        self.scope.v_off *= ratio;
        self.scope.level *= ratio;
        let d = self.unit_off(0) - old_off;
        self.db_lo += d;
        self.db_hi += d;
    }

    /// Level (dBFS) of the displayed spectrum at frequency `f`.
    pub fn level_at(&self, f: f32, ch: usize) -> Option<f32> {
        let data = if self.show_live || !self.show_avg { &self.an.live[ch] } else { &self.an.avg[ch] };
        if data.len() < 2 || self.an.n == 0 {
            return None;
        }
        let b = f / (self.an.sr / self.an.n as f32);
        if b < 0.0 || b > (data.len() - 1) as f32 {
            return None;
        }
        let kc = (b.round() as usize).clamp(1, data.len() - 2);
        // On a peak, use the window's main-lobe shape to undo scalloping loss, so a 0 dBFS tone reads
        // 0 dBFS wherever it falls between bins. Elsewhere: 3-point parabola in dB.
        if self.snap_peak && (b - kc as f32).abs() <= 0.5 {
            if let Some(v) = dsp::corrected_peak_db(data, kc, &self.an.peak_tab) {
                return Some(v);
            }
        }
        let (a, m, c) = (data[kc - 1], data[kc], data[kc + 1]);
        let t = b - kc as f32;
        Some(m + 0.5 * t * (c - a) + 0.5 * t * t * (a - 2.0 * m + c))
    }

    pub fn snap_freq(&self, f: f32, tol_hz: f32) -> f32 {
        let ch = self.meas_ch;
        let d = &self.an.live[ch];
        if d.len() < 4 || self.an.n == 0 {
            return f;
        }
        let bin_hz = self.an.sr / self.an.n as f32;
        let kc = (f / bin_hz).round() as isize;
        let w = (tol_hz / bin_hz).ceil().max(2.0) as isize;
        let (mut kb, mut vb) = (kc.clamp(1, d.len() as isize - 2), f32::MIN);
        for k in (kc - w).max(1)..=(kc + w).min(d.len() as isize - 2) {
            if d[k as usize] > vb {
                vb = d[k as usize];
                kb = k;
            }
        }
        let k = kb as usize;
        let (a, b, c) = (d[k - 1], d[k], d[k + 1]);
        let den = a - 2.0 * b + c;
        let off: f32 = if den.abs() > 1e-9 { 0.5 * (a - c) / den } else { 0.0 };
        (kb as f32 + off.clamp(-0.5, 0.5)) * bin_hz
    }

    pub fn take_snapshot(&mut self) {
        if self.an.n == 0 {
            return;
        }
        let ch = self.meas_ch;
        let (data, label) = match self.snap_src {
            0 => (self.an.live[ch].clone(), "Live"),
            1 => (self.an.avg[ch].clone(), "Avg"),
            2 => (self.an.max[ch].clone(), "Max"),
            _ => (self.an.min[ch].clone(), "Min"),
        };
        const COLORS: [Color32; 6] = [
            Color32::from_rgb(255, 214, 10),
            Color32::from_rgb(120, 255, 120),
            Color32::from_rgb(255, 140, 60),
            Color32::from_rgb(190, 130, 255),
            Color32::from_rgb(255, 255, 255),
            Color32::from_rgb(90, 170, 255),
        ];
        let id = self.snaps.len() + 1;
        self.snaps.push(Snapshot {
            name: format!("#{id} {} {}", label, if ch == 0 { "L" } else { "R" }),
            color: COLORS[(id - 1) % COLORS.len()],
            data,
            sr: self.an.sr,
            n: self.an.n,
            visible: true,
            ch,
        });
    }

    fn export_csv(&mut self) {
        let a = &self.an;
        if a.n == 0 {
            return;
        }
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = format!("visuscope_spectrum_{ts}.csv");
        let mut s = String::from("freq_hz,live_L,live_R,avg_L,avg_R,max_L,max_R,min_L,min_R");
        for sn in &self.snaps {
            s.push(',');
            s.push_str(&sn.name.replace(',', " "));
        }
        s.push('\n');
        let bin = a.sr / a.n as f32;
        for k in 0..a.live[0].len() {
            use std::fmt::Write;
            let _ = write!(
                s,
                "{:.3},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}",
                k as f32 * bin,
                a.live[0][k], a.live[1][k], a.avg[0][k], a.avg[1][k], a.max[0][k], a.max[1][k], a.min[0][k], a.min[1][k]
            );
            for sn in &self.snaps {
                let _ = write!(s, ",{:.2}", sn.data.get(k).copied().unwrap_or(f32::NAN));
            }
            s.push('\n');
        }
        self.note = match std::fs::write(&path, s) {
            Ok(_) => format!("Saved {path}"),
            Err(e) => format!("CSV error: {e}"),
        };
    }

    // ---------------------------------------------------------------- UI pieces

    fn set_vpk(&mut self, mask: [bool; 2], vpk: f32) {
        self.recal(|a| {
            if !a.cal.on {
                a.cal.vpk = [vpk, vpk];
            }
            for c in 0..2 {
                if mask[c] {
                    a.cal.vpk[c] = vpk;
                }
            }
            a.cal.on = true;
        });
    }

    /// Peak volts at full scale implied by the reference voltage and what the channel currently measures.
    fn vpk_from_measurement(&self, ch: usize) -> Option<f32> {
        let r = self.cal.ref_vrms;
        if self.cal.method == 0 {
            let m = self.an.metrics[ch];
            if !m.valid {
                return None;
            }
            Some(r * std::f32::consts::SQRT_2 / 10f32.powf(m.fund_dbfs / 20.0))
        } else {
            let rms = 10f32.powf(self.an.rms_db[ch] / 20.0);
            if rms < 1e-6 { None } else { Some(r / rms) }
        }
    }

    pub fn calibration_ui(&mut self, ui: &mut Ui) {
        ui.heading("Voltage calibration");
        if self.cal.on {
            for c in 0..2 {
                let vpk = self.cal.vpk[c];
                let vrms = vpk / std::f32::consts::SQRT_2;
                let dbv = 20.0 * vrms.log10();
                let dbu = dbv - 20.0 * DBU_REF_V.log10();
                ui.label(RichText::new(format!("{}: 0 dBFS = {} pk = {} rms ({dbv:+.2} dBV, {dbu:+.2} dBu)", ["L", "R"][c], fmt_volts(vpk), fmt_volts(vrms))).small());
            }
        } else {
            ui.label(RichText::new("Not calibrated - showing dBFS / full-scale units").small().weak());
        }
        ui.horizontal(|ui| {
            ui.label("Display unit");
            let mut u = self.unit;
            ui.add_enabled_ui(self.cal.on, |ui| {
                egui::ComboBox::from_id_salt("unit").width(70.0).selected_text(self.unit_name()).show_ui(ui, |ui| {
                    for x in Unit::ALL {
                        ui.selectable_value(&mut u, x, x.name());
                    }
                });
            });
            if u != self.unit {
                self.recal(|a| a.unit = u);
            }
        });

        ui.label(RichText::new("Know your converter's full scale? 0 dBFS equals:").small());
        let mut apply: Option<[bool; 2]> = None;
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut self.cal.entry_val).speed(0.01));
            egui::ComboBox::from_id_salt("entryunit").width(60.0).selected_text(["Vpk", "Vrms", "dBV", "dBu"][self.cal.entry_unit as usize]).show_ui(ui, |ui| {
                for (i, n) in ["Vpk", "Vrms", "dBV", "dBu"].iter().enumerate() {
                    ui.selectable_value(&mut self.cal.entry_unit, i as u8, *n);
                }
            });
            if ui.small_button("L").clicked() {
                apply = Some([true, false]);
            }
            if ui.small_button("R").clicked() {
                apply = Some([false, true]);
            }
            if ui.small_button("Both").clicked() {
                apply = Some([true, true]);
            }
        });
        if let Some(mask) = apply {
            let v = self.cal.entry_to_vpk();
            if v.is_finite() && v > 1e-9 {
                self.set_vpk(mask, v);
            }
        }

        ui.label(RichText::new("Or apply a known sine / signal and calibrate:").small());
        let mut cal_mask: Option<[bool; 2]> = None;
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut self.cal.ref_vrms).speed(0.005).range(1e-6..=1000.0).suffix(" Vrms"));
            egui::ComboBox::from_id_salt("calmethod").width(78.0).selected_text(["Tone", "Wideband"][self.cal.method as usize]).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.cal.method, 0, "Tone (fundamental)");
                ui.selectable_value(&mut self.cal.method, 1, "Wideband (RMS)");
            });
        });
        ui.horizontal(|ui| {
            if ui.button("Cal L").clicked() {
                cal_mask = Some([true, false]);
            }
            if ui.button("Cal R").clicked() {
                cal_mask = Some([false, true]);
            }
            if ui.button("Cal both").clicked() {
                cal_mask = Some([true, true]);
            }
            if ui.button("Reset").clicked() {
                self.recal(|a| a.cal.on = false);
                self.note = "Calibration cleared".into();
            }
        });
        if let Some(mask) = cal_mask {
            // average over the selected channels
            let vals: Vec<f32> = (0..2).filter(|&c| mask[c]).filter_map(|c| self.vpk_from_measurement(c).map(|v| (c, v)).map(|(_, v)| v)).collect();
            if vals.is_empty() {
                self.note = "Calibration failed: no usable signal (apply the reference first)".into();
            } else {
                for c in 0..2 {
                    if mask[c] {
                        if let Some(v) = self.vpk_from_measurement(c) {
                            self.set_vpk([c == 0, c == 1], v);
                        }
                    }
                }
                self.note = "Calibrated from measurement".into();
            }
        }
        ui.label(RichText::new("Tone mode uses the detected fundamental (sine). Wideband uses the time-domain RMS (any waveform).").small().weak());
    }

    pub fn measurements_ui(&mut self, ui: &mut Ui) {
        let m = self.sm_metrics[self.meas_ch];
        let ch = self.meas_ch;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.readout_hz, 0.5..=10.0).text("readout Hz"));
            ui.checkbox(&mut self.readout_hold, "Hold");
        });
        ui.heading(format!("Measurements ({})", if ch == 0 { "Left" } else { "Right" }));
        if !m.valid {
            ui.label(RichText::new("No tone detected (need a signal above -110 dBFS)").weak());
        }
        let col = if m.valid { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
        let big = |t: String| RichText::new(t).size(20.0).strong().color(col);
        egui::Grid::new("meas").num_columns(2).spacing([14.0, 2.0]).show(ui, |ui| {
            ui.label("Fundamental");
            ui.label(RichText::new(format!("{} @ {:.1} dBFS", fmt_hz(m.fund_hz), m.fund_dbfs)).color(col));
            ui.end_row();
            ui.label("THD+N");
            ui.label(big(format!("{:.2} dB  ({})", m.thdn_db, fmt_pct(m.thdn_db))));
            ui.end_row();
            ui.label("THD");
            ui.label(RichText::new(format!("{:.2} dB  ({})", m.thd_db, fmt_pct(m.thd_db))).color(col));
            ui.end_row();
            ui.label("SINAD");
            ui.label(RichText::new(format!("{:.2} dB", m.sinad_db)).color(col));
            ui.end_row();
            ui.label("SNR");
            ui.label(RichText::new(format!("{:.2} dB", m.snr_db)).color(col));
            ui.end_row();
            ui.label("ENOB");
            ui.label(big(format!("{:.2} bits", m.enob)));
            ui.end_row();
            ui.label("ENOB (FS-ref)");
            ui.label(RichText::new(format!("{:.2} bits", m.enob_fs)).color(col));
            ui.end_row();
            ui.label("SFDR");
            ui.label(RichText::new(format!("{:.1} dBc", m.sfdr_db)).color(col));
            ui.end_row();
            ui.label("Noise floor");
            ui.label(RichText::new(format!("{:.1} dBFS/bin", m.noise_floor_dbfs)).color(col));
            ui.end_row();
            ui.label("RMS / Peak");
            ui.label(format!("{:.1} / {:.1} dBFS", self.sm_rms[ch], self.sm_peak[ch]));
            ui.end_row();
            if self.cal.on {
                let vs = self.cal.vpk[ch];
                let a = 10f32.powf(m.fund_dbfs / 20.0) * vs;
                ui.label("Fund (voltage)");
                ui.label(RichText::new(format!("{} rms  ({} pk)", fmt_volts(a / std::f32::consts::SQRT_2), fmt_volts(a))).color(col));
                ui.end_row();
                ui.label("RMS / Peak (V)");
                ui.label(format!("{} / {}", fmt_volts(10f32.powf(self.sm_rms[ch] / 20.0) * vs), fmt_volts(10f32.powf(self.sm_peak[ch] / 20.0) * vs)));
                ui.end_row();
            }
            ui.label("Crest / DC");
            ui.label(format!("{:.1} dB / {:+.5}", self.sm_peak[ch] - self.sm_rms[ch] - 3.01, self.sm_dc[ch]));
            ui.end_row();
        });
        let bar = |ui: &mut Ui, label: &str, v: f32, max: f32| {
            ui.add(egui::ProgressBar::new((v / max).clamp(0.0, 1.0)).desired_width(ui.available_width() - 4.0).text(label));
        };
        bar(ui, &format!("ENOB {:.1} / 24 bit", m.enob.max(0.0)), if m.valid { m.enob } else { 0.0 }, 24.0);
        bar(ui, &format!("THD+N {:.0} dB", m.thdn_db), if m.valid { -m.thdn_db } else { 0.0 }, 144.0);

        ui.add_space(2.0);
        ui.collapsing("Harmonics (dBc)", |ui| {
            egui::Grid::new("harm").num_columns(3).show(ui, |ui| {
                for h in 2..=self.p.harmonics.min(MAXH) {
                    let v = m.harm_db[h - 1];
                    ui.label(format!("H{h}"));
                    ui.label(fmt_hz(m.harm_hz[h - 1]));
                    ui.label(if v > -199.0 { format!("{v:.1}") } else { "-".into() });
                    ui.end_row();
                }
            });
        });
    }

    pub fn cursors_ui(&mut self, ui: &mut Ui) {
        ui.heading("Cursors");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.snap_peak, "Snap to peak");
            if ui.button("Clear").clicked() {
                self.cur_a = None;
                self.cur_b = None;
            }
        });
        ui.label(RichText::new("Click = A, right-click = B (spectrum / spectrogram)").small().weak());
        let line = |ui: &mut Ui, name: &str, c: Color32, f: Option<f32>, app: &App| {
            if let Some(f) = f {
                let l = app.level_at(f, 0).map(|v| format!("{:.1}", v + app.unit_off(0))).unwrap_or("-".into());
                let r = app.level_at(f, 1).map(|v| format!("{:.1}", v + app.unit_off(1))).unwrap_or("-".into());
                ui.label(RichText::new(format!("{name}: {}   L {l}  R {r} {}", fmt_hz(f), app.unit_name())).color(c));
            }
        };
        line(ui, "A", CUR_A, self.cur_a, self);
        line(ui, "B", CUR_B, self.cur_b, self);
        if let (Some(a), Some(b)) = (self.cur_a, self.cur_b) {
            let df = b - a;
            let mut s = format!("dF {}   ratio {:.4}   T {:.3} ms", fmt_hz(df.abs()), b / a.max(1e-6), 1000.0 / df.abs().max(1e-6));
            if let (Some(la), Some(lb)) = (self.level_at(a, self.meas_ch), self.level_at(b, self.meas_ch)) {
                s.push_str(&format!("   dL {:+.2} dB", lb - la));
            }
            ui.label(s);
        }
    }

    pub fn snapshots_ui(&mut self, ui: &mut Ui) {
        ui.heading("Snapshots");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("snapsrc").width(70.0).selected_text(["Live", "Avg", "Max", "Min"][self.snap_src]).show_ui(ui, |ui| {
                for (i, n) in ["Live", "Avg", "Max", "Min"].iter().enumerate() {
                    ui.selectable_value(&mut self.snap_src, i, *n);
                }
            });
            if ui.button("Capture").clicked() {
                self.take_snapshot();
            }
            if ui.button("Export CSV").clicked() {
                self.export_csv();
            }
        });
        let mut del = None;
        for (i, s) in self.snaps.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut s.visible, RichText::new(&s.name).color(s.color));
                if ui.small_button("x").clicked() {
                    del = Some(i);
                }
            });
        }
        if let Some(i) = del {
            self.snaps.remove(i);
        }
    }

    fn handle_screenshot(&mut self) {
        let mut n = self.shot_note.lock();
        if !n.is_empty() {
            self.note = std::mem::take(&mut *n);
        }
    }

    pub fn zoom_f(&mut self, factor: f32) {
        let c = 0.5 * (self.f_lo + self.f_hi);
        zoom_view(&mut self.f_lo, &mut self.f_hi, factor, c, 0.002);
    }

    pub fn pan_f(&mut self, frac: f32) {
        let w = self.f_hi - self.f_lo;
        pan_view(&mut self.f_lo, &mut self.f_hi, frac * w);
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings(true);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t_frame = Instant::now();
        let beat = |app: &App, phase: u32| {
            app.sh.ui_phase.store(phase, std::sync::atomic::Ordering::Relaxed);
            app.sh.ui_beat.store(app.sh.t0.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
        };
        beat(self, 1);

        // first frames: open at 75% of the monitor, centred
        if !self.sized {
            self.size_frames += 1;
            let ms = ctx.input(|i| i.viewport().monitor_size);
            if self.prof {
                let (inner, ppp) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().native_pixels_per_point));
                eprintln!("sizing frame {}: monitor_size {ms:?} inner {inner:?} ppp {ppp:?}", self.size_frames);
            }
            // On Windows main() already created the window at 75% of the screen, centred.
            let presized = crate::PRESIZED.load(std::sync::atomic::Ordering::Relaxed);
            if presized || ms.is_some() || self.size_frames > 20 {
                if !presized {
                    if let Some(ms) = ms {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(ms * 0.75));
                        // centring must wait until the new size has been applied
                        self.center_at = Some(self.size_frames + 6);
                    }
                }
                self.sized = true;
                // windows are laid out only after the area has reached its final size
                self.settle = if presized { 3 } else { 10 };
            }
        }

        if let Some(at) = self.center_at {
            self.size_frames += 1;
            if self.size_frames >= at {
                self.center_at = None;
                if let Some(c) = egui::ViewportCommand::center_on_screen(&ctx) {
                    ctx.send_viewport_cmd(c);
                }
            }
        }
        if self.p != self.p_sent {
            *self.sh.params.lock() = self.p.clone();
            self.p_sent = self.p.clone();
        }
        if self.tg != self.gen_sent {
            *self.gen_shared.lock() = self.tg.clone();
            self.gen_sent = self.tg.clone();
        }
        beat(self, 2);
        self.pull_analysis();
        beat(self, 3);
        self.ingest_columns();
        beat(self, 4);
        if self.recolor && self.last_recolor.elapsed().as_millis() > 120 {
            self.refresh_textures();
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
        self.stereo.update(&self.sh, dt);
        beat(self, 5);
        if !self.readout_hold && self.sm_t.elapsed().as_secs_f32() >= 1.0 / self.readout_hz {
            self.sm_t = Instant::now();
            self.sm_metrics = self.an.metrics;
            self.sm_rms = self.an.rms_db;
            self.sm_peak = self.an.peak_db;
            self.sm_dc = self.an.dc;
            let s = &self.stereo;
            self.sm_st = [s.corr_f, s.corr_s, s.bal_db, s.mid_db, s.side_db];
        }
        self.handle_screenshot();
        self.poll_scan();
        self.tools_update();
        self.handle_hotkeys(&ctx);

        beat(self, 6);
        self.top_bar_ui(ui);
        self.status_bar_ui(ui);
        egui::Panel::left("controls").resizable(true).default_size(290.0).min_size(220.0).show(ui, |ui| self.controls_ui(ui));
        beat(self, 7);
        egui::CentralPanel::default().show(ui, |ui| {
            let area = ui.available_rect_before_wrap();
            if self.sized {
                self.show_windows(&ctx, area);
            }
        });

        beat(self, 8);
        self.ms[WIN_COUNT] += 0.1 * (t_frame.elapsed().as_secs_f32() * 1000.0 - self.ms[WIN_COUNT]);
        if self.last_save_t.elapsed().as_secs_f32() > 2.0 {
            self.last_save_t = Instant::now();
            self.save_settings(false);
        }
        self.frames += 1;
        if self.prof_t.elapsed().as_secs_f32() > 1.0 {
            self.ui_fps = self.frames as f32 / self.prof_t.elapsed().as_secs_f32();
            self.frames = 0;
            self.prof_t = Instant::now();
            if self.prof {
                eprintln!(
                    "ui fps {:.0} | ms: spectrum {:.2} spectrogram {:.2} stereo {:.2} 3d {:.2} scope {:.2} frame {:.2} | dsp {:.2} ms @ {:.0} fps",
                    self.ui_fps, self.ms[0], self.ms[1], self.ms[2], self.ms[3], self.ms[4], self.ms[WIN_COUNT], self.an.dsp_ms, self.an.dsp_fps
                );
            }
        }
        if self.fps_cap >= 241.0 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(1.0 / self.fps_cap));
        }
    }
}

pub const CH_COLORS: [Color32; 2] = [Color32::from_rgb(70, 200, 255), Color32::from_rgb(255, 100, 160)];
pub const CUR_A: Color32 = Color32::from_rgb(255, 230, 80);
pub const CUR_B: Color32 = Color32::from_rgb(120, 255, 140);

pub fn fmt_hz(f: f32) -> String {
    if f >= 10000.0 {
        format!("{:.2} kHz", f / 1000.0)
    } else if f >= 1000.0 {
        format!("{:.3} kHz", f / 1000.0)
    } else {
        format!("{f:.2} Hz")
    }
}

pub fn fmt_hz_short(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 0.001 { format!("{}k", k.round() as i32) } else { format!("{k:.1}k") }
    } else if (f - f.round()).abs() < 0.001 {
        format!("{}", f.round() as i32)
    } else {
        format!("{f:.1}")
    }
}

fn fmt_pct(db: f32) -> String {
    let p = 100.0 * 10f32.powf(db / 20.0);
    if p >= 0.1 { format!("{p:.3} %") } else if p >= 0.0001 { format!("{p:.5} %") } else { format!("{:.2e} %", p) }
}

/// Zoom a normalised [lo,hi] window about `anchor`.
pub fn zoom_view(lo: &mut f32, hi: &mut f32, factor: f32, anchor: f32, min_w: f32) {
    let w = *hi - *lo;
    let nw = (w * factor).clamp(min_w, 1.0);
    let t = ((anchor - *lo) / w.max(1e-9)).clamp(0.0, 1.0);
    let nlo = (anchor - t * nw).clamp(0.0, 1.0 - nw);
    *lo = nlo;
    *hi = nlo + nw;
}

pub fn pan_view(lo: &mut f32, hi: &mut f32, delta: f32) {
    let w = *hi - *lo;
    let nlo = (*lo + delta).clamp(0.0, 1.0 - w);
    *lo = nlo;
    *hi = nlo + w;
}
