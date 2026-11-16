//! Track headers of the timeline: name, buttons, fader, channel rows and the input menu.

use super::*;

impl Daw {
    /// A click on a track header: selects it; with Ctrl it joins or leaves the group of selected tracks.
    pub(super) fn click_track(&mut self, ui: &Ui, id: u32) {
        if ui.input(|i| i.modifiers.command) {
            if self.sel_tracks.is_empty()
                && let Some(cur) = self.sel_track {
                    self.sel_tracks.insert(cur);
                }
            if !self.sel_tracks.remove(&id) {
                self.sel_tracks.insert(id);
            }
        } else {
            self.sel_tracks.clear();
        }
        self.sel_track = Some(id);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn track_header(&mut self, ui: &mut Ui, p: &mut Project, ti: usize, hr: Rect, clip: Rect, remove: &mut Option<usize>, swap: &mut Option<(usize, usize)>, dup: &mut Option<usize>) {
        let selected = self.sel_track == Some(p.tracks[ti].id) || self.sel_tracks.contains(&p.tracks[ti].id);
        let col = track_color(&p.tracks[ti]);
        let painter = ui.painter().with_clip_rect(hr.intersect(clip));
        painter.rect_filled(hr, 0.0, if selected { HDR_SEL } else { HDR_BG });
        painter.rect_filled(Rect::from_min_size(hr.min, vec2(4.0, hr.height())), 0.0, col);
        let id = p.tracks[ti].id;
        // click on the empty header selects the track; right-click opens the track menu
        let bg_resp = ui.interact(hr.intersect(clip), ui.scope_id().with(("daw_hdr", id)), Sense::click());
        if bg_resp.clicked() {
            self.click_track(ui, id);
        }
        bg_resp.context_menu(|ui| {
            let mut editing = self.auto_edit == Some(id);
            if ui.checkbox(&mut editing, crate::lang::tr("Edit volume automation")).on_hover_text(crate::lang::tr("Draw level changes over time on this track's lane: double-click adds a point, drag moves it, right-click deletes it. The level is added to the fader.")).changed() {
                self.auto_edit = if editing { Some(id) } else { None };
                ui.close();
            }
            if !p.tracks[ti].auto.is_empty() && crate::mapping::btn(ui, "Clear the automation").clicked() {
                self.checkpoint(p);
                p.tracks[ti].auto.clear();
                self.touch();
                ui.close();
            }
            // takes: what a recording replaced is kept on the shelf of the track
            if !p.tracks[ti].shelf.is_empty() {
                let mut takes: Vec<u32> = p.tracks[ti].shelf.iter().map(|(k, _)| *k).collect();
                takes.sort_unstable();
                takes.dedup();
                ui.menu_button(format!("Takes ({})", takes.len()), |ui| {
                    let mut show = self.takes_view.contains(&id);
                    if ui.checkbox(&mut show, crate::lang::tr("Show the takes under the track")).changed() {
                        if show { self.takes_view.insert(id); } else { self.takes_view.remove(&id); }
                    }
                    ui.separator();
                    match self.range {
                        Some((a, b)) if b > a => {
                            ui.label(RichText::new(crate::lang::tr("Use a take for the selected range:")).small().weak());
                            for k in &takes {
                                let (lo, hi) = p.tracks[ti].shelf.iter().filter(|(t, _)| t == k).fold((i64::MAX, 0), |(lo, hi), (_, c)| (lo.min(c.start), hi.max(c.end())));
                                let t = format!("Take {k}  ({} - {})", clock(lo as f32 / p.sr as f32), clock(hi as f32 / p.sr as f32));
                                if crate::mapping::btn(ui, t).clicked() {
                                    self.checkpoint(p);
                                    if !p.comp_swap(ti, *k, a, b) {
                                        self.note = format!("Take {k} has nothing in that range");
                                    }
                                    self.touch();
                                    ui.close();
                                }
                            }
                        }
                        _ => {
                            ui.label(RichText::new(crate::lang::tr("Select a time range to use a take there")).small().weak());
                        }
                    }
                    ui.separator();
                    if crate::mapping::btn(ui, "Delete the older takes").clicked() {
                        self.checkpoint(p);
                        p.tracks[ti].shelf.clear();
                        self.takes_view.remove(&id);
                        self.touch();
                        ui.close();
                    }
                });
            }
            let mut bt = p.tracks[ti].beats;
            if ui.checkbox(&mut bt, crate::lang::tr("Beatmarkers")).on_hover_text(crate::lang::tr("Finds the beat of the music on this track and marks it on the waveform. Hold Shift while selecting to snap to the beat. Off by default.")).changed() {
                p.tracks[ti].beats = bt;
                self.touch();
                ui.close();
            }
            ui.separator();
            if crate::mapping::btn(ui, "Rename").clicked() {
                self.renaming = Some((id, p.tracks[ti].name.clone()));
                ui.close();
            }
            if crate::mapping::btn(ui, "Import audio file to this track...").on_hover_text(crate::lang::tr("Place a file on this track at the playhead")).clicked() && self.open_dlg.is_none() {
                self.import_target = Some((id, if IMPORT_AT_START.load(Relaxed) { 0 } else { self.cursor }));
                self.open_dlg = Some(dialogs::open_many("Add audio to the track", "Audio files", decode::EXTENSIONS));
                ui.close();
            }
            if graph_on() {
                if crate::mapping::btn(ui, "Record from the Routing window").on_hover_text(crate::lang::tr("Arms the track and adds a Timeline record node you can connect to anything")).clicked() {
                    self.checkpoint(p);
                    p.tracks[ti].input = graph_input(id);
                    p.tracks[ti].arm = true;
                    crate::graph::bus().request_rec_node(id, &p.tracks[ti].name);
                    ui.close();
                }
            } else {
                ui.menu_button(crate::lang::tr("Record input"), |ui| self.input_menu(ui, p, ti));
            }
            if p.tracks[ti].arm && crate::mapping::btn(ui, "Disarm").clicked() {
                p.tracks[ti].arm = false;
                self.touch();
                ui.close();
            }
            if p.tracks[ti].clips.iter().any(|c| c.src.stereo()) {
                ui.separator();
                let t = &p.tracks[ti];
                if t.chan == Chan::Stereo || t.chan == Chan::Mono {
                    let mono = t.chan == Chan::Mono;
                    if crate::mapping::btn(ui, if mono { "Back to stereo" } else { "Convert to mono" }).clicked() {
                        self.checkpoint(p);
                        p.tracks[ti].chan = if mono { Chan::Stereo } else { Chan::Mono };
                        ui.close();
                    }
                    if crate::mapping::btn(ui, "Split into left and right tracks").on_hover_text(crate::lang::tr("Two tracks, panned hard left and hard right")).clicked() {
                        self.split_req = Some(id);
                        ui.close();
                    }
                } else if crate::mapping::btn(ui, "Back to stereo").clicked() {
                    self.checkpoint(p);
                    p.tracks[ti].chan = Chan::Stereo;
                    p.tracks[ti].pan = 0.0;
                    ui.close();
                }
                let expanded = p.tracks[ti].h >= 2;
                if crate::mapping::btn(ui, if expanded { "Hide channel sub-tracks" } else { "Show channel sub-tracks" }).clicked() {
                    p.tracks[ti].h = if expanded { 1 } else { 2 };
                    self.touch();
                    ui.close();
                }
            }
            crate::windows::sticky_menu(ui, crate::lang::tr("Input trim (gain / delay)"), |ui| {
                ui.label(RichText::new(crate::lang::tr("For recording from several interfaces at once:")).small().weak());
                let t = &mut p.tracks[ti];
                let mut ch = false;
                ui.horizontal(|ui| {
                    ui.label(crate::lang::tr("Gain"));
                    ch |= ui.add(egui::DragValue::new(&mut t.in_db).range(-60.0..=40.0).speed(0.1).suffix(crate::lang::tr(" dB"))).changed();
                });
                ui.horizontal(|ui| {
                    ui.label(crate::lang::tr("Place earlier by"));
                    ch |= ui.add(egui::DragValue::new(&mut t.in_ms).range(-1000.0..=1000.0).speed(0.2).suffix(crate::lang::tr(" ms"))).changed();
                });
                ui.label(RichText::new(crate::lang::tr("Use the delay for an interface whose audio arrives late (its latency),\nor a negative value to push a take later. Applied to the next recording.")).small().weak());
                if ch {
                    self.touch();
                }
            });
            ui.separator();
            ui.menu_button(crate::lang::tr("Colour"), |ui| {
                for (k, &(r, g, b)) in TRACK_COLORS.iter().enumerate() {
                    if ui.add(egui::Button::new("      ").fill(Color32::from_rgb(r, g, b))).clicked() {
                        p.tracks[ti].color = k;
                        self.touch();
                        ui.close();
                    }
                }
            });
            ui.menu_button(crate::lang::tr("Height"), |ui| {
                for (k, n) in ["Small", "Normal", "Large (split channels)"].iter().enumerate() {
                    if ui.selectable_label(p.tracks[ti].h as usize == k, crate::lang::tr(n)).clicked() {
                        p.tracks[ti].h = k as u8;
                        self.touch();
                        ui.close();
                    }
                }
            });
            if ui.add_enabled(ti > 0, egui::Button::new(crate::lang::tr("Move up"))).clicked() {
                *swap = Some((ti, ti - 1));
                ui.close();
            }
            if ui.add_enabled(ti + 1 < p.tracks.len(), egui::Button::new(crate::lang::tr("Move down"))).clicked() {
                *swap = Some((ti, ti + 1));
                ui.close();
            }
            if crate::mapping::btn(ui, "Duplicate track").clicked() {
                *dup = Some(ti);
                ui.close();
            }
            let n_sel = self.merge_ids(p).len();
            ui.separator();
            ui.label(RichText::new(if n_sel >= 2 { format!("{n_sel} tracks selected") } else { "Ctrl + click track names to select several".to_string() }).small().weak());
            if ui.add_enabled(n_sel >= 2, egui::Button::new(crate::lang::tr("Merge selected tracks into one"))).on_hover_text(crate::lang::tr("Mix them as they sound into one track and remove the originals (undo brings them back)")).clicked() {
                self.act_merge_tracks(p, true);
                ui.close();
            }
            if ui.add_enabled(n_sel >= 2, egui::Button::new(crate::lang::tr("Mix selected tracks to a new track"))).on_hover_text(crate::lang::tr("Keep the originals")).clicked() {
                self.act_merge_tracks(p, false);
                ui.close();
            }
            if crate::mapping::btn(ui, "Delete track").clicked() {
                *remove = Some(ti);
                ui.close();
            }
        });
        let content = Rect::from_min_max(hr.min + vec2(9.0, 3.0), pos2(hr.max.x - 12.0, hr.max.y - 2.0));
        ui.scope_builder(UiBuilder::new().max_rect(content).layout(Layout::top_down(Align::LEFT)), |ui| {
            ui.shrink_clip_rect(content.intersect(clip));
            ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
            ui.spacing_mut().interact_size.y = 15.0;
            ui.spacing_mut().slider_width = 52.0;
            ui.spacing_mut().slider_rail_height = 4.0;
            // name + M S R
            let w = content.width();
            ui.horizontal(|ui| {
                ui.set_height(17.0);
                if self.renaming.as_ref().is_some_and(|r| r.0 == id) {
                    let (_, text) = self.renaming.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(w - 66.0).font(FontId::proportional(11.5)));
                    // focus once when the field appears; asking every frame would keep it from ever closing
                    let init = ui.scope_id().with(("daw_rename_init", id));
                    if !ui.data(|d| d.get_temp::<bool>(init).unwrap_or(false)) {
                        r.request_focus();
                        ui.data_mut(|d| d.insert_temp(init, true));
                    }
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if r.lost_focus() {
                        ui.data_mut(|d| d.insert_temp(init, false));
                        let (_, text) = self.renaming.take().unwrap();
                        if !esc && !text.trim().is_empty() {
                            p.tracks[ti].name = text.trim().to_string();
                            self.touch();
                        }
                    }
                } else {
                    let r = ui.add_sized(vec2(w - 62.0, 15.0), egui::Label::new(RichText::new(&p.tracks[ti].name).strong().size(11.5)).truncate().sense(Sense::click()));
                    if r.double_clicked() {
                        self.renaming = Some((id, p.tracks[ti].name.clone()));
                    } else if r.clicked() {
                        self.click_track(ui, id);
                    }
                    r.on_hover_text(crate::lang::tr("Double-click to rename.  Ctrl + click: select more tracks (to merge them)"));
                }
                let t = &mut p.tracks[ti];
                if mini_toggle(ui, "M", t.mute, Color32::from_rgb(235, 190, 70), "Mute") {
                    t.mute = !t.mute;
                    self.touch();
                }
                if mini_toggle(ui, "S", t.solo, Color32::from_rgb(110, 200, 250), "Solo") {
                    t.solo = !t.solo;
                    self.touch();
                }
                if mini_toggle(ui, "R", t.arm, Color32::from_rgb(235, 80, 80), "Arm for recording") {
                    t.arm = !t.arm;
                    if t.arm && graph_on() {
                        // the track records what its record node in the routing graph receives
                        p.tracks[ti].input = graph_input(id);
                        crate::graph::bus().request_rec_node(id, &p.tracks[ti].name);
                    } else if t.arm && t.input.dev.is_empty()
                        && let Some(inp) = self.default_input_for(p, ti) {
                            p.tracks[ti].input = inp;
                        }
                    self.touch();
                }
            });
            // volume + pan (+ channel view)
            ui.horizontal(|ui| {
                // the two faders take what the icons and the little buttons leave, so nothing runs under the level meters
                let minis = 1 + usize::from(p.tracks[ti].clips.iter().any(|c| c.src.stereo()));
                let free = ui.available_width() - 2.0 * 12.0 - minis as f32 * 18.0 - (3 + minis) as f32 * ui.spacing().item_spacing.x;
                ui.spacing_mut().slider_width = (free / 2.0).clamp(18.0, 60.0);
                let mut t = p.tracks[ti].clone();
                draw_icon(ui, SliderIcon::Volume);
                let tip = format!("Volume {:+.1} dB", t.vol_db);
                let ch1 = self.fader(ui, ("vol", id), &mut t.vol_db, -60.0..=12.0, 0.0, 1.0, &tip);
                draw_icon(ui, SliderIcon::Pan);
                let ptxt = if t.pan.abs() < 0.01 { "centre".to_string() } else if t.pan < 0.0 { format!("{:.0}% left", -t.pan * 100.0) } else { format!("{:.0}% right", t.pan * 100.0) };
                let ch2 = self.fader(ui, ("pan", id), &mut t.pan, -1.0..=1.0, 0.0, 100.0, &format!("Pan (left / right balance): {ptxt}"));
                if ch1 || ch2 {
                    p.tracks[ti].vol_db = t.vol_db;
                    p.tracks[ti].pan = t.pan;
                    self.touch();
                }
                let inv = p.tracks[ti].invert;
                if mini_toggle(ui, "Ø", inv, Color32::from_rgb(250, 140, 200), "Invert the waveform (polarity) of this track") {
                    p.tracks[ti].invert = !inv;
                    self.touch();
                }
                if p.tracks[ti].clips.iter().any(|c| c.src.stereo()) {
                    let on = p.tracks[ti].h >= 2;
                    if mini_toggle(ui, "LR", on, Color32::from_rgb(120, 170, 250), "Show the left and right channel as sub-tracks you can edit separately") {
                        p.tracks[ti].h = if on { 1 } else { 2 };
                        self.touch();
                    }
                }
            });
            if p.tracks[ti].h >= 2 {
                self.channel_rows(ui, p, ti);
            }
            // input selection of armed tracks
            if p.tracks[ti].arm && hr.height() >= 62.0 {
                if graph_on() {
                    ui.label(RichText::new(crate::lang::tr("records from the Routing window")).small().weak());
                } else {
                    self.input_row(ui, p, ti);
                }
            }
        });
        // level meters
        let lv = if p.tracks[ti].arm && self.play.is_none() || self.rec.is_some() && p.tracks[ti].arm {
            self.in_level.get(&id).copied().unwrap_or([0.0; 2])
        } else {
            self.out_level.get(ti).copied().unwrap_or([0.0; 2])
        };
        let mh = hr.height() - 8.0;
        meter_bar(&painter, Rect::from_min_size(pos2(hr.right() - 10.0, hr.top() + 4.0), vec2(3.0, mh)), lv[0], self.show_clip);
        meter_bar(&painter, Rect::from_min_size(pos2(hr.right() - 6.0, hr.top() + 4.0), vec2(3.0, mh)), lv[1], self.show_clip);
        painter.line_segment([pos2(hr.left(), hr.bottom()), pos2(hr.right(), hr.bottom())], Stroke::new(1.0, Color32::from_rgb(8, 10, 14)));
    }

    /// A small slider. Double-click types a value, right-click resets. `disp` scales the typed / shown number
    /// (e.g. 100 for a pan in percent). Returns true when the value changed.
    pub(super) fn fader(&mut self, ui: &mut Ui, key: impl std::hash::Hash + std::fmt::Debug, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, disp: f32, tip: &str) -> bool {
        let id = ui.scope_id().with(key);
        let mut changed = false;
        if self.val_edit.as_ref().is_some_and(|e| e.0 == id) {
            let (_, text, focused) = self.val_edit.as_mut().unwrap();
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.spacing().slider_width).font(FontId::proportional(10.5)));
            if !*focused {
                *focused = true;
                r.request_focus();
                let mut st = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
                st.store(ui.ctx(), r.id);
            }
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if r.lost_focus() {
                let (_, text, _) = self.val_edit.take().unwrap();
                if !esc {
                    let num: String = text.chars().filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | ',')).collect();
                    if let Ok(x) = num.replace(',', ".").parse::<f32>() {
                        *v = (x / disp).clamp(*range.start(), *range.end());
                        changed = true;
                    }
                }
            }
            return changed;
        }
        let before = *v;
        let r = ui.add(egui::Slider::new(v, range).show_value(false));
        // a double-click first moves the slider with its single clicks: remember the value from before them
        let pre_id = id.with("pre");
        let now = ui.input(|i| i.time);
        if r.hovered() && ui.input(|i| i.pointer.any_pressed()) {
            let old = ui.data(|d| d.get_temp::<(f32, f64)>(pre_id));
            if old.is_none_or(|(_, t)| now - t > 0.6) {
                ui.data_mut(|d| d.insert_temp(pre_id, (before, now)));
            }
        }
        // a slider only senses dragging, so clicks (double-click, right-click) need a widget of their own on top
        let click = ui.interact(r.rect, id.with("click"), Sense::click());
        if click.double_clicked() {
            if let Some((pv, _)) = ui.data(|d| d.get_temp::<(f32, f64)>(pre_id)) {
                *v = pv;
            }
            self.val_edit = Some((id, format!("{:.1}", *v * disp), false));
        } else if r.changed() {
            changed = true;
        }
        if click.secondary_clicked() {
            *v = default;
            changed = true;
        }
        click.on_hover_text(format!("{tip}\nDouble-click: type a value.  Right-click: reset."));
        changed
    }

    /// Left / right sub-track controls of a stereo track, plus mono conversion and splitting.
    pub(super) fn channel_rows(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        let id = p.tracks[ti].id;
        let mut t = p.tracks[ti].clone();
        let (mut changed, mut ck) = (false, false);
        match t.chan {
            Chan::Stereo => {
                for k in 0..2 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().slider_width = 46.0;
                        let name = if k == 0 { "Left" } else { "Right" };
                        ui.add_sized(vec2(11.0, 15.0), egui::Label::new(RichText::new(&name[..1]).strong().size(10.5).color(Color32::from_rgb(160, 185, 230))));
                        if mini_toggle(ui, "M", t.ch_mute[k], Color32::from_rgb(235, 190, 70), &format!("Mute the {} channel", name.to_lowercase())) {
                            t.ch_mute[k] = !t.ch_mute[k];
                            changed = true;
                            ck = true;
                        }
                        // level differences between the channels are the balance's job; a gain set here by an older version is shown so it can be cleared
                        if t.ch_db[k].abs() > 0.05 {
                            ui.label(RichText::new(format!("{:+.1} dB", t.ch_db[k])).small().weak());
                            if crate::mapping::small_btn(ui, "0 dB").on_hover_text(crate::lang::tr("Clear the gain of this channel (use the balance for level differences between the channels)")).clicked() {
                                t.ch_db[k] = 0.0;
                                changed = true;
                                ck = true;
                            }
                        }
                    });
                }
            }
            Chan::Left | Chan::Right | Chan::Mono => {
                let txt = match t.chan {
                    Chan::Left => "Plays the left channel only",
                    Chan::Right => "Plays the right channel only",
                    _ => "Left and right mixed to mono",
                };
                ui.label(RichText::new(txt).small().weak());
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mono = t.chan == Chan::Mono;
            if t.chan == Chan::Left || t.chan == Chan::Right {
                if crate::mapping::small_btn(ui, "Back to stereo").on_hover_text(crate::lang::tr("Play both channels again")).clicked() {
                    t.chan = Chan::Stereo;
                    t.pan = 0.0;
                    changed = true;
                    ck = true;
                }
            } else {
                if ui.selectable_label(mono, RichText::new(crate::lang::tr("Mono")).size(10.5)).on_hover_text(crate::lang::tr("Mix left and right to one mono signal (not destructive)")).clicked() {
                    t.chan = if mono { Chan::Stereo } else { Chan::Mono };
                    changed = true;
                    ck = true;
                }
                if crate::mapping::small_btn(ui, "Split L/R").on_hover_text(crate::lang::tr("Make two tracks: the left channel panned left and the right channel panned right")).clicked() {
                    self.split_req = Some(id);
                }
            }
        });
        if ck {
            self.checkpoint(p);
        }
        if changed {
            let dst = &mut p.tracks[ti];
            dst.chan = t.chan;
            dst.pan = t.pan;
            dst.ch_db = t.ch_db;
            dst.ch_mute = t.ch_mute;
            self.touch();
        }
    }

    /// Menu entries that connect a track to a recording input (and arm it).
    pub(super) fn input_menu(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        if self.devs_v.is_empty() {
            ui.label(RichText::new(crate::lang::tr("scanning inputs...")).small().weak());
        }
        let devs = self.devs_v.clone();
        for d in devs {
            ui.menu_button(capture::short_label(&d.label), |ui| {
                ui.label(RichText::new(format!("{} ch, {} Hz", d.ch, d.sr)).small().weak());
                let mut pick: Option<Mode> = None;
                for c in 0..d.ch.min(32) {
                    if crate::mapping::btn(ui, format!("Channel {}", c + 1)).clicked() {
                        pick = Some(Mode::Chan(c));
                    }
                }
                ui.separator();
                for k in 0..(d.ch / 2).min(16) {
                    if crate::mapping::btn(ui, format!("Stereo pair {}+{}", 2 * k + 1, 2 * k + 2)).clicked() {
                        pick = Some(Mode::Pair(k));
                    }
                }
                if crate::mapping::btn(ui, "Mono mix of all channels").clicked() {
                    pick = Some(Mode::Mix);
                }
                if let Some(m) = pick {
                    self.checkpoint(p);
                    p.tracks[ti].input = Input { dev: d.id.clone(), mode: m };
                    p.tracks[ti].arm = true;
                    self.touch();
                    ui.close();
                }
            });
        }
    }

    pub(super) fn default_input_for(&self, p: &Project, ti: usize) -> Option<Input> {
        let mut q = p.clone();
        q.tracks[ti].arm = false;
        self.default_input(&q)
    }

    pub(super) fn input_row(&mut self, ui: &mut Ui, p: &mut Project, ti: usize) {
        let id = p.tracks[ti].id;
        let cur_dev = p.tracks[ti].input.dev.clone();
        let dev_ch = self.devs_v.iter().find(|d| d.id == cur_dev).map(|d| d.ch).unwrap_or(2);
        let dev_label = self.devs_v.iter().find(|d| d.id == cur_dev).map(|d| capture::short_label(&d.label)).unwrap_or_else(|| if cur_dev.is_empty() { "no input".into() } else { "device missing".into() });
        let status = self.capt.get(&cur_dev).map(|c| c.status.lock().clone());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mut new_dev: Option<String> = None;
            egui::ComboBox::from_id_salt(("daw_dev", id)).width(74.0).truncate().selected_text(dev_label).show_ui(ui, |ui| {
                for d in &self.devs_v {
                    if ui.selectable_label(d.id == cur_dev, format!("{}  ({} ch)", capture::short_label(&d.label), d.ch)).clicked() {
                        new_dev = Some(d.id.clone());
                    }
                }
            });
            if let Some(d) = new_dev {
                p.tracks[ti].input = Input { dev: d, mode: Mode::Chan(0) };
                self.touch();
            }
            let mut mode = p.tracks[ti].input.mode;
            egui::ComboBox::from_id_salt(("daw_ch", id)).width(64.0).selected_text(mode.label()).show_ui(ui, |ui| {
                for c in 0..dev_ch.min(32) {
                    ui.selectable_value(&mut mode, Mode::Chan(c), Mode::Chan(c).label());
                }
                for k in 0..(dev_ch / 2).min(16) {
                    ui.selectable_value(&mut mode, Mode::Pair(k), format!("Stereo {}", Mode::Pair(k).label()));
                }
                ui.selectable_value(&mut mode, Mode::Mix, crate::lang::tr("Mono mix"));
            });
            if mode != p.tracks[ti].input.mode {
                p.tracks[ti].input.mode = mode;
                self.touch();
            }
            if let Some(s) = status
                && (s.contains("not found") || s.contains("unsupported") || s.contains("reconnect")) {
                    ui.label(RichText::new("!").color(Color32::from_rgb(255, 170, 70))).on_hover_text(s);
                }
        });
    }

    // ------------------------------------------------------------------ analysis panel
}
