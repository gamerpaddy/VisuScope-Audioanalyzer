//! Frequency-response window: sweep controls plus magnitude / phase / group delay / coherence.

use super::plot::{canvas, inset, Plot};
use super::sweep::{State, SweepResult};
use super::*;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke};

const SMOOTH: [(f32, &str); 5] = [(0.0, "off"), (24.0, "1/24 oct"), (12.0, "1/12 oct"), (6.0, "1/6 oct"), (3.0, "1/3 oct")];
const LOWER: [&str; 7] = ["Phase", "Group delay", "Coherence", "None", "Minimum phase", "Excess phase", "Excess delay"];
const LOG_N: usize = 900;

struct Curve {
    f: Vec<f32>,
    /// unsmoothed magnitude, what "Store" keeps
    raw: Vec<f32>,
    mag: Vec<f32>,
    phase_deg: Vec<f32>,
    gd_ms: Vec<f32>,
    coh: Option<Vec<f32>>,
    /// the phase a minimum-phase system with this magnitude has, what is left of the measured phase, and the delay of that rest
    min_deg: Vec<f32>,
    exc_deg: Vec<f32>,
    exc_gd_ms: Vec<f32>,
}

pub struct Response {
    smooth: usize,
    lower: usize,
    y_lo: f32,
    y_hi: f32,
    normalize: bool,
    /// show the level (the gain plus the level of the sweep) instead of the gain
    dbfs: bool,
    curve: Option<Curve>,
    key: (u64, usize, u32),
    /// cut the impulse response after this time (ms) and work the curves out again, without a new sweep
    gate_on: bool,
    gate_ms: f32,
    /// the positions added to the spatial average: unsmoothed magnitude and phase on the grid `spat_f`
    spat: Vec<(Vec<f32>, Vec<f32>)>,
    spat_f: Vec<f32>,
    spat_vec: bool,
    /// the last measurements (time, frequencies, magnitude, phase), kept without asking, newest last
    hist: Vec<(String, Vec<f32>, Vec<f32>, Vec<f32>)>,
    hist_id: u64,
    /// level on the input when Measure was pressed (dBFS RMS): the noise the sweep has to rise above
    pre_rms: f32,
    /// run a short sweep first and only measure when its level is right
    check_first: bool,
    /// the settings of the real sweep while the short one runs, and the id of the result before it
    burst: Option<(super::sweep::SweepParams, u64)>,
    burst_note: String,
    held: Option<(String, Vec<f32>, Vec<f32>)>,
    save: Option<dialogs::Pending>,
}

/// 1/N-octave smoothing of a power quantity sampled on a log grid.
pub fn smooth_log(v: &[f32], pts_per_oct: f32, n: f32) -> Vec<f32> {
    if n <= 0.0 {
        return v.to_vec();
    }
    let half = (pts_per_oct / n * 0.5).max(0.0);
    let mut pre = vec![0f64; v.len() + 1];
    for (i, x) in v.iter().enumerate() {
        pre[i + 1] = pre[i] + 10f64.powf(*x as f64 / 10.0);
    }
    (0..v.len())
        .map(|i| {
            let a = (i as f32 - half).floor().max(0.0) as usize;
            let b = ((i as f32 + half).ceil() as usize + 1).min(v.len());
            (10.0 * ((pre[b] - pre[a]) / (b - a).max(1) as f64).max(1e-30).log10()) as f32
        })
        .collect()
}

/// The response from the stored impulse response cut `gate_ms` after its peak (half a Hann window over the last quarter): what
/// arrives later (reflections of the room) is left out. Nothing below 1 / gate is resolved.
pub fn gated_transfer(r: &SweepResult, gate_ms: f32) -> super::sweep::Transfer {
    use rustfft::num_complex::Complex32;
    let len = (r.pre + (gate_ms * 1e-3 * r.sr) as usize).clamp(16.min(r.ir.len()), r.ir.len());
    let m = (len * 2).next_power_of_two().max(16384);
    let t0 = r.pre.min(len) + (len - r.pre.min(len)) * 3 / 4;
    let mut buf: Vec<Complex32> = (0..m)
        .map(|k| {
            let w = if k < t0 { 1.0 } else { 0.5 * (1.0 + (std::f32::consts::PI * (k - t0) as f32 / (len - t0).max(1) as f32).cos()) };
            Complex32::new(if k < len { r.ir[k] * w } else { 0.0 }, 0.0)
        })
        .collect();
    rustfft::FftPlanner::new().plan_fft_forward(m).process(&mut buf);
    let df = r.sr / m as f32;
    let (mut mag_db, mut phase) = (Vec::with_capacity(m / 2 + 1), Vec::with_capacity(m / 2 + 1));
    let mut prev = 0f32;
    for (k, v) in buf.iter().take(m / 2 + 1).enumerate() {
        // the peak to t = 0
        let v = v * Complex32::from_polar(1.0, std::f32::consts::TAU * ((k * r.pre) % m) as f32 / m as f32);
        mag_db.push(20.0 * v.norm().max(1e-12).log10());
        let d = (v.arg() - prev + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        prev += if k == 0 { v.arg() } else { d };
        phase.push(prev);
    }
    let (f0, f1) = r.params.span(r.sr);
    super::sweep::anchor_phase(&mut phase, ((f0 * f1).sqrt() / df) as usize);
    super::sweep::Transfer { df, mag_db, phase, coh: None }
}

/// The phase (radians) of the minimum-phase system with the magnitude `mag_db` (bins 0 to Nyquist), by the Hilbert transform
/// of the log magnitude. Outside the bins `lo..=hi` (what the sweep covered) the magnitude is held at the value of the edge.
pub fn min_phase(mag_db: &[f32], lo: usize, hi: usize) -> Vec<f32> {
    use rustfft::num_complex::Complex32;
    let n = mag_db.len();
    if n < 4 {
        return vec![0.0; n];
    }
    let lo = lo.min(n - 1);
    let hi = hi.clamp(lo, n - 1);
    let m = 2 * (n - 1);
    let ln = |k: usize| mag_db[k.clamp(lo, hi)] * (std::f32::consts::LN_10 / 20.0);
    let mut c: Vec<Complex32> = (0..m).map(|k| Complex32::new(ln(if k < n { k } else { m - k }), 0.0)).collect();
    let mut pl = rustfft::FftPlanner::new();
    pl.plan_fft_inverse(m).process(&mut c);
    for (k, v) in c.iter_mut().enumerate() {
        *v *= if k == 0 || k == m / 2 { 1.0 } else if k < m / 2 { 2.0 } else { 0.0 } / m as f32;
    }
    pl.plan_fft_forward(m).process(&mut c);
    c.iter().take(n).map(|v| v.im).collect()
}

/// Frequencies, unsmoothed magnitude (dB) and phase (degrees) of a sweep result, as the Store button keeps them.
pub fn curve_of(r: &SweepResult) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let c = Response::make_curve(r, 0, 0.0);
    (c.f, c.raw, c.phase_deg)
}

impl Response {
    pub fn new() -> Self {
        Self { smooth: 3, lower: 0, y_lo: -40.0, y_hi: 20.0, normalize: false, dbfs: false, curve: None, key: (0, usize::MAX, 0), gate_on: false, gate_ms: 5.0, spat: Vec::new(), spat_f: Vec::new(), spat_vec: false, hist: Vec::new(), hist_id: 0, pre_rms: f32::NAN, check_first: true, burst: None, burst_note: String::new(), held: None, save: None }
    }

    fn build(&mut self, r: &SweepResult) {
        self.curve = Some(Self::make_curve(r, self.smooth, self.gate()));
        self.key = (r.id, self.smooth, self.gate().to_bits());
    }

    fn gate(&self) -> f32 {
        if self.gate_on { self.gate_ms } else { 0.0 }
    }

    fn make_curve(r: &SweepResult, smooth: usize, gate_ms: f32) -> Curve {
        let gated = (gate_ms > 0.0).then(|| gated_transfer(r, gate_ms));
        let h = gated.as_ref().unwrap_or(&r.h);
        let nyq = r.sr * 0.5;
        // only what the sweep covered: outside it there is nothing but the noise of the deconvolution (the curve is cut at the
        // start and the end frequency of the sweep, in the magnitude and in the phase, delay and coherence under it)
        let f0 = r.params.f0.max(h.df * 2.0).max(1.0);
        let f1 = nyq.min(r.params.f1).max(f0 * 2.0);
        let ppo = LOG_N as f32 / (f1 / f0).log2();
        let at = |data: &[f32], f: f32| -> f32 {
            let b = f / h.df;
            let k = (b.floor() as usize).min(data.len() - 2);
            let t = b - k as f32;
            data[k] * (1.0 - t) + data[k + 1] * t
        };
        let f: Vec<f32> = (0..LOG_N).map(|i| f0 * (f1 / f0).powf(i as f32 / (LOG_N - 1) as f32)).collect();
        let mag_raw: Vec<f32> = f.iter().map(|&x| at(&h.mag_db, x)).collect();
        let mag = smooth_log(&mag_raw, ppo, SMOOTH[smooth].0);
        // group delay from the unwrapped phase of the full-resolution response
        let gd_full: Vec<f32> = (1..h.phase.len() - 1).map(|k| -(h.phase[k + 1] - h.phase[k - 1]) / (2.0 * std::f32::consts::TAU * h.df) * 1000.0).collect();
        let gd_raw: Vec<f32> = f.iter().map(|&x| at(&gd_full, (x - h.df).max(0.0))).collect();
        let gd_ms = if SMOOTH[smooth].0 > 0.0 {
            // smooth linearly (delay is not a power)
            let half = (ppo / SMOOTH[smooth].0 * 0.5) as usize;
            (0..LOG_N).map(|i| gd_raw[i.saturating_sub(half)..(i + half + 1).min(LOG_N)].iter().sum::<f32>() / (i + half + 1).min(LOG_N).saturating_sub(i.saturating_sub(half)).max(1) as f32).collect()
        } else {
            gd_raw
        };
        let phase_deg = f.iter().map(|&x| at(&h.phase, x).to_degrees()).collect();
        let coh = h.coh.as_ref().map(|c| f.iter().map(|&x| at(c, x)).collect());
        let minp = min_phase(&h.mag_db, (f0 / h.df) as usize, (f1 / h.df) as usize);
        let exc: Vec<f32> = h.phase.iter().zip(&minp).map(|(a, b)| a - b).collect();
        let exc_gd_full: Vec<f32> = (1..exc.len() - 1).map(|k| -(exc[k + 1] - exc[k - 1]) / (2.0 * std::f32::consts::TAU * h.df) * 1000.0).collect();
        let min_deg = f.iter().map(|&x| at(&minp, x).to_degrees()).collect();
        let exc_deg = f.iter().map(|&x| at(&exc, x).to_degrees()).collect();
        let exc_gd_ms = f.iter().map(|&x| at(&exc_gd_full, (x - h.df).max(0.0))).collect();
        Curve { f, raw: mag_raw, mag, phase_deg, gd_ms, coh, min_deg, exc_deg, exc_gd_ms }
    }

    fn controls(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let sw = &mut *cx.sweep;
        let busy = sw.busy();
        ui.horizontal(|ui| {
            let b = egui::Button::new(RichText::new(if busy { "Stop" } else { "Measure" }).strong()).min_size(egui::vec2(96.0, 26.0));
            if ui.add(b).clicked() {
                if busy {
                    sw.cancel();
                } else {
                    self.pre_rms = cx.an.rms_db[sw.p.dut_ch.min(1)];
                    self.burst_note.clear();
                    if self.check_first {
                        self.burst = Some((sw.p.clone(), sw.result.as_ref().map_or(0, |r| r.id)));
                        (sw.p.secs, sw.p.tail, sw.p.averages) = (0.5, sw.p.tail.min(0.6), 1);
                    }
                    sw.start();
                }
            }
            if busy {
                ui.add(egui::ProgressBar::new(sw.progress()).desired_width(120.0).show_percentage());
            }
            if self.burst.is_some() {
                ui.label(RichText::new(crate::lang::tr("level check...")).small().weak());
            }
            ui.checkbox(&mut self.check_first, crate::lang::tr("Check level first")).on_hover_text(crate::lang::tr("Play a short sweep (half a second) at the same level first. The measurement only starts when that neither clipped nor came back too quiet; otherwise it says what is wrong"));
            if !self.burst_note.is_empty() {
                ui.colored_label(Color32::from_rgb(255, 130, 100), &self.burst_note);
            }
            match &sw.state {
                State::Arming(_) => {
                    ui.label(RichText::new(crate::lang::tr("starting output...")).small().weak());
                }
                State::Computing { .. } => {
                    ui.label(RichText::new(crate::lang::tr("deconvolving...")).small().weak());
                }
                State::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(255, 130, 100), e);
                }
                _ => {}
            }
        });
        egui::CollapsingHeader::new(crate::lang::tr("Sweep settings")).default_open(false).show(ui, |ui| {
            let p = &mut sw.p;
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("sweep_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(crate::lang::tr("Level"));
                    ui.add(egui::Slider::new(&mut p.amp_db, -60.0..=0.0).suffix(crate::lang::tr(" dBFS")));
                    ui.end_row();
                    ui.label(crate::lang::tr("Range"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut p.full, crate::lang::tr("Full")).on_hover_text(crate::lang::tr("From 5 Hz to the top of the sample rate (the graph rate with the routing graph: 96 kHz at 192 kHz). Outside the sweep nothing is measured: the curve falls off there."));
                        ui.add_enabled_ui(!p.full, |ui| {
                            // (the end stays above the start: a sweep that does not rise cannot be analysed)
                            let f1 = p.f1;
                            ui.add(egui::DragValue::new(&mut p.f0).range(5.0..=(f1 / 1.2).clamp(5.0, 2000.0)).speed(1.0).suffix(crate::lang::tr(" Hz")));
                            ui.label(crate::lang::tr("to"));
                            ui.add(egui::DragValue::new(&mut p.f1).range((p.f0 * 1.2).max(1000.0)..=96000.0).speed(10.0).suffix(crate::lang::tr(" Hz")));
                        });
                    });
                    ui.end_row();
                    ui.label(crate::lang::tr("Length"));
                    ui.add(egui::Slider::new(&mut p.secs, 0.5..=60.0).logarithmic(true).suffix(" s")).on_hover_text(crate::lang::tr("A longer sweep lowers the noise and gives the low frequencies more time. The limit is the memory of the analysis: at 192 kHz about 40 s"));
                    ui.end_row();
                    ui.label(crate::lang::tr("Tail"));
                    ui.add(egui::Slider::new(&mut p.tail, 0.3..=10.0).suffix(" s")).on_hover_text(crate::lang::tr("Recording time after the sweep ends: must cover latency and the decay of the room"));
                    ui.end_row();
                    ui.label(crate::lang::tr("Averages"));
                    ui.add(egui::Slider::new(&mut p.averages, 1..=16)).on_hover_text(crate::lang::tr("More runs lower the noise; 2 or more also give the coherence"));
                    ui.end_row();
                    ui.label(crate::lang::tr("Response window"));
                    ui.add(egui::Slider::new(&mut p.resp_ms, 20.0..=2000.0).logarithmic(true).suffix(crate::lang::tr(" ms"))).on_hover_text(crate::lang::tr("Short = gated (ignores late reflections), long = more resolution"));
                    ui.end_row();
                    ui.label(crate::lang::tr("IR length"));
                    ui.add(egui::Slider::new(&mut p.ir_ms, 200.0..=4000.0).suffix(crate::lang::tr(" ms")));
                    ui.end_row();
                    ui.label(crate::lang::tr("Reference"));
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut p.loopback, false, crate::lang::tr("ideal")).on_hover_text(crate::lang::tr("Assume a perfect sweep: the result includes the sound card"));
                        ui.selectable_value(&mut p.loopback, true, crate::lang::tr("R input")).on_hover_text(crate::lang::tr("L is the device under test, R carries the reference (loopback of the output): the sound card cancels"));
                    });
                    ui.end_row();
                    if !p.loopback {
                        ui.label(crate::lang::tr("Input channel"));
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut p.dut_ch, 0, "L");
                            ui.selectable_value(&mut p.dut_ch, 1, "R");
                        });
                        ui.end_row();
                    }
                });
            });
        });
        let _ = cx.gen_running;
    }
}

impl Tool for Response {
    fn update(&mut self, cx: &mut Ctx, _visible: bool) {
        // the short sweep of the level check: back to the real settings, and on with the measurement when the level is right
        if let Some((p, old)) = self.burst.clone() {
            let new = cx.sweep.result.as_ref().filter(|r| r.id != old).map(|r| (r.id, r.capture_peak_db));
            if let Some((id, peak)) = new {
                self.burst = None;
                self.hist_id = id;
                cx.sweep.p = p;
                if peak > -0.3 {
                    self.burst_note = format!("Level check: the input clips (peak {peak:.1} dBFS). Lower the sweep level or the input gain. Not measured.");
                } else if peak < -60.0 {
                    self.burst_note = format!("Level check: hardly any signal arrives (peak {peak:.1} dBFS). Check the cable, the input channel and the output. Not measured.");
                } else {
                    cx.sweep.start();
                }
            } else if !cx.sweep.busy() {
                // stopped or failed
                self.burst = None;
                cx.sweep.p = p;
            }
        }
        // every finished sweep goes into the history, whichever window started it
        if let Some(r) = cx.sweep.result.as_ref().filter(|r| r.id != self.hist_id) {
            self.hist_id = r.id;
            let (f, m, p) = curve_of(r);
            let t = utc_stamp(now_unix()).1;
            self.hist.push((format!("{} UTC, {:.0} dBFS, {} run(s)", t.get(11..19).unwrap_or(""), r.params.amp_db, r.runs), f, m, p));
            if self.hist.len() > 12 {
                self.hist.remove(0);
            }
        }
        if let Some(d) = &self.save
            && let Some(res) = d.take() {
                self.save = None;
                if let (Some(path), Some(c)) = (res, &self.curve) {
                    let mut s = String::from("freq_hz,mag_db,phase_deg,group_delay_ms,coherence\n");
                    for i in 0..c.f.len() {
                        s += &format!("{:.3},{:.3},{:.2},{:.4},{}\n", c.f[i], c.mag[i], c.phase_deg[i], c.gd_ms[i], c.coh.as_ref().map(|v| format!("{:.4}", v[i])).unwrap_or_default());
                    }
                    cx.req.note = Some(match crate::fsutil::atomic_write(&path, s) {
                        Ok(_) => format!("Saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    });
                }
            }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        self.controls(ui, cx);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(crate::lang::tr("Smooth")).small().weak());
            egui::ComboBox::from_id_salt("rsp_sm").width(78.0).selected_text(crate::lang::tr(SMOOTH[self.smooth].1)).show_ui(ui, |ui| {
                for (i, s) in SMOOTH.iter().enumerate() {
                    ui.selectable_value(&mut self.smooth, i, s.1);
                }
            });
            ui.label(RichText::new(crate::lang::tr("Below")).small().weak());
            egui::ComboBox::from_id_salt("rsp_lo").width(92.0).selected_text(crate::lang::tr(LOWER[self.lower])).show_ui(ui, |ui| {
                for (i, s) in LOWER.iter().enumerate() {
                    ui.selectable_value(&mut self.lower, i, crate::lang::tr(s));
                }
            });
            ui.checkbox(&mut self.normalize, crate::lang::tr("Normalise")).on_hover_text(crate::lang::tr("Shift the magnitude so the 1 kHz value reads 0 dB"));
            ui.checkbox(&mut self.dbfs, crate::lang::tr("dBFS")).on_hover_text(crate::lang::tr("Show the level in dBFS (the gain plus the level of the sweep: a loopback of a -12 dBFS sweep reads -12) instead of the gain in dB (a loopback reads 0)"));
            if crate::mapping::btn(ui, "Hold").on_hover_text(crate::lang::tr("Keep the current magnitude trace as a comparison")).clicked() {
                self.held = self.curve.as_ref().map(|c| ("held".to_string(), c.f.clone(), c.mag.clone()));
            }
            if self.held.is_some() && crate::mapping::small_btn(ui, "Clear hold").clicked() {
                self.held = None;
            }
            if !self.hist.is_empty() {
                ui.menu_button(format!("History ({})", self.hist.len()), |ui| {
                    ui.label(RichText::new(crate::lang::tr("The last sweeps are kept here without storing them (until the program closes).")).small().weak());
                    for k in (0..self.hist.len()).rev() {
                        ui.horizontal(|ui| {
                            ui.label(&self.hist[k].0);
                            if ui.small_button(crate::lang::tr("compare")).on_hover_text(crate::lang::tr("Show it as the grey held curve")).clicked() {
                                self.held = Some((self.hist[k].0.clone(), self.hist[k].1.clone(), smooth_log(&self.hist[k].2, LOG_N as f32 / (self.hist[k].1[LOG_N - 1] / self.hist[k].1[0]).log2(), SMOOTH[self.smooth].0)));
                            }
                            if ui.small_button(crate::lang::tr("to traces")).on_hover_text(crate::lang::tr("Store it as a named trace")).clicked() {
                                let h = self.hist[k].clone();
                                cx.lib.add(Some(h.0), h.1, h.2, h.3);
                            }
                        });
                    }
                });
            }
            if ui.add_enabled(self.curve.is_some() && self.save.is_none(), egui::Button::new(crate::lang::tr("CSV"))).on_hover_text(crate::lang::tr("Export magnitude, phase, delay and coherence")).clicked() {
                self.save = Some(dialogs::save("Export response", "CSV", &["csv"], "response.csv".into()));
            }
            ui.checkbox(&mut self.gate_on, crate::lang::tr("Gate")).on_hover_text(crate::lang::tr("Cut the measured impulse response after this time and work the curves out again, without a new sweep: reflections that arrive later are left out (quasi-anechoic). The Response window of the sweep settings does the same while measuring, down to 20 ms; this gate goes down to 0.5 ms and can be changed afterwards. With the R input as reference the gate works on the impulse response of the L input alone"));
            ui.add_enabled(self.gate_on, egui::DragValue::new(&mut self.gate_ms).range(0.5..=2000.0).speed(0.05).suffix(crate::lang::tr(" ms")));
            ui.add(egui::DragValue::new(&mut self.y_lo).range(-200.0..=self.y_hi - 5.0).speed(0.5).prefix(crate::lang::tr("min ")));
            ui.add(egui::DragValue::new(&mut self.y_hi).range(self.y_lo + 5.0..=100.0).speed(0.5).prefix(crate::lang::tr("max ")));
        });

        cx.lib.ui(ui, self.curve.as_ref().map(|c| (c.f.clone(), c.raw.clone(), c.phase_deg.clone())));
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(crate::lang::tr("Spatial average")).small().weak()).on_hover_text(crate::lang::tr("Measure at several microphone positions and add each one: the average of the positions is stored as one trace, a better base for an equaliser than one position. (Average in the Traces window does the same, by power, for traces that are stored already.)"));
            if ui.add_enabled(self.curve.is_some(), egui::Button::new(crate::lang::tr("Add position"))).clicked()
                && let Some(c) = &self.curve {
                    if self.spat_f.len() != c.f.len() || self.spat_f.first() != c.f.first() || self.spat_f.last() != c.f.last() {
                        self.spat.clear();
                        self.spat_f = c.f.clone();
                    }
                    self.spat.push((c.raw.clone(), c.phase_deg.clone()));
                }
            ui.label(format!("{}", self.spat.len()));
            ui.selectable_value(&mut self.spat_vec, false, crate::lang::tr("power")).on_hover_text(crate::lang::tr("Average of the powers: the phase is ignored"));
            ui.selectable_value(&mut self.spat_vec, true, crate::lang::tr("vector")).on_hover_text(crate::lang::tr("Average with the phase: what does not arrive in phase at all positions cancels"));
            if ui.add_enabled(self.spat.len() >= 2, egui::Button::new(crate::lang::tr("Store average"))).clicked() {
                let (mag, ph) = spatial_average(&self.spat, self.spat_vec);
                cx.lib.add(Some(format!("Spatial average of {}", self.spat.len())), self.spat_f.clone(), mag, ph);
            }
            if !self.spat.is_empty() && crate::mapping::small_btn(ui, "Clear").clicked() {
                self.spat.clear();
            }
        });
        // rebuild the display curve if there is a new result, a new smoothing or a new gate
        if let Some(r) = &cx.sweep.result
            && self.key != (r.id, self.smooth, self.gate().to_bits()) {
                self.build(r);
            }
        // the gate on the impulse response: drag the line
        if let (true, Some(r)) = (self.gate_on, &cx.sweep.result) {
            let (resp, painter, rect) = super::plot::canvas_h(ui, 62.0);
            let pr = egui::Rect::from_min_max(rect.min + egui::vec2(44.0, 4.0), rect.max - egui::vec2(8.0, 14.0));
            let pre_ms = r.pre as f32 / r.sr * 1000.0;
            let span = (self.gate_ms * 2.5).clamp(5.0, r.ir.len() as f32 / r.sr * 1000.0 - pre_ms);
            let plot = Plot::new(pr, -pre_ms.min(span * 0.1), span, -1.05, 1.05, false);
            plot.grid_x(&painter, &|v| format!("{v:.1} ms"));
            painter.rect_stroke(pr, 0.0, Stroke::new(1.0, crate::views::GRID_MAJOR), egui::StrokeKind::Inside);
            let pk = r.ir.iter().fold(1e-12f32, |a, b| a.max(b.abs()));
            let (k0, k1) = ((((plot.x0 + pre_ms) * 1e-3 * r.sr) as usize).min(r.ir.len()), (((span + pre_ms) * 1e-3 * r.sr) as usize).min(r.ir.len()));
            let step = ((k1 - k0) / (pr.width().max(1.0) as usize * 2)).max(1);
            // (the strongest sample of every step, with its sign)
            plot.line(&painter, (k0..k1).step_by(step).map(|k| (k as f32 / r.sr * 1000.0 - pre_ms, r.ir[k..(k + step).min(k1)].iter().copied().fold(0.0f32, |a, b| if b.abs() > a.abs() { b } else { a }) / pk)), Stroke::new(1.0, Color32::from_rgb(90, 200, 255)));
            let x = plot.tx(self.gate_ms);
            painter.rect_filled(egui::Rect::from_min_max(Pos2::new(x.min(pr.right()), pr.top()), pr.max), 0.0, Color32::from_black_alpha(120));
            painter.line_segment([Pos2::new(x, pr.top()), Pos2::new(x, pr.bottom())], Stroke::new(2.0, Color32::from_rgb(255, 214, 10)));
            painter.text(pr.left_top() + egui::vec2(4.0, 2.0), Align2::LEFT_TOP, "impulse response: drag the yellow line to the first reflection", FontId::proportional(9.5), crate::views::TXT);
            if let Some(h) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
                self.gate_ms = plot.fx(h.x).clamp(0.5, 2000.0);
            }
        }
        let Some(c) = &self.curve else {
            ui.add_space(8.0);
            ui.label(RichText::new(crate::lang::tr("Press Measure. The generator plays a log sweep while the input records it.\nUse a cable or a [Loopback] input; set Reference to \"R input\" to cancel the sound card.")).weak());
            return;
        };
        // level check: a sweep that clipped or hardly rose above the noise is not worth reading
        if let Some(r) = &cx.sweep.result {
            let snr = r.capture_peak_db - 3.0 - self.pre_rms;
            let warn = if r.capture_peak_db > -0.3 {
                Some(format!("The input clipped during the sweep (peak {:.1} dBFS): lower the sweep level or the input gain and measure again.", r.capture_peak_db))
            } else if r.capture_peak_db < -60.0 {
                Some(format!("Hardly any signal arrived (peak {:.1} dBFS): check the cable, the input channel and the output.", r.capture_peak_db))
            } else if self.pre_rms.is_finite() && snr < 30.0 {
                Some(format!("The sweep was only about {snr:.0} dB above the noise that was on the input before it: raise the level, or use a longer sweep or averages."))
            } else {
                None
            };
            if let Some(w) = warn {
                ui.colored_label(Color32::from_rgb(255, 150, 90), w);
            }
        }
        let ppo = LOG_N as f32 / (c.f[c.f.len() - 1] / c.f[0]).log2();
        let (layers, rel) = cx.lib.layers(&c.f, ppo, SMOOTH[self.smooth].0);
        let rel_v = rel.clone();
        let norm = if self.normalize { -c.f.iter().zip(&c.mag).min_by(|a, b| (a.0 - 1000.0).abs().total_cmp(&(b.0 - 1000.0).abs())).map(|x| *x.1).unwrap_or(0.0) } else { 0.0 };
        // the level of the sweep, when the scale is dBFS (not for the difference to a reference trace: that is a gain)
        let lvl = if self.dbfs && rel_v.is_none() { cx.sweep.result.as_ref().map(|r| r.params.amp_db).unwrap_or(0.0) } else { 0.0 };
        let norm = norm + lvl;
        let has_lower = self.lower != 3 && !(self.lower == 2 && c.coh.is_none());

        let (resp, painter, rect) = canvas(ui, 140.0);
        let split = if has_lower { rect.height() * 0.68 } else { rect.height() };
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split));
        let pr = inset(top);
        if pr.width() < 40.0 || pr.height() < 30.0 {
            return;
        }
        let (fa, fb) = (c.f[0], *c.f.last().unwrap());
        let plot = Plot::new(pr, fa.max(1.0), fb, self.y_lo, self.y_hi, true);
        plot.grid_y(&painter, &|v| format!("{v:.0}"));
        plot.grid_x(&painter, &|f| crate::app::fmt_hz_short(f));
        plot.frame(&painter);
        if let Some((_, f, m)) = &self.held {
            plot.line(&painter, f.iter().copied().zip(m.iter().map(|v| v + lvl)), Stroke::new(1.0, Color32::from_rgb(150, 150, 160)));
        }
        for (_, col, v) in &layers {
            plot.line(&painter, c.f.iter().copied().zip(v.iter().copied()), Stroke::new(1.2, *col));
        }
        let rel_at = |i: usize| rel_v.as_ref().map(|r| r[i]).unwrap_or(0.0);
        plot.line(&painter, c.f.iter().copied().zip(c.mag.iter().enumerate().map(|(i, m)| m - rel_at(i) + norm)), Stroke::new(1.6, Color32::from_rgb(90, 200, 255)));
        for (i, (name, col, _)) in layers.iter().enumerate() {
            painter.text(Pos2::new(pr.right() - 6.0, pr.bottom() - 6.0 - i as f32 * 12.0), Align2::RIGHT_BOTTOM, name, FontId::proportional(10.0), *col);
        }
        painter.text(Pos2::new(rect.left() + 2.0, pr.top()), Align2::LEFT_TOP, if self.dbfs { "dBFS" } else { "dB" }, FontId::proportional(9.0), crate::views::TXT);

        let mut lower_plot: Option<Plot> = None;
        if has_lower {
            let low = egui::Rect::from_min_max(Pos2::new(rect.left(), rect.top() + split), rect.max);
            let lp = inset(low);
            let (ys, lo, hi, unit): (Box<dyn Iterator<Item = f32>>, f32, f32, &str) = match self.lower {
                0 => (Box::new(c.phase_deg.iter().copied()), -360.0, 360.0, "deg"),
                1 => {
                    let m = c.gd_ms.iter().fold(0f32, |a, b| a.max(b.abs())).max(0.5).min(200.0);
                    (Box::new(c.gd_ms.iter().copied()), -m * 0.25, m * 1.1, "ms")
                }
                2 => (Box::new(c.coh.as_ref().unwrap().iter().copied()), 0.0, 1.05, "coh"),
                4 => (Box::new(c.min_deg.iter().copied()), -360.0, 360.0, "deg"),
                5 => {
                    let m = c.exc_deg.iter().fold(0f32, |a, b| a.max(b.abs())).clamp(90.0, 3600.0);
                    (Box::new(c.exc_deg.iter().copied()), -m, m, "deg")
                }
                _ => {
                    let m = c.exc_gd_ms.iter().fold(0f32, |a, b| a.max(b.abs())).max(0.5).min(200.0);
                    (Box::new(c.exc_gd_ms.iter().copied()), -m * 0.25, m * 1.1, "ms")
                }
            };
            let p2 = Plot::new(lp, plot.x0, plot.x1, lo, hi, true);
            p2.grid_y(&painter, &|v| format!("{v:.1}").trim_end_matches(".0").to_string());
            p2.grid_x(&painter, &|_| String::new());
            p2.frame(&painter);
            p2.line(&painter, c.f.iter().copied().zip(ys), Stroke::new(1.4, [Color32::from_rgb(255, 170, 70), Color32::from_rgb(120, 230, 150), Color32::from_rgb(230, 130, 230), Color32::WHITE, Color32::from_rgb(255, 220, 120), Color32::from_rgb(255, 130, 110), Color32::from_rgb(120, 230, 220)][self.lower]));
            painter.text(Pos2::new(rect.left() + 2.0, lp.top()), Align2::LEFT_TOP, unit, FontId::proportional(9.0), crate::views::TXT);
            lower_plot = Some(p2);
        }
        // hover read-out
        if let Some(h) = resp.hover_pos().filter(|p| p.x >= pr.left() && p.x <= pr.right() && p.y >= rect.top()) {
            let f = plot.fx(h.x);
            let i = c.f.partition_point(|&x| x < f).min(c.f.len() - 1);
            let mut txt = format!("{}   {:.2} {}", crate::app::fmt_hz(f), c.mag[i] + norm, if self.dbfs { "dBFS" } else { "dB" });
            if self.lower == 0 {
                txt += &format!("   {:.0}°", c.phase_deg[i]);
            }
            if self.lower == 1 {
                txt += &format!("   {:.3} ms", c.gd_ms[i]);
            }
            match self.lower {
                4 => txt += &format!("   min {:.0}°", c.min_deg[i]),
                5 => txt += &format!("   excess {:.0}°", c.exc_deg[i]),
                6 => txt += &format!("   excess {:.3} ms", c.exc_gd_ms[i]),
                _ => {}
            }
            if let (2, Some(co)) = (self.lower, &c.coh) {
                txt += &format!("   coh {:.3}", co[i]);
            }
            painter.line_segment([Pos2::new(h.x, pr.top()), Pos2::new(h.x, rect.bottom() - 18.0)], Stroke::new(1.0, Color32::from_white_alpha(50)));
            plot.label(&painter, Pos2::new(pr.right() - 4.0, pr.top() + 4.0), txt, Color32::WHITE, Align2::RIGHT_TOP);
        }
        let _ = lower_plot;
        if self.gate_on {
            // nothing below 1 / gate is resolved
            let fv = 1000.0 / self.gate_ms;
            let x = plot.tx(fv).clamp(pr.left(), pr.right());
            painter.rect_filled(egui::Rect::from_min_max(pr.min, Pos2::new(x, pr.bottom())), 0.0, Color32::from_black_alpha(110));
            painter.text(Pos2::new(x + 4.0, pr.bottom() - 4.0), Align2::LEFT_BOTTOM, format!("gate {} ms: valid above {}", self.gate_ms, crate::app::fmt_hz(fv)), FontId::proportional(9.5), Color32::from_rgb(255, 214, 10));
        }
        if let Some(r) = &cx.sweep.result {
            painter.text(Pos2::new(pr.left() + 6.0, rect.bottom() - 20.0), Align2::LEFT_BOTTOM, format!("{} run(s), input peak {:.1} dBFS, {} ms window", r.runs, r.capture_peak_db, r.params.resp_ms as i32), FontId::proportional(9.5), Color32::from_white_alpha(90));
        }
    }

    fn report(&self, cx: &Ctx) -> Vec<crate::report::Section> {
        use crate::report::*;
        let Some(r) = &cx.sweep.result else { return Vec::new() };
        let c = Self::make_curve(r, self.smooth, self.gate());
        let ser = |name: String, col: usize, v: &[f32]| Series { name, color: COLORS[col], pts: c.f.iter().copied().zip(v.iter().copied()).collect() };
        let x = (c.f[0], *c.f.last().unwrap());
        let mag = vec![ser(format!("magnitude, smoothing {}", SMOOTH[self.smooth].1), 0, &c.mag)];
        let y = auto_range(&mag, 5.0, 20.0);
        let mut blocks = vec![
            Block::Text(format!("Exponential sweep {:.0} Hz to {:.0} Hz, {:.1} s, {:.0} dBFS, {} run(s), input peak {:.1} dBFS, response window {:.0} ms.", r.params.f0, r.params.f1, r.params.secs, r.params.amp_db, r.runs, r.capture_peak_db, r.params.resp_ms)),
            Block::Plot(PlotSpec { title: "Magnitude".into(), xlog: true, xlabel: "Hz".into(), ylabel: "dB".into(), x, y, series: mag }),
        ];
        let mut pts: Vec<(f32, f32)> = Vec::new();
        let mut prev = f32::NAN;
        for (i, p) in c.phase_deg.iter().enumerate() {
            let w = (p + 180.0).rem_euclid(360.0) - 180.0;
            if prev.is_finite() && (w - prev).abs() > 180.0 {
                pts.push((f32::NAN, f32::NAN));
            }
            pts.push((c.f[i], w));
            prev = w;
        }
        blocks.push(Block::Plot(PlotSpec { title: "Phase".into(), xlog: true, xlabel: "Hz".into(), ylabel: "degrees".into(), x, y: (-180.0, 180.0), series: vec![Series { name: String::new(), color: COLORS[1], pts }] }));
        if let Some(co) = &c.coh {
            blocks.push(Block::Plot(PlotSpec { title: "Coherence".into(), xlog: true, xlabel: "Hz".into(), ylabel: String::new(), x, y: (0.0, 1.0), series: vec![ser(String::new(), 3, co)] }));
        }
        vec![Section { id: "Response", title: "Frequency response".into(), blocks }]
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("smooth", self.smooth);
        o.put("lower", self.lower);
        o.put("y_lo", self.y_lo);
        o.put("y_hi", self.y_hi);
        o.put("norm", self.normalize);
        o.put("dbfs", self.dbfs);
        o.put("gate_on", self.gate_on);
        o.put("gate_ms", self.gate_ms);
        o.put("check_first", self.check_first);
    }

    fn load(&mut self, i: &SettingsIn) {
        if let Some(v) = i.get::<usize>("smooth") {
            self.smooth = v.min(SMOOTH.len() - 1);
        }
        if let Some(v) = i.get::<usize>("lower") {
            self.lower = v.min(LOWER.len() - 1);
        }
        i.set("y_lo", &mut self.y_lo);
        i.set("y_hi", &mut self.y_hi);
        i.set("norm", &mut self.normalize);
        i.set("dbfs", &mut self.dbfs);
        i.set("gate_on", &mut self.gate_on);
        i.set("gate_ms", &mut self.gate_ms);
        i.set("check_first", &mut self.check_first);
        self.gate_ms = self.gate_ms.clamp(0.5, 2000.0);
        self.y_hi = self.y_hi.clamp(-50.0, 100.0);
        self.y_lo = self.y_lo.clamp(-200.0, self.y_hi - 5.0);
    }
}


/// Average of several positions: by power (no phase) or as vectors (magnitude in dB, phase in degrees).
pub fn spatial_average(pos: &[(Vec<f32>, Vec<f32>)], vector: bool) -> (Vec<f32>, Vec<f32>) {
    use rustfft::num_complex::Complex32;
    let n = pos.first().map_or(0, |p| p.0.len());
    let k = pos.len().max(1) as f32;
    if vector {
        let v: Vec<Complex32> = (0..n).map(|i| pos.iter().map(|p| Complex32::from_polar(10f32.powf(p.0[i] / 20.0), p.1.get(i).copied().unwrap_or(0.0).to_radians())).sum::<Complex32>() / k).collect();
        (v.iter().map(|c| 20.0 * c.norm().max(1e-12).log10()).collect(), v.iter().map(|c| c.arg().to_degrees()).collect())
    } else {
        ((0..n).map(|i| 10.0 * (pos.iter().map(|p| 10f32.powf(p.0[i] / 10.0)).sum::<f32>() / k).max(1e-30).log10()).collect(), Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::num_complex::Complex32;

    #[test]
    fn minimum_phase_of_a_one_pole_low_pass_and_the_averages() {
        // y[n] = 0.5 x[n] + 0.5 y[n-1] is minimum phase: the phase from its magnitude is its own phase
        let n = 4097;
        let h: Vec<Complex32> = (0..n).map(|k| Complex32::new(0.5, 0.0) / (Complex32::new(1.0, 0.0) - Complex32::from_polar(0.5, -std::f32::consts::PI * k as f32 / (n - 1) as f32))).collect();
        let mag: Vec<f32> = h.iter().map(|c| 20.0 * c.norm().log10()).collect();
        let p = min_phase(&mag, 0, n - 1);
        for k in [100usize, 1000, 3000] {
            assert!((p[k] - h[k].arg()).abs() < 0.01, "{k}: {} {}", p[k], h[k].arg());
        }
        // two positions in opposite phase: the powers stay, the vectors cancel
        let pos = vec![(vec![0.0f32; 4], vec![0.0f32; 4]), (vec![0.0; 4], vec![180.0; 4])];
        assert!(spatial_average(&pos, false).0[0].abs() < 0.01);
        assert!(spatial_average(&pos, true).0[0] < -100.0);
    }
}
