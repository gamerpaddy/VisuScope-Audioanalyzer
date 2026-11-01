//! Profiler window: system latency, kernel interrupt / DPC time and audio callback timing, with every
//! hiccup that could be heard listed together with the most likely cause.

use super::{Ctx, Tool};
use crate::prof::{self, Sec};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};
use std::sync::atomic::Ordering::Relaxed;

pub struct Profiler {
    started: bool,
}

impl Profiler {
    pub fn new() -> Self {
        Self { started: false }
    }
}

/// Bars of the last two minutes, scaled to `max`; bars above `limit` are red and a line marks it.
fn bars(ui: &mut Ui, title: &str, secs: &[Sec], val: impl Fn(&Sec) -> f32, max: f32, limit: Option<f32>, unit: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().max(120.0), 64.0), Sense::hover());
    let p = ui.painter_at(r);
    p.rect_filled(r, 3.0, Color32::from_rgb(15, 18, 24));
    let n = 120usize;
    let w = r.width() / n as f32;
    let cur = secs.last().map(&val).unwrap_or(0.0);
    let peak = secs.iter().rev().take(n).map(&val).fold(0f32, f32::max);
    let max = max.max(peak * 1.1).max(1e-3);
    for (k, s) in secs.iter().rev().take(n).enumerate() {
        let v = val(s);
        let h = (v / max).clamp(0.0, 1.0) * (r.height() - 14.0);
        let x = r.right() - (k as f32 + 1.0) * w;
        let bad = limit.is_some_and(|l| v > l);
        let col = if bad { Color32::from_rgb(235, 90, 80) } else if s.hiccup { Color32::from_rgb(235, 170, 70) } else { Color32::from_rgb(80, 160, 220) };
        p.rect_filled(Rect::from_min_max(pos2(x, r.bottom() - h), pos2(x + w.max(1.0) - 0.5, r.bottom())), 0.0, col);
    }
    if let Some(l) = limit {
        let y = r.bottom() - (l / max).clamp(0.0, 1.0) * (r.height() - 14.0);
        p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 120, 100, 150)));
    }
    p.text(r.min + vec2(5.0, 2.0), Align2::LEFT_TOP, format!("{title}   now {cur:.1}{unit}   worst {peak:.1}{unit}"), FontId::proportional(10.0), Color32::from_gray(200));
}

fn mmss(t: f32) -> String {
    format!("{}:{:02}", (t as u32) / 60, (t as u32) % 60)
}

impl Tool for Profiler {
    fn update(&mut self, _cx: &mut Ctx, _visible: bool) {
        if !self.started {
            self.started = true;
            prof::start();
        }
    }

    fn status(&self) -> Option<(String, Color32)> {
        let st = prof::state().lock();
        let t = st.secs.back().map(|s| s.t)?;
        let recent = st.events.iter().filter(|e| t - e.t < 10.0).count();
        (recent > 0).then(|| (format!("audio hiccup ({recent})"), Color32::from_rgb(255, 160, 80)))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        let (secs, events, ok, total): (Vec<Sec>, Vec<prof::Event>, bool, u32) = {
            let st = prof::state().lock();
            (st.secs.iter().copied().collect(), st.events.iter().cloned().collect(), st.counters_ok, st.total_hiccups)
        };
        if secs.is_empty() {
            ui.label("Measuring ...");
            return;
        }
        let last = *secs.last().unwrap();
        let recent = events.iter().filter(|e| last.t - e.t < 60.0).count();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.horizontal(|ui| {
                if recent == 0 {
                    ui.label(RichText::new("No hiccups in the last minute").strong().color(Color32::from_rgb(110, 210, 140)));
                } else {
                    ui.label(RichText::new(format!("{recent} hiccup(s) in the last minute")).strong().color(Color32::from_rgb(255, 170, 80)));
                }
                ui.label(RichText::new(format!("{total} since start")).small().weak());
                if ui.small_button("Clear").clicked() {
                    prof::clear();
                }
                if ui.small_button("Copy report").on_hover_text("Copy the numbers and the hiccup list to the clipboard").clicked() {
                    let mut s = format!("VisuScope {} profiler report\n", crate::VERSION_FULL);
                    s += &format!("timer latency worst {:.1} ms, input callback gap worst {:.1} ms, DPC {:.1} % (worst core {:.1} %), interrupts {:.1} %, CPU {:.0} %\n", last.wake_max_ms, last.cb_gap_max_ms, last.dpc, last.dpc_worst, last.intr, last.cpu);
                    for e in &events {
                        s += &format!("{}  {}  -> {}\n", mmss(e.t), e.what, e.cause);
                    }
                    ui.ctx().copy_text(s);
                }
            });
            if !ok {
                ui.label(RichText::new("Windows performance counters are not available: only timing measured by the program is shown.").small().color(Color32::from_rgb(255, 190, 90)));
            }
            // numbers
            let recent_secs: Vec<&Sec> = secs.iter().rev().take(10).collect();
            let worst10 = |f: fn(&Sec) -> f32| recent_secs.iter().map(|s| f(s)).fold(0f32, f32::max);
            let (fr, sr) = (prof::C.cb_frames.load(Relaxed), prof::C.cb_sr.load(Relaxed));
            let period = if sr > 0 { fr as f32 * 1000.0 / sr as f32 } else { 0.0 };
            // the engine block changes with every tick: show the average of the last full second so that it can be read
            let blk = last.eng_block_frames.round() as u32;
            let block_ms = last.eng_block_ms;
            {
                let row = |ui: &mut Ui, k: &str, v: String, warn: bool| {
                    ui.label(RichText::new(k).small().weak());
                    ui.add(egui::Label::new(RichText::new(v).small().monospace().color(if warn { Color32::from_rgb(255, 140, 110) } else { Color32::from_rgb(215, 224, 238) })).wrap());
                };
                row(ui, "Timer latency (1 ms sleep)", format!("{:.2} ms now, {:.1} ms worst in 10 s", last.wake_max_ms, worst10(|s| s.wake_max_ms)), worst10(|s| s.wake_max_ms) > 5.0);
                row(ui, "Late wake-ups in 10 s", format!(">2 ms: {}   >5 ms: {}   >10 ms: {}", recent_secs.iter().map(|s| s.late2).sum::<u32>(), recent_secs.iter().map(|s| s.late5).sum::<u32>(), recent_secs.iter().map(|s| s.late10).sum::<u32>()), recent_secs.iter().any(|s| s.late10 > 0));
                row(ui, "Input callback", if sr > 0 { format!("buffer {period:.1} ms ({fr} frames @ {sr} Hz), worst gap {:.1} ms, worst run {:.2} ms", worst10(|s| s.cb_gap_max_ms), worst10(|s| s.cb_dur_max_ms)) } else { "no stream".into() }, period > 0.0 && worst10(|s| s.cb_gap_max_ms) > period * 2.5 + 4.0);
                if cx.graph_on {
                    row(ui, "Routing engine", format!("tick avg {:.2} ms, max {:.2} ms, block about {blk} frames ({block_ms:.0} ms)", last.eng_tick_avg_ms, worst10(|s| s.eng_tick_max_ms)), worst10(|s| s.eng_tick_max_ms) > 8.0);
                }
                row(ui, "Analysis (DSP thread)", format!("{:.2} ms per frame at {:.0} fps", cx.an.dsp_ms, cx.an.dsp_fps), cx.an.dsp_ms > 12.0);
                if ok {
                    row(ui, "Kernel DPC time", format!("{:.1} %  (worst core {:.1} %)   {:.0} DPCs/s", last.dpc, last.dpc_worst, last.dpc_rate), last.dpc_worst >= 8.0 || last.dpc >= 4.0);
                    row(ui, "Interrupt time", format!("{:.1} %   {:.0} interrupts/s", last.intr, last.int_rate), last.intr >= 5.0);
                    row(ui, "CPU", format!("{:.0} %   queue {:.0}   {:.0} context switches/s", last.cpu, last.queue, last.ctx_rate), last.cpu >= 92.0);
                }
                let safety = prof::safety_ms() as f32;
                let est = period + if cx.graph_on { block_ms } else { 0.0 } + 60.0 + safety;
                row(ui, "Input to output delay", format!("about {est:.0} ms (input buffer {period:.0} ms + {} + output cushion 60 ms + safety buffer {safety:.0} ms)", if cx.graph_on { format!("engine block {:.0} ms", (block_ms / 5.0).round() * 5.0) } else { "no engine".into() }), false);
            }
            bars(ui, "Timer latency", &secs, |s| s.wake_max_ms, 10.0, Some(5.0), " ms");
            bars(ui, "Input callback gap", &secs, |s| s.cb_gap_max_ms, (period * 3.0).max(20.0), if period > 0.0 { Some(period * 2.5 + 4.0) } else { None }, " ms");
            if ok {
                bars(ui, "DPC time, worst core", &secs, |s| s.dpc_worst, 10.0, Some(8.0), " %");
                bars(ui, "Interrupt time", &secs, |s| s.intr, 10.0, Some(5.0), " %");
            }
            ui.separator();
            ui.label(RichText::new("Hiccups").strong());
            if events.is_empty() {
                ui.label(RichText::new("Nothing that could be heard so far. When the sound crackles, the cause shows up here.").small().weak());
            }
            for e in events.iter().take(40) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(mmss(e.t)).monospace().small().weak());
                    ui.label(RichText::new(&e.what).small().strong());
                });
                ui.label(RichText::new(&e.cause).small().color(if e.severe { Color32::from_rgb(255, 160, 110) } else { Color32::from_gray(165) }));
                ui.add_space(2.0);
            }
            egui::CollapsingHeader::new(RichText::new("What helps against crackling").small()).default_open(false).show(ui, |ui| {
                for t in [
                    "Raise the safety buffer in Options (adds a little delay, hides short stalls of the system), or the audio buffer of the interface (its control panel).",
                    "Windows power plan: High performance; switch off USB selective suspend and core parking.",
                    "DPC time above a few percent: update or disable the driver that LatencyMon names (network, Wi-Fi, graphics, USB).",
                    "Lower the load: smaller FFT, lower FFT rate, fewer open windows, a lower UI frame rate cap in Options.",
                    "Close programs that scan files or sync in the background while recording.",
                ] {
                    ui.label(RichText::new(format!("- {t}")).small().weak());
                }
            });
        });
    }
}
