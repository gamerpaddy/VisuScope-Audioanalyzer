//! The inside of the nodes: device pickers, parameter rows and the body of every kind of node.

use super::*;

impl Routing {
    pub(super) fn device_combo(&self, ui: &mut Ui, n: &mut Node, id_salt: &str) -> bool {
        let Some(e) = &self.eng else { return false };
        let devs: Vec<DevEx> = e.devs.lock().clone();
        let cur = if n.s.is_empty() {
            let d = e.default_in.lock().clone();
            devs.iter().find(|x| x.id == d).map(|x| format!("{} (default)", short_label(&x.label))).unwrap_or_else(|| "default input".into())
        } else {
            devs.iter().find(|x| x.id == n.s).map(|x| short_label(&x.label)).unwrap_or_else(|| "device missing".into())
        };
        let mut changed = false;
        egui::ComboBox::from_id_salt((id_salt, n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
            for d in &devs {
                if ui.selectable_label(n.s == d.id, format!("{}  ({} ch, {} Hz)", short_label(&d.label), d.ch, d.sr)).clicked() {
                    n.s = d.id.clone();
                    changed = true;
                }
            }
        });
        changed
    }

    pub(super) fn param_row(ui: &mut Ui, n: &mut Node, i: usize, label_w: f32) -> bool {
        Self::param_row_max(ui, n, i, label_w, None)
    }

    /// A parameter row; `max` limits the upper end of the slider (the channels of the device, for instance).
    pub(super) fn param_row_max(ui: &mut Ui, n: &mut Node, i: usize, label_w: f32, max: Option<f32>) -> bool {
        let z = ui.spacing().interact_size.y / 16.0;
        let defs = n.kind.info().params;
        let Some(d) = defs.get(i) else { return false };
        let hi = max.map_or(d.max, |m| m.min(d.max).max(d.min + 0.01));
        if n.p.len() <= i {
            n.p.resize(i + 1, d.def);
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // right-click the name to put the value back to its default
            let followed = MOD_CUR.lock().iter().any(|m| m.node == n.id && m.param == i);
            let name = ui.add_sized(vec2(label_w * z, 16.0 * z), egui::Label::new(RichText::new(crate::lang::tr(d.name)).small().color(if followed { Color32::from_rgb(255, 170, 70) } else { Color32::from_rgb(160, 172, 190) })).truncate().sense(Sense::click()));
            if name.on_hover_text(if followed { "Follows a signal (right-click the slider to change it). Right-click here: reset to the default" } else { "Right-click: reset to the default" }).secondary_clicked() {
                n.p[i] = d.def;
                changed = true;
            }
            if !d.choices.is_empty() {
                let switch = d.choices.len() == 2;
                // a followed switch shows the state the signal gives it
                let shown = if switch && followed { super::super::engine::MOD_LIVE.lock().iter().find(|m| m.0 == n.id && m.1 == i).map(|m| m.2).filter(|v| v.is_finite()).unwrap_or(n.p[i]) } else { n.p[i] };
                let cur = crate::lang::tr(d.choices.get(shown.round() as usize).copied().unwrap_or("?"));
                let w = ui.available_width() - 4.0;
                let r = ui.add_enabled_ui(!(switch && followed), |ui| {
                    egui::ComboBox::from_id_salt(("pc", n.id, i)).width(w).truncate().selected_text(cur).show_ui(ui, |ui| {
                        for (k, c) in d.choices.iter().enumerate() {
                            if ui.selectable_label(n.p[i].round() as usize == k, crate::lang::tr(*c)).clicked() {
                                n.p[i] = k as f32;
                                changed = true;
                            }
                        }
                    })
                });
                if switch {
                    // right-click: bind to a key, note or button (toggle or momentary); the dot at the left edge takes a signal
                    // (the menu gets an id of its own: the list of the dropdown and a menu of the same widget would share one popup)
                    let mut menu_r = r.inner.response.clone();
                    menu_r.id = menu_r.id.with("bind");
                    crate::mapping::button_menu(&menu_r,&crate::mapping::Target::toggle(format!("nodet:{}:{}", n.id, i), format!("{} #{}: {}", n.kind.info().name, n.id, d.name), true));
                    param_port(ui, z, pos2(ui.max_rect().left() - 10.0 * z, ui.min_rect().center().y), n.id, i, (0.0, 1.0, false, "switch"));
                }
            } else {
                // the value box next to the slider takes the width of the longest value it will show
                let digits = if d.unit == " us" { 7 } else { format!("{:.1}", hi.abs().max(d.min.abs())).len() + usize::from(d.min < 0.0) };
                let value_w = (digits + d.unit.len()) as f32 * 7.4 + 22.0;
                ui.spacing_mut().slider_width = (ui.available_width() - value_w * z - 8.0 * z).max(30.0 * z);
                let int = d.unit.is_empty() && hi - d.min >= 1.0 && d.min.fract() == 0.0 && d.def.fract() == 0.0;
                // right-click the track: back to the default; right-click the value box: reset, follow a signal, bind to an input
                let t = crate::mapping::Target::slider(format!("node:{}:{}", n.id, i), format!("{} #{}: {}", n.kind.info().name, n.id, d.name), d.min, hi, d.log, d.unit);
                let nid = n.id;
                let fmt = if int {
                    crate::slider::Fmt { int: true, ..crate::slider::Fmt::new(d.unit, 0) }
                } else if d.unit == " us" {
                    crate::slider::Fmt { us: true, ..crate::slider::Fmt::new("", 0) }
                } else if d.unit == " Hz" || d.unit == " ms" {
                    // (slow rates, wow, flutter, tremolo: tenths of a Hz)
                    crate::slider::Fmt::new(d.unit, if d.log && d.min >= 1.0 { 0 } else { 1 })
                } else {
                    crate::slider::Fmt::new(d.unit, 1)
                };
                if followed {
                    // the slider shows the value the signal gives it, live (what is dragged here does not count)
                    let mut shown = super::super::engine::MOD_LIVE.lock().iter().find(|m| m.0 == nid && m.1 == i).map(|m| m.2).filter(|v| v.is_finite()).unwrap_or(n.p[i]);
                    crate::slider::bound(ui, &mut shown, d.min, hi, d.log, d.def, &t, fmt, |_| {});
                } else {
                    changed |= crate::slider::bound(ui, &mut n.p[i], d.min, hi, d.log, d.def, &t, fmt, |_| {});
                }
                // the control input of the value: at the left edge of the node, in the row
                param_port(ui, z, pos2(ui.max_rect().left() - 10.0 * z, ui.min_rect().center().y), nid, i, (d.min, hi, d.log, d.unit));
            }
        });
        changed
    }

    /// Output nodes whose device is also captured by an input node (a loopback of it): a feedback loop.
    pub(super) fn feedback_nodes(&self, cx: &Ctx) -> HashSet<NodeId> {
        let default_out = cx.out_devs.first().map(|d| d.id.clone()).unwrap_or_default();
        let outs: Vec<(NodeId, String)> = self.g.nodes.iter().filter(|n| n.kind == Kind::Output).map(|n| (n.id, if n.s.is_empty() { default_out.clone() } else { n.s.clone() })).collect();
        let mut bad = HashSet::new();
        for i in self.g.nodes.iter().filter(|n| n.kind == Kind::Input) {
            let dev = if i.s.is_empty() { cx.default_in.clone() } else { i.s.clone() };
            // only a path from the input to the output closes the loop
            for (o, od) in &outs {
                if *od == dev && !dev.is_empty() && self.g.reaches(i.id, *o) {
                    bad.insert(i.id);
                    bad.insert(*o);
                }
            }
        }
        bad
    }

    /// Widgets of one node; returns true when something changed.
    pub(super) fn body(&mut self, ui: &mut Ui, cx: &Ctx, n: &mut Node, feedback: bool) -> bool {
        let z = self.zoom;
        ui.spacing_mut().item_spacing.y = 3.0 * z;
        ui.spacing_mut().interact_size.y = 16.0 * z;
        let mut ch = false;
        if feedback {
            ui.label(RichText::new(crate::lang::tr("feedback: this output is captured by the input that feeds it")).small().color(Color32::from_rgb(255, 120, 100)));
        }
        let status = self.eng.as_ref().and_then(|e| e.node_status.lock().get(&n.id).cloned());
        let meter = self.eng.as_ref().map(|e| e.meter(n.id)).unwrap_or_default();
        match n.kind {
            Kind::Input => {
                ch |= self.device_combo(ui, n, "indev");
                let ch_n = self.eng.as_ref().and_then(|e| {
                    let d = e.devs.lock();
                    let cur = if n.s.is_empty() { e.default_in.lock().clone() } else { n.s.clone() };
                    d.iter().find(|x| x.id == cur).map(|x| x.ch)
                });
                for i in 0..2 {
                    if let Some(c) = ch_n {
                        n.p[i] = n.p[i].min(c as f32);
                    }
                    // (the slider ends at the last channel of the device)
                    ch |= Self::param_row_max(ui, n, i, 52.0, ch_n.map(|c| c as f32));
                }
                ch |= Self::param_row(ui, n, 2, 52.0);
                ch |= Self::param_row(ui, n, 3, 52.0);
                if let Some(s) = status {
                    let bad = s.contains("not found") || s.contains("unsupported") || s.contains("reconnect") || s.contains("no device");
                    ui.label(RichText::new(s).small().color(if bad { Color32::from_rgb(255, 160, 90) } else { Color32::from_gray(120) }));
                }
            }
            Kind::Generator => {
                let n_gen = cx.gens.lock().len().max(1);
                let gi = (n.p[0].round() as usize).clamp(1, n_gen);
                let desc = cx.gens.lock().get(gi - 1).map(|p| format!("{} {:.0} Hz, {:.0} dBFS{}", p.wave.name(), p.freq, p.level_db[0], if p.on { "" } else { "  (off)" })).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::lang::tr("Generator")).small().color(Color32::from_rgb(160, 172, 190)));
                    egui::ComboBox::from_id_salt(("gsel", n.id)).selected_text(format!("Gen {gi}")).width(70.0 * z).show_ui(ui, |ui| {
                        for k in 1..=n_gen {
                            if ui.selectable_label(gi == k, format!("Gen {k}")).clicked() {
                                n.p[0] = k as f32;
                                ch = true;
                            }
                        }
                    });
                });
                ui.label(RichText::new(desc).small().weak());
                ui.label(RichText::new(crate::lang::tr("set it in the Generator window (tabs)")).small().weak());
            }
            Kind::TimelineOut | Kind::TimelineRec => {
                let tracks = bus().tracks.lock().clone();
                let master_ok = n.kind == Kind::TimelineOut;
                let cur = if n.s.is_empty() {
                    if master_ok { "Master mix".to_string() } else { "choose a track".to_string() }
                } else {
                    tracks.iter().find(|t| t.0.to_string() == n.s).map(|t| t.1.clone()).unwrap_or_else(|| "track missing".into())
                };
                egui::ComboBox::from_id_salt(("tsel", n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    if master_ok && ui.selectable_label(n.s.is_empty(), crate::lang::tr("Master mix")).clicked() {
                        n.s.clear();
                        ch = true;
                    }
                    for (id, name) in &tracks {
                        if ui.selectable_label(n.s == id.to_string(), name).clicked() {
                            n.s = id.to_string();
                            ch = true;
                        }
                    }
                    if !master_ok {
                        ui.separator();
                        if crate::mapping::btn(ui, "+ New track for this node").on_hover_text(crate::lang::tr("Adds an armed track to the timeline that records what this node receives")).clicked() {
                            bus().track_requests.lock().push((n.id, format!("Node {}", n.id)));
                            ui.close();
                        }
                    }
                });
                ui.label(RichText::new(if master_ok { "play the timeline to hear / see it" } else { "arm this track to record it" }).small().weak());
            }
            Kind::Dual => {
                // the rows line up with the two output dots
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0 * z);
                for k in 0..2 {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                    ui.painter().text(pos2(row.right() - 8.0, row.center().y), Align2::RIGHT_CENTER, format!("Line {} out", k + 1), FontId::proportional(11.0 * z), Color32::from_rgb(210, 190, 140));
                }
                ui.label(RichText::new(crate::lang::tr("both lines carry the whole stereo signal")).small().weak());
            }
            Kind::Split | Kind::Merge => {
                // the rows line up with the dots: the left channel on top, the right one below
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0 * z);
                for (k, name) in ["Left", "Right"].into_iter().enumerate() {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                    let col = if k == 0 { Color32::from_rgb(70, 200, 255) } else { Color32::from_rgb(255, 100, 160) };
                    let (al, x) = if n.kind == Kind::Split { (Align2::RIGHT_CENTER, row.right() - 8.0) } else { (Align2::LEFT_CENTER, row.left() + 8.0) };
                    ui.painter().text(pos2(x, row.center().y), al, if n.kind == Kind::Split { format!("{name} channel out") } else { format!("{name} channel in") }, FontId::proportional(11.0 * z), col);
                }
                ui.label(RichText::new(if n.kind == Kind::Split { "each output carries one channel on both sides" } else { "each input is made mono; first = left, second = right" }).small().weak());
            }
            Kind::Output => {
                if n.inputs() == 2 {
                    // the rows line up with the two input dots: the left channel on top, the right one below
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(6.0 * z);
                    for (k, name) in ["Left", "Right"].into_iter().enumerate() {
                        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                        let col = if k == 0 { Color32::from_rgb(70, 200, 255) } else { Color32::from_rgb(255, 100, 160) };
                        ui.painter().text(pos2(row.left() + 8.0, row.center().y), Align2::LEFT_CENTER, format!("{name} channel in"), FontId::proportional(11.0 * z), col);
                    }
                    ui.spacing_mut().item_spacing.y = 3.0 * z;
                }
                let cur = if n.s.is_empty() { cx.out_devs.first().map(|d| format!("{} (default)", d.label)).unwrap_or_else(|| "default output".into()) } else { cx.out_devs.iter().find(|d| d.id == n.s).map(|d| d.label.clone()).unwrap_or_else(|| "device missing".into()) };
                egui::ComboBox::from_id_salt(("outdev", n.id)).width(ui.available_width() - 6.0).truncate().selected_text(cur).show_ui(ui, |ui| {
                    for (i, d) in cx.out_devs.iter().enumerate() {
                        let id = if i == 0 { String::new() } else { d.id.clone() };
                        if ui.selectable_label(n.s == id, &d.label).clicked() {
                            n.s = id;
                            ch = true;
                        }
                    }
                });
                ch |= Self::param_row(ui, n, 0, 40.0);
                ch |= Self::param_row(ui, n, 1, 40.0);
                ch |= Self::param_row(ui, n, 2, 40.0);
                if let Some(s) = status {
                    ui.add(egui::Label::new(RichText::new(s).small().color(Color32::from_gray(120))).wrap());
                }
            }
            Kind::Analyzer => {
                ui.label(RichText::new(crate::lang::tr("feeds spectrum, scope, loudness ...")).small().weak());
            }
            Kind::Eq => {
                let fs = self.eng.as_ref().map_or(48000.0, |e| e.rate.load(std::sync::atomic::Ordering::Relaxed) as f32);
                let drawing = self.eq_draw.contains(&n.id);
                let bands0 = (n.p.len() / 4).clamp(1, EQ_MAX);
                n.p.resize(bands0 * 4, 0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let mut d = drawing;
                    if ui.toggle_value(&mut d, crate::lang::tr("Draw")).on_hover_text(crate::lang::tr("Edit with the mouse in the graph: drag a dot (frequency and level), click empty space = new band, right-click a dot = remove it")).changed() {
                        if d {
                            self.eq_draw.insert(n.id);
                        } else {
                            self.eq_draw.remove(&n.id);
                        }
                    }
                    if ui.add_enabled(n.p.len() / 4 < EQ_MAX, egui::Button::new(crate::lang::tr("+ band"))).on_hover_text(crate::lang::tr("Add a band (up to 12)")).clicked() {
                        n.p.extend([0.0, 1000.0, 0.0, 1.0]);
                        ch = true;
                    }
                    ui.label(RichText::new(format!("{} bands", n.p.len() / 4)).small().weak());
                });
                // response curve; with Draw on it is taller and the bands are dots to drag
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, if drawing { 150.0 * z } else { 54.0 * z }), if drawing { Sense::click_and_drag() } else { Sense::hover() });
                let range = 24.0f32;
                let xf = |f: f32| r.left() + ((f.max(20.0) / 20.0).log10() / 3.0).clamp(0.0, 1.0) * r.width();
                let fx = |x: f32| 20.0 * 10f32.powf(((x - r.left()) / r.width()).clamp(0.0, 1.0) * 3.0);
                let yg = |g: f32| r.center().y - (g / range).clamp(-1.0, 1.0) * r.height() * 0.5;
                let gy = |y: f32| ((r.center().y - y) / (r.height() * 0.5)).clamp(-1.0, 1.0) * range;
                let painter = ui.painter_at(r);
                painter.rect_filled(r, 3.0, Color32::from_rgb(14, 17, 23));
                for f in [100.0f32, 1000.0, 10000.0] {
                    painter.line_segment([pos2(xf(f), r.top()), pos2(xf(f), r.bottom())], Stroke::new(1.0, Color32::from_gray(30)));
                }
                if drawing {
                    for g in [-12.0f32, 12.0] {
                        painter.line_segment([pos2(r.left(), yg(g)), pos2(r.right(), yg(g))], Stroke::new(1.0, Color32::from_gray(30)));
                        painter.text(pos2(r.left() + 3.0, yg(g)), Align2::LEFT_CENTER, format!("{g:+.0}"), FontId::proportional(8.0), Color32::from_gray(90));
                    }
                }
                painter.line_segment([pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)], Stroke::new(1.0, Color32::from_gray(48)));
                let curve = |n: &Node| -> Vec<Pos2> {
                    let mut u = EqUnit::default();
                    u.set(&n.p, fs);
                    (0..120).map(|i| {
                        let t = i as f32 / 119.0;
                        pos2(r.left() + t * r.width(), yg(u.response_db(fs, 20.0 * 1000f32.powf(t))))
                    }).collect()
                };
                painter.add(Shape::line(curve(n), Stroke::new(1.6, if n.bypass { Color32::from_gray(90) } else { Color32::from_rgb(120, 190, 255) })));
                let mut remove: Option<usize> = None;
                if drawing {
                    for b in 0..n.p.len() / 4 {
                        let o = b * 4;
                        let t = n.p[o].round() as usize;
                        if t >= 6 {
                            continue;
                        }
                        let uses_gain = matches!(t, 0 | 1 | 2 | 5);
                        // the dots stay inside the graph, also for the extremes of the range
                        let m = 7.0 * z;
                        let c = pos2(xf(n.p[o + 1]).clamp(r.left() + m, r.right() - m), (if uses_gain { yg(n.p[o + 2]) } else { r.center().y }).clamp(r.top() + m, r.bottom() - m));
                        let hr = ui.interact(Rect::from_center_size(c, vec2(18.0 * z, 18.0 * z)), ui.scope_id().with(("eqh", n.id, b)), Sense::click_and_drag());
                        let col = EQ_PALETTE[b % EQ_PALETTE.len()];
                        painter.circle_filled(c, (if hr.hovered() || hr.dragged() { 7.0 } else { 5.5 }) * z, col);
                        painter.circle_stroke(c, 5.5 * z, Stroke::new(1.0, Color32::BLACK));
                        painter.text(c + vec2(0.0, -11.0 * z), Align2::CENTER_CENTER, format!("{}", b + 1), FontId::proportional(8.0 * z), col);
                        if hr.dragged() {
                            if let Some(q) = hr.interact_pointer_pos() {
                                n.p[o + 1] = fx(q.x).clamp(20.0, 20000.0).round();
                                if uses_gain {
                                    let g = gy(q.y);
                                    n.p[o + 2] = ((if t == 5 { g.min(0.0) } else { g }) * 10.0).round() / 10.0;
                                }
                                ch = true;
                            }
                        }
                        if hr.hovered() {
                            painter.text(pos2(r.right() - 4.0, r.top() + 8.0), Align2::RIGHT_CENTER, format!("{}  {:.0} Hz  {:+.1} dB  Q {:.2}", EQ_TYPES[t], n.p[o + 1], n.p[o + 2], n.p[o + 3]), FontId::proportional(9.5), Color32::from_gray(210));
                        }
                        if hr.secondary_clicked() && n.p.len() / 4 > 1 {
                            remove = Some(b);
                        }
                    }
                    // empty space: a click or a drag makes a new band under the pointer, which the drag then moves
                    if (resp.clicked() || resp.drag_started()) && n.p.len() / 4 < EQ_MAX {
                        if let Some(q) = resp.interact_pointer_pos() {
                            n.p.extend([0.0, fx(q.x).round(), (gy(q.y) * 10.0).round() / 10.0, 1.0]);
                            self.eq_new = Some((n.id, n.p.len() / 4 - 1));
                            ch = true;
                        }
                    }
                    if resp.dragged() {
                        if let (Some((id, b)), Some(q)) = (self.eq_new, resp.interact_pointer_pos()) {
                            if id == n.id && b < n.p.len() / 4 {
                                n.p[b * 4 + 1] = fx(q.x).round();
                                n.p[b * 4 + 2] = (gy(q.y) * 10.0).round() / 10.0;
                                ch = true;
                            }
                        }
                    } else if self.eq_new.is_some_and(|(id, _)| id == n.id) {
                        self.eq_new = None;
                    }
                }
                // the band list: type, frequency, level, width
                for b in 0..n.p.len() / 4 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let o = b * 4;
                        let (sw, _) = ui.allocate_exact_size(vec2(4.0 * z, 14.0 * z), Sense::hover());
                        ui.painter().rect_filled(sw, 2.0, EQ_PALETTE[b % EQ_PALETTE.len()]);
                        let cur = EQ_TYPES[(n.p[o].round() as usize).min(EQ_TYPES.len() - 1)];
                        egui::ComboBox::from_id_salt(("eqt", n.id, b)).width(66.0 * z).selected_text(cur).show_ui(ui, |ui| {
                            for (k, c) in EQ_TYPES.iter().enumerate() {
                                if ui.selectable_label(n.p[o].round() as usize == k, crate::lang::tr(*c)).clicked() {
                                    n.p[o] = k as f32;
                                    // a notch needs a depth: the level of the band
                                    if k == 5 && n.p[o + 2] > -1.0 {
                                        n.p[o + 2] = -24.0;
                                    }
                                    ch = true;
                                }
                            }
                        });
                        let t = n.p[o].round() as usize;
                        let nid = n.id;
                        let (lo, hi) = if t == 5 { (-60.0, 0.0) } else { (-24.0, 24.0) };
                        // (right-click a value: let it follow a signal; while it does, the live value is shown)
                        let (mut sv, f1) = followed_value(nid, o + 1, n.p[o + 1]);
                        let r1 = ui.add(egui::DragValue::new(if f1 { &mut sv } else { &mut n.p[o + 1] }).range(20.0..=20000.0).speed(5.0).suffix(crate::lang::tr(" Hz")).max_decimals(0));
                        param_port(ui, z, pos2(r1.rect.center().x, r1.rect.top() - 1.0 * z), nid, o + 1, (20.0, 20000.0, true, " Hz"));
                        ch |= r1.changed() && !f1;
                        let (mut sv2, f2) = followed_value(nid, o + 2, n.p[o + 2]);
                        let r2 = ui.add_enabled(matches!(t, 0 | 1 | 2 | 5), egui::DragValue::new(if f2 { &mut sv2 } else { &mut n.p[o + 2] }).range(lo..=hi).speed(0.1).suffix(crate::lang::tr(" dB")).max_decimals(1)).on_hover_text(if t == 5 { "Depth of the notch" } else { "Level of the band" });
                        param_port(ui, z, pos2(r2.rect.center().x, r2.rect.top() - 1.0 * z), nid, o + 2, (lo, hi, false, " dB"));
                        ch |= r2.changed() && !f2;
                        let (mut sv3, f3) = followed_value(nid, o + 3, n.p[o + 3]);
                        let r3 = ui.add(egui::DragValue::new(if f3 { &mut sv3 } else { &mut n.p[o + 3] }).range(0.1..=10.0).speed(0.02).max_decimals(2)).on_hover_text(crate::lang::tr("Q (width: higher is narrower)"));
                        param_port(ui, z, pos2(r3.rect.center().x, r3.rect.top() - 1.0 * z), nid, o + 3, (0.1, 10.0, true, ""));
                        ch |= r3.changed() && !f3;
                        if n.p.len() / 4 > 1 && crate::mapping::small_btn(ui, "x").on_hover_text(crate::lang::tr("Remove this band")).clicked() {
                            remove = Some(b);
                        }
                    });
                }
                if let Some(b) = remove {
                    n.p.drain(b * 4..b * 4 + 4);
                    ch = true;
                }
            }
            Kind::Comp | Kind::Limiter => {
                for i in 0..n.kind.info().params.len() {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 8.0 * z), Sense::hover());
                let gr = (-meter.extra).clamp(0.0, 24.0) / 24.0;
                ui.painter().rect_filled(r, 2.0, Color32::from_gray(30));
                ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * gr, r.height())), 2.0, Color32::from_rgb(235, 150, 60));
                ui.label(RichText::new(format!("gain reduction {:.1} dB", meter.extra)).small().weak());
            }
            Kind::Gate => {
                for i in 0..4 {
                    ch |= Self::param_row(ui, n, i, 56.0);
                }
                ui.label(RichText::new(if meter.extra > 0.5 { "open" } else { "closed" }).small().weak());
            }
            Kind::Mixer => {
                let cnt = n.inputs();
                // the number of inputs and the master level come first, so that adding an input moves nothing the
                // pointer is on; the rows below line up with the input dots (no extra spacing between them)
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0 * z);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), ROW * z), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_height(ROW * z);
                    ui.label(RichText::new(crate::lang::tr("Inputs")).small().color(Color32::from_rgb(160, 172, 190)));
                    let mut c = cnt;
                    if spinner(ui, ("mixn", n.id), &mut c, 1, MIX_MAX, z) {
                        n.p[0] = c as f32;
                        ch = true;
                    }
                });
                ui.allocate_ui_with_layout(vec2(ui.available_width(), ROW * z), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_height(ROW * z);
                    ch |= Self::param_row(ui, n, 1, 44.0);
                });
                let cnt = n.inputs();
                for k in 0..cnt {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW * z), Sense::hover());
                    ui.scope_builder(UiBuilder::new().max_rect(row.shrink2(vec2(10.0 * z, 2.0 * z))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(format!("{}", k + 1)).small().weak());
                        ui.spacing_mut().slider_width = 100.0 * z;
                        let muted = n.p[2 + MIX_MAX + k] >= 0.5;
                        let t = crate::mapping::Target::slider(format!("node:{}:{}", n.id, 2 + k), format!("Mixer #{}: input {} gain", n.id, k + 1), -60.0, 12.0, false, " dB");
                        let nid = n.id;
                        let (mut shown, followed) = followed_value(nid, 2 + k, n.p[2 + k]);
                        ui.add_enabled_ui(!muted, |ui| {
                            if followed {
                                crate::slider::bound(ui, &mut shown, -60.0, 12.0, false, 0.0, &t, crate::slider::Fmt::new(" dB", 1), |_| {});
                            } else {
                                ch |= crate::slider::bound(ui, &mut n.p[2 + k], -60.0, 12.0, false, 0.0, &t, crate::slider::Fmt::new(" dB", 1), |_| {});
                            }
                        });
                        // (at the right edge: the left edge has the input dot of the row)
                        param_port(ui, z, pos2(ui.max_rect().right() + 10.0 * z, ui.min_rect().center().y), nid, 2 + k, (-60.0, 12.0, false, " dB"));
                        let mb = ui.selectable_label(muted, "M").on_hover_text(crate::lang::tr("Mute this input"));
                        crate::mapping::button_menu(&mb, &crate::mapping::Target::toggle(format!("mixm:{}:{}", n.id, k), format!("Mixer #{}: mute input {}", n.id, k + 1), true));
                        if mb.clicked() {
                            n.p[2 + MIX_MAX + k] = if muted { 0.0 } else { 1.0 };
                            ch = true;
                        }
                    });
                }
            }
            Kind::Distortion => {
                let old = n.param(1).round() as usize;
                for i in [1, 0, 2, 5, 6, 7, 3, 4] {
                    if n.shows(i) {
                        ch |= Self::param_row(ui, n, i, 50.0);
                    }
                }
                let new = n.param(1).round() as usize;
                if new != old {
                    // the harmonics are meant to be added at the level of the signal, the clippers are turned down
                    if new == DIST_HARMONIC && (n.p[4] + 6.0).abs() < 0.01 {
                        n.p[4] = 0.0;
                    } else if old == DIST_HARMONIC && n.p[4].abs() < 0.01 {
                        n.p[4] = -6.0;
                    }
                }
                if new == DIST_HARMONIC {
                    ui.label(RichText::new(crate::lang::tr("a series of harmonics of the signal, relative to its level")).small().weak());
                }
            }
            Kind::Convolver => {
                let name = if n.s.is_empty() { "none".to_string() } else { std::path::Path::new(&n.s).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| n.s.clone()) };
                // buttons on one row, the file name on its own (cut to the width of the node), the status wraps
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if crate::mapping::btn(ui, "Load IR...").on_hover_text(crate::lang::tr("A room, a cabinet or a measured impulse response (also one saved from the Impulse window)")).clicked() {
                        self.ir_dlg = Some((n.id, crate::tools::dialogs::open("Impulse response", "Audio", crate::tools::decode::EXTENSIONS)));
                    }
                    if !n.s.is_empty() && crate::mapping::btn(ui, "Remove").on_hover_text(crate::lang::tr("Remove the impulse response")).clicked() {
                        n.s.clear();
                        ch = true;
                    }
                });
                ui.add(egui::Label::new(RichText::new(name).small().weak()).truncate()).on_hover_text(&n.s);
                for i in 0..2 {
                    ch |= Self::param_row(ui, n, i, 40.0);
                }
                if let Some(s) = status {
                    ui.add(egui::Label::new(RichText::new(s).small().color(Color32::from_gray(120))).wrap());
                }
            }
            _ => {
                for i in 0..n.kind.info().params.len() {
                    if n.shows(i) {
                        ch |= Self::param_row(ui, n, i, 56.0);
                    }
                }
                if n.kind == Kind::Phase && n.param(1).round() as usize == 3 {
                    ch |= phase_curve(ui, n, z);
                }
                if matches!(n.kind, Kind::Phase | Kind::NoiseRed | Kind::Shifter) {
                    if let Some(s) = status.filter(|s| !s.is_empty()) {
                        ui.label(RichText::new(s).small().color(Color32::from_gray(120)));
                    }
                }
            }
        }
        // output level
        if n.kind != Kind::Analyzer || true {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 7.0 * z), Sense::hover());
            let p = ui.painter();
            for (c, pk) in meter.peak.iter().enumerate() {
                let y = r.top() + c as f32 * 4.0 * z;
                let db = 20.0 * pk.max(1e-6).log10();
                let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let rr = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), 3.0 * z));
                p.rect_filled(rr, 1.0, Color32::from_gray(30));
                p.rect_filled(Rect::from_min_size(rr.min, vec2(rr.width() * t, 3.0 * z)), 1.0, if db > -1.0 { Color32::from_rgb(235, 70, 70) } else if db > -12.0 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(70, 200, 120) });
            }
        }
        ch
    }
}
