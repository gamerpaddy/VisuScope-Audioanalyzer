//! The analysis panel of the timeline: levels, spectrum and spectrogram of the selection.

use super::*;

impl Daw {
    pub(super) fn analysis_panel(&mut self, ui: &mut Ui, cx: &mut Ctx, p: &Project, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(17, 21, 28));
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_rgb(40, 48, 62)), egui::StrokeKind::Inside);
        let inner = rect.shrink2(vec2(8.0, 5.0));
        let (a, b) = self.analysis_range(p);
        let what = if self.range.is_some() {
            "selection"
        } else if !self.sel.is_empty() {
            "selected clips"
        } else {
            "whole project"
        };
        let src = match (self.an_src, self.analysis_track_name(p)) {
            (AnSrc::Track, Some(n)) => format!("track {n}"),
            _ => "master mix".to_string(),
        };
        // header line
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), 18.0));
        ui.scope_builder(UiBuilder::new().max_rect(head).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(format!("Analysis of the {what}, {src}   {} - {}", clock(a as f32 / p.sr as f32), clock(b as f32 / p.sr as f32))).small().color(Color32::from_rgb(170, 182, 200)));
            ui.selectable_value(&mut self.an_src, AnSrc::Master, crate::lang::tr("Master"));
            ui.selectable_value(&mut self.an_src, AnSrc::Track, crate::lang::tr("Track"));
            if crate::mapping::small_btn(ui, "Loop & play").on_hover_text(crate::lang::tr("Play this stretch in a loop: all analysers (spectrum, scope, loudness ...) follow it")).clicked() {
                if self.range.is_none() && b > a {
                    self.range = Some((a, b));
                }
                self.loop_on = true;
                self.seek(a);
                if self.play.is_none() {
                    self.feed_once = true;
                    self.play_start(cx);
                }
            }
            if let Some(out) = self.an_out.clone() {
                if crate::mapping::small_btn(ui, "Store spectrum as trace").on_hover_text(crate::lang::tr("Add the averaged spectrum to the trace library (Response, Transfer and EQ windows can use it)")).clicked() {
                    let name = format!("Timeline {what} ({})", src);
                    let avg: Vec<f32> = (0..out.f.len()).map(|i| 10.0 * (0.5 * (10f32.powf(out.db[0][i] / 10.0) + 10f32.powf(out.db[1][i] / 10.0))).max(1e-12).log10()).collect();
                    cx.lib.add(Some(name), out.f.clone(), avg, Vec::new());
                    cx.req.note = Some("Spectrum stored in the trace library".into());
                }
            }
            if self.an_running {
                ui.spinner();
            }
        });
        let body = Rect::from_min_max(pos2(inner.left(), inner.top() + 22.0), inner.max);
        let Some(out) = self.an_out.clone() else {
            painter.text(body.center(), Align2::CENTER_CENTER, if b > a { "analysing..." } else { "nothing to analyse yet" }, FontId::proportional(12.0), Color32::from_rgb(110, 122, 140));
            return;
        };
        // wide enough for "-10.4 / -14.2 dBFS" and "+0.00093 / +0.00001" in the monospace values
        // a narrow panel (to the right of the tracks) stacks the three parts instead of putting them side by side
        if body.width() < 600.0 {
            // a narrow panel stacks the three parts, each at a size it can be read at; when the panel is too low for all of them
            // the whole stack scrolls instead of squeezing the plots to nothing
            let two_cols = body.width() >= 330.0;
            let stats_h = if two_cols { 104.0 } else { 178.0 };
            let plot_h = ((body.height() - stats_h - 12.0) * 0.5).clamp(110.0, 260.0);
            let total = (stats_h + 2.0 * plot_h + 12.0).max(body.height());
            ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                ui.shrink_clip_rect(body);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), total), Sense::hover());
                    let s = Rect::from_min_size(full.min, vec2(full.width(), stats_h));
                    let sp = Rect::from_min_size(pos2(full.left(), s.bottom() + 6.0), vec2(full.width(), plot_h));
                    let sg = Rect::from_min_max(pos2(full.left(), sp.bottom() + 6.0), full.max);
                    self.stats_grid(ui, &out, s, two_cols, false);
                    self.spectrum_plot(ui, &out, sp);
                    self.spectrogram_view(ui, &out, sg, (a, b), p.sr);
                });
            });
            if out.truncated {
                painter.text(pos2(rect.right() - 10.0, rect.top() + 6.0), Align2::RIGHT_TOP, "analysed: first 5 minutes", FontId::proportional(10.0), Color32::from_rgb(255, 190, 80));
            }
            return;
        }
        let (stats_r, spec_r, spg_r) = {
            let stats_w = 250.0f32.min(body.width() * 0.34);
            let rest = body.width() - stats_w - 12.0;
            let spec_w = rest * 0.56;
            let s = Rect::from_min_size(body.min, vec2(stats_w, body.height()));
            let sp = Rect::from_min_size(pos2(s.right() + 6.0, body.top()), vec2(spec_w, body.height()));
            (s, sp, Rect::from_min_max(pos2(sp.right() + 6.0, body.top()), body.max))
        };
        self.stats_grid(ui, &out, stats_r, false, true);
        self.spectrum_plot(ui, &out, spec_r);
        self.spectrogram_view(ui, &out, spg_r, (a, b), p.sr);
        if out.truncated {
            painter.text(pos2(rect.right() - 10.0, rect.top() + 6.0), Align2::RIGHT_TOP, "analysed: first 5 minutes", FontId::proportional(10.0), Color32::from_rgb(255, 190, 80));
        }
    }

    pub(super) fn analysis_track_name(&self, p: &Project) -> Option<String> {
        let id = self.sel_track.or_else(|| self.sel.iter().next().and_then(|c| p.find_clip(*c)).map(|(ti, _)| p.tracks[ti].id))?;
        p.track_index(id).map(|ti| p.tracks[ti].name.clone())
    }

    /// `two_cols`: two label / value pairs per row (a wide but low stack); `scroll`: the grid scrolls by itself when it does not fit.
    pub(super) fn stats_grid(&self, ui: &mut Ui, out: &AnalysisOut, r: Rect, two_cols: bool, scroll: bool) {
        let s = &out.stats;
        let f = |v: f32| if v <= -119.0 { "-inf".to_string() } else { format!("{v:.1}") };
        let rows: Vec<(&str, String)> = vec![
            ("Length", clock(s.secs)),
            ("Peak L / R", format!("{} / {} dBFS", f(s.peak_db[0]), f(s.peak_db[1]))),
            ("RMS L / R", format!("{} / {} dBFS", f(s.rms_db[0]), f(s.rms_db[1]))),
            ("True peak", format!("{} dBTP", f(s.true_peak_db))),
            ("Loudness", s.lufs.map_or("-".into(), |v| format!("{v:.1} LUFS"))),
            ("Loudness range", s.lra.map_or("-".into(), |v| format!("{v:.1} LU"))),
            ("Crest factor", format!("{:.1} dB", s.crest_db)),
            ("L/R correlation", format!("{:+.2}", s.corr)),
            ("DC offset", format!("{:+.5} / {:+.5}", s.dc[0], s.dc[1])),
            ("Clipped samples", format!("{}", s.clipped)),
            ("Silence", format!("{:.0} %", s.silence_pct)),
        ];
        // the rows fill the room there is: more air between them when the area is tall, larger text when there is plenty
        let per_row = if two_cols { rows.len().div_ceil(2) } else { rows.len() } as f32;
        let room = r.height() / per_row;
        let gap = (room - 15.0).clamp(1.0, 9.0);
        let big = room > 24.0 && r.width() > 230.0;
        let show = |ui: &mut Ui, rows: Vec<(&str, String)>| {
            let cell = |ui: &mut Ui, k: &str, v: String| {
                let warn = (k == "Clipped samples" && s.clipped > 0) || (k == "True peak" && s.true_peak_db > -0.1);
                let (kt, vt) = (RichText::new(k).weak(), RichText::new(v).monospace().color(if warn { Color32::from_rgb(255, 130, 110) } else { Color32::from_rgb(215, 224, 238) }));
                if big {
                    ui.label(kt.size(12.0));
                    ui.add(egui::Label::new(vt.size(13.5)).truncate());
                } else {
                    ui.label(kt.small());
                    // (in two columns the values are not cut: the grid takes the width it needs)
                    ui.add(egui::Label::new(vt.small()).wrap_mode(if two_cols { egui::TextWrapMode::Extend } else { egui::TextWrapMode::Truncate }));
                }
            };
            if two_cols {
                let half = rows.len().div_ceil(2);
                egui::Grid::new("daw_stats2").num_columns(4).spacing([10.0, gap]).show(ui, |ui| {
                    for i in 0..half {
                        cell(ui, rows[i].0, rows[i].1.clone());
                        match rows.get(i + half) {
                            Some((k, v)) => cell(ui, k, v.clone()),
                            None => {
                                ui.label("");
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });
            } else {
                egui::Grid::new("daw_stats").num_columns(2).spacing([8.0, gap]).show(ui, |ui| {
                    for (k, v) in rows {
                        cell(ui, k, v);
                        ui.end_row();
                    }
                });
            }
        };
        ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
            ui.shrink_clip_rect(r);
            if scroll {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| show(ui, rows));
            } else {
                show(ui, rows);
            }
        });
    }

    pub(super) fn spectrum_plot(&self, ui: &mut Ui, out: &AnalysisOut, r: Rect) {
        if r.width() < 60.0 || r.height() < 40.0 {
            return;
        }
        let painter = ui.painter_at(r);
        let resp = ui.interact(r, ui.scope_id().with("daw_spec"), Sense::hover());
        painter.rect_filled(r, 3.0, crate::views::BG);
        let rect = r;
        let plot_r = inset(rect);
        let f1 = (out.sr as f32 * 0.5).min(20000.0);
        let pl = Plot::new(plot_r, analysis::F_LO, f1, -100.0, 0.0, true);
        pl.grid_x(&painter, &|f| hz_label(f));
        pl.grid_y(&painter, &|v| format!("{v:.0}"));
        pl.frame(&painter);
        for (c, col) in [(0usize, Color32::from_rgb(70, 200, 255)), (1, Color32::from_rgb(255, 100, 160))] {
            pl.line(&painter, out.f.iter().copied().zip(out.db[c].iter().copied()), Stroke::new(1.3, col.gamma_multiply(0.9)));
        }
        painter.text(rect.left_top() + vec2(48.0, 6.0), Align2::LEFT_TOP, "Average spectrum (dBFS)", FontId::proportional(9.5), Color32::from_rgb(130, 142, 160));
        if let Some(h) = resp.hover_pos() {
            if plot_r.contains(h) {
                let f = pl.fx(h.x);
                if let Some(i) = out.f.iter().position(|&x| x >= f) {
                    painter.line_segment([pos2(h.x, plot_r.top()), pos2(h.x, plot_r.bottom())], Stroke::new(1.0, Color32::from_white_alpha(50)));
                    pl.label(&painter, pos2(plot_r.right() - 4.0, plot_r.top() + 4.0), format!("{}Hz  L {:.1}  R {:.1}", hz_label(out.f[i]), out.db[0][i], out.db[1][i]), Color32::WHITE, Align2::RIGHT_TOP);
                }
            }
        }
    }

    pub(super) fn spectrogram_view(&mut self, ui: &mut Ui, out: &Arc<AnalysisOut>, r: Rect, range: (i64, i64), sr: u32) {
        if r.width() < 60.0 || r.height() < 40.0 || out.spg.is_empty() {
            return;
        }
        // texture from the cached analysis (rebuilt when a new result arrived)
        let ptr = Arc::as_ptr(out) as usize;
        if self.spg_tex.is_none() || self.spg_ptr != ptr {
            let lut = crate::colormap::Cmap::Inferno.lut();
            let (w, h) = (out.spg_w, out.spg_h);
            let mut px = vec![Color32::BLACK; w * h];
            for x in 0..w {
                for y in 0..h {
                    px[(h - 1 - y) * w + x] = lut[out.spg[x * h + y] as usize];
                }
            }
            let img = egui::ColorImage::new([w, h], px);
            match &mut self.spg_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.spg_tex = Some(ui.ctx().load_texture("daw_spectrogram", img, egui::TextureOptions::LINEAR)),
            }
            self.spg_ptr = ptr;
        }
        let painter = ui.painter_at(r);
        let rect = r;
        painter.rect_filled(rect, 3.0, Color32::BLACK);
        let plot_r = Rect::from_min_max(rect.min + vec2(34.0, 4.0), rect.max - vec2(4.0, 16.0));
        if let Some(t) = &self.spg_tex {
            painter.image(t.id(), plot_r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // frequency axis (log, same mapping as the rows)
        let f1 = (out.sr as f32 * 0.5).min(20000.0);
        for f in [50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            if f >= f1 {
                continue;
            }
            let t = (f / analysis::F_LO).ln() / (f1 / analysis::F_LO).ln();
            let y = plot_r.bottom() - t * plot_r.height();
            painter.line_segment([pos2(plot_r.left() - 3.0, y), pos2(plot_r.left(), y)], Stroke::new(1.0, Color32::from_gray(120)));
            painter.text(pos2(plot_r.left() - 5.0, y), Align2::RIGHT_CENTER, hz_label(f), FontId::proportional(8.5), Color32::from_gray(140));
        }
        let (a, b) = range;
        painter.text(pos2(plot_r.left(), plot_r.bottom() + 2.0), Align2::LEFT_TOP, clock(a as f32 / sr as f32), FontId::proportional(8.5), Color32::from_gray(140));
        painter.text(pos2(plot_r.right(), plot_r.bottom() + 2.0), Align2::RIGHT_TOP, clock(b as f32 / sr as f32), FontId::proportional(8.5), Color32::from_gray(140));
        painter.rect_stroke(plot_r, 0.0, Stroke::new(1.0, Color32::from_gray(50)), egui::StrokeKind::Inside);
        // playhead inside the analysed stretch
        let pos = self.pos();
        if pos >= a && pos <= b && b > a {
            let x = plot_r.left() + (pos - a) as f32 / (b - a) as f32 * plot_r.width();
            painter.line_segment([pos2(x, plot_r.top()), pos2(x, plot_r.bottom())], Stroke::new(1.0, PLAYHEAD));
        }
    }
}
