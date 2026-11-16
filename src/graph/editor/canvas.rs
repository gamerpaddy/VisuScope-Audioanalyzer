//! The canvas of the node editor: nodes, wires, dragging, selection, zoom and the menus on them.

use super::*;

impl Routing {
    pub(super) fn canvas(&mut self, ui: &mut Ui, cx: &Ctx, canvas: Rect) {
        let origin = canvas.min + self.pan;
        // nodes that were saved on top of each other (or grew over their neighbours) are moved apart once their sizes are known
        if self.tidy_pending && self.g.nodes.iter().all(|n| self.sizes.contains_key(&n.id)) {
            self.tidy_pending = false;
            self.tidy();
            self.push();
        }
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, CANVAS_BG);
        // dot grid
        let step = (24.0 * self.zoom).max(10.0);
        let (ox, oy) = (self.pan.x.rem_euclid(step), self.pan.y.rem_euclid(step));
        let mut y = canvas.top() + oy;
        while y < canvas.bottom() {
            let mut x = canvas.left() + ox;
            while x < canvas.right() {
                painter.circle_filled(pos2(x, y), 0.8, Color32::from_gray(42));
                x += step;
            }
            y += step;
        }
        let bg = ui.interact(canvas, ui.scope_id().with("routing_bg"), Sense::click_and_drag());
        let shift = ui.input(|i| i.modifiers.shift);
        let hand = ui.input(|i| i.modifiers.alt || i.key_down(egui::Key::Space));
        // a drag on the background selects with a band; Alt or Space with the drag (or the middle or right button) moves the view
        if bg.dragged_by(egui::PointerButton::Primary) && self.wire.is_none() {
            if !hand {
                if self.band.is_none() {
                    self.band = ui.input(|i| i.pointer.press_origin());
                }
            } else {
                self.pan += bg.drag_delta();
            }
        }
        if bg.dragged_by(egui::PointerButton::Middle) || bg.dragged_by(egui::PointerButton::Secondary) {
            self.pan += bg.drag_delta();
        }
        // the wheel only zooms the node window, about the pointer, wherever it is (also over a node and over the graph of an
        // equaliser); popups on top keep their own wheel. A pinch on a touchpad (or a touch screen) and Ctrl + wheel zoom the same way.
        let over = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p) && ui.ctx().layer_id_at(*p) == Some(ui.layer_id()));
        if let Some(p) = over {
            let (sc, alt, pinch) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.alt, i.zoom_delta()));
            let mut factor = if pinch.is_finite() && pinch > 0.0 { pinch } else { 1.0 };
            if sc != egui::Vec2::ZERO {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                if alt {
                    // Alt + wheel (or a horizontal wheel) moves the view
                    self.pan += sc;
                } else if sc.y != 0.0 {
                    factor *= (sc.y * 0.0025).exp();
                } else {
                    self.pan += sc;
                }
            }
            if (factor - 1.0).abs() > 1e-4 {
                // the spot under the pointer stays where it is
                let m = (p - origin) / self.zoom;
                self.zoom = (self.zoom * factor).clamp(0.15, 1.8);
                self.pan = p - canvas.min - m * self.zoom;
            }
        }
        // node coordinates <-> screen
        let (origin, z) = (canvas.min + self.pan, self.zoom);
        let sp = move |m: Pos2| origin + m.to_vec2() * z;
        // the listening probe follows the pointer (not while a wire, a band or the background is being dragged)
        let probing = if self.probe_on && self.wire.is_none() && self.band.is_none() && !bg.dragged() { over.and_then(|p| self.probe_target(p, &sp, z)) } else { None };
        self.probe_body = probing.is_some_and(|t| t.1);
        self.set_probe(probing.map(|t| t.0));
        let mp = move |s: Pos2| ((s - origin) / z).to_pos2();
        if bg.clicked() {
            self.sel.clear();
            self.sel_wire = None;
        }
        let pointer = ui.input(|i| i.pointer.interact_pos());

        // wires first (under the nodes)
        let mut wire_hit: Option<(NodeId, u8)> = None;
        let metersum: HashMap<NodeId, f32> = self.eng.as_ref().map(|e| self.g.nodes.iter().map(|n| (n.id, e.meter(n.id).peak[0].max(e.meter(n.id).peak[1]))).collect()).unwrap_or_default();
        for e in &self.g.edges {
            let (Some(a), Some(b)) = (self.g.node(e.from), self.g.node(e.to)) else { continue };
            let pts = wire_pts(sp(out_pos(a, e.from_port as usize)), sp(in_pos(b, e.port as usize)));
            let lvl = metersum.get(&e.from).copied().unwrap_or(0.0);
            let live = ((20.0 * lvl.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
            let selected = self.sel_wire == Some((e.to, e.port));
            let near = pointer.is_some_and(|p| canvas.contains(p) && wire_distance(&pts, p) < 7.0);
            if near && self.wire.is_none() {
                wire_hit = Some((e.to, e.port));
            }
            let col = if selected { Color32::from_rgb(255, 214, 80) } else if near { Color32::from_rgb(200, 220, 255) } else { Color32::from_rgb(80, 120, 170).gamma_multiply(0.6 + 0.4 * live) };
            painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, Stroke::new(if selected || near { 3.0 } else { 2.2 }, col))));
        }
        // a parameter that follows a signal: a dashed line from the signal to the node
        for m in &self.g.mods {
            let (Some(a), Some(b)) = (self.g.node(m.src), self.g.node(m.node)) else { continue };
            let bw = self.sizes.get(&b.id).map_or(node_w(b.kind), |s| s.0);
            // to the port of the parameter row (where the row was drawn last), else to the top of the node
            let row = MOD_ROWS.lock().iter().find(|r| r.0 == (m.node, m.param)).map(|r| r.1);
            let pts = wire_pts(sp(out_pos(a, 0)), row.unwrap_or_else(|| sp(pos2(b.x + bw * 0.5, b.y))));
            let line: Vec<Pos2> = (0..=24).map(|k| bezier_at(&pts, k as f32 / 24.0)).collect();
            painter.extend(Shape::dashed_line(&line, Stroke::new(1.6, Color32::from_rgb(255, 170, 70).gamma_multiply(0.8)), 6.0, 4.0));
        }
        if let Some((to, port)) = wire_hit
            && (bg.clicked() || ui.input(|i| i.pointer.primary_clicked())) {
                self.sel_wire = Some((to, port));
                self.sel.clear();
            }
        // a right-click picks the wire under the pointer (or none) for the menu
        if bg.secondary_clicked() {
            self.sel_wire = wire_hit;
        }
        let mut changed_graph = false;
        bg.context_menu(|ui| {
            let at = pointer.map(mp).unwrap_or(pos2(100.0, 100.0));
            if let Some((to, port)) = self.sel_wire {
                if crate::mapping::btn(ui, "Insert a node here...").clicked() {
                    self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::Insert(to, port), focused: false });
                    ui.close();
                }
                // a line that carries one channel already (after a split node, for instance) has nothing to split
                let mono = self.g.edge_into(to, port).is_some_and(|e| self.g.carries_mono(e.from, 0));
                if crate::mapping::btn(ui, "Split here into two stereo lines").on_hover_text(crate::lang::tr("A node with two outputs, both the whole stereo signal: the first goes on to the same place, the second is free to connect anywhere")).clicked() {
                    let place = self.free_place(at, Kind::Dual);
                    if let Some(s) = self.g.split_wire_as(Kind::Dual, to, port, place.x, place.y) {
                        self.sel.clear();
                        self.sel.insert(s);
                        self.sel_wire = None;
                        changed_graph = true;
                        self.note = "Two lines: the first goes on to the old target, connect the second where you want it".into();
                    }
                    ui.close();
                }
                if !mono && crate::mapping::btn(ui, "Split here into left and right channels").on_hover_text(crate::lang::tr("A split node in this line: the left channel goes on to the same place, the right output is free to connect anywhere")).clicked() {
                    let place = self.free_place(at, Kind::Split);
                    if let Some(s) = self.g.split_wire(to, port, place.x, place.y) {
                        self.sel.clear();
                        self.sel.insert(s);
                        self.sel_wire = None;
                        changed_graph = true;
                        self.note = "Split: the left channel goes on to the old target, connect the right output where you want it".into();
                    }
                    ui.close();
                }
                if crate::mapping::btn(ui, "Merge here with another line").on_hover_text(crate::lang::tr("A merge node in this line: this line goes into its left input, connect a second line to the right input")).clicked() {
                    let place = self.free_place(at, Kind::Merge);
                    if let Some(s) = self.g.split_wire_as(Kind::Merge, to, port, place.x, place.y) {
                        self.sel.clear();
                        self.sel.insert(s);
                        self.sel_wire = None;
                        changed_graph = true;
                        self.note = "Merge: this line is the left input, connect the second line to the right input".into();
                    }
                    ui.close();
                }
                if crate::mapping::btn(ui, "Disconnect").clicked() {
                    self.g.disconnect(to, port);
                    self.sel_wire = None;
                    changed_graph = true;
                    ui.close();
                }
                ui.separator();
            }
            if crate::mapping::btn(ui, "Add node...  (Space)").clicked() {
                self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::None, focused: false });
                ui.close();
            }
            if crate::mapping::btn(ui, "Fit all nodes").clicked() {
                self.fit_next = true;
                ui.close();
            }
        });

        // nodes
        let mut dragged: Option<(NodeId, egui::Vec2)> = None;
        let mut clicked: Option<(NodeId, bool)> = None;
        let mut remove: Option<NodeId> = None;
        let mut port_drag: Option<(WireDrag, bool)> = None; // (what, released)
        let ids: Vec<NodeId> = self.g.nodes.iter().map(|n| n.id).collect();
        let loops = self.feedback_nodes(cx);
        for id in ids {
            let Some(idx) = self.g.nodes.iter().position(|n| n.id == id) else { continue };
            let mut n = self.g.nodes[idx].clone();
            let w = node_w(n.kind) * z;
            let (hdr, row) = (HDR * z, ROW * z);
            let info = n.kind.info();
            let top_left = sp(pos2(n.x, n.y));
            let selected = self.sel.contains(&id);
            let bg_idx = painter.add(Shape::Noop);
            let body_rect = Rect::from_min_size(top_left + vec2(0.0, hdr), vec2(w, 600.0 * z));
            let mut used_h = 0.0;
            let mut body_changed = false;
            if body_rect.top() < canvas.bottom() + 600.0 && top_left.x < canvas.right() && top_left.x + w > canvas.left() {
                let (changed, h) = {
                    let mut child = ui.new_child(UiBuilder::new().max_rect(body_rect.shrink2(vec2(10.0 * z, 5.0 * z))).layout(egui::Layout::top_down(egui::Align::LEFT)));
                    child.set_clip_rect(canvas);
                    scale_style(child.style_mut(), z);
                    let c = self.body(&mut child, cx, &mut n, loops.contains(&id));
                    // nothing may reach past the node: (a test hook, VISUSCOPE_NODECHECK=1, logs the kinds that do)
                    if child.min_rect().width() > w - 8.0 * z && std::env::var_os("VISUSCOPE_NODECHECK").is_some() {
                        crate::log::log(&format!("node overflow: {:?} is {:.0} wide in a node of {:.0}", n.kind, child.min_rect().width(), w));
                    }
                    (c, child.min_rect().height() + 10.0 * z)
                };
                body_changed = changed;
                used_h = h;
            }
            let full = Rect::from_min_size(top_left, vec2(w, hdr + used_h.max(row)));
            if body_changed && n.kind == Kind::Eq {
                // a band was added or removed: the node grows or shrinks
                self.tidy_pending = true;
            }
            self.sizes.insert(id, (w / z, full.height() / z));
            // header
            let header = Rect::from_min_size(top_left, vec2(w, hdr));
            let (r, g, b) = info.color;
            let hcol = Color32::from_rgb(r, g, b);
            painter.set(
                bg_idx,
                Shape::Vec(vec![
                    Shape::rect_filled(full, 6.0, BODY_BG),
                    Shape::rect_filled(header, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, if n.bypass { Color32::from_gray(70) } else { hcol.gamma_multiply(if selected { 1.0 } else { 0.78 }) }),
                    Shape::rect_stroke(full, 6.0, Stroke::new(if selected { 1.8 } else { 1.0 }, if selected { Color32::from_rgb(235, 240, 250) } else { Color32::from_rgb(52, 60, 76) }), egui::StrokeKind::Inside),
                ]),
            );
            let title = match n.kind {
                Kind::Input => "Input".to_string(),
                k => crate::lang::tr(k.name()).to_string(),
            };
            painter.text(header.min + vec2(10.0 * z, hdr * 0.5), Align2::LEFT_CENTER, title, FontId::proportional(12.0 * z), Color32::from_rgb(10, 14, 20));
            // header buttons: bypass, remove
            let hb = ui.interact(Rect::from_min_max(header.min, pos2(header.max.x - 44.0 * z, header.max.y)), ui.scope_id().with(("nh", id)), Sense::click_and_drag());
            if info.inputs > 0 && info.outputs > 0 {
                let br = Rect::from_center_size(pos2(header.max.x - 34.0 * z, header.center().y), vec2(14.0 * z, 14.0 * z));
                let r = ui.interact(br, ui.scope_id().with(("nb", id)), Sense::click());
                painter.rect_filled(br, 3.0, if n.bypass { Color32::from_rgb(235, 190, 70) } else { Color32::from_black_alpha(60) });
                painter.text(br.center(), Align2::CENTER_CENTER, "B", FontId::proportional(9.5 * z), if n.bypass { Color32::BLACK } else { Color32::from_gray(210) });
                if r.on_hover_text(crate::lang::tr("Bypass this node")).clicked() {
                    n.bypass = !n.bypass;
                    body_changed = true;
                }
            }
            let xr = Rect::from_center_size(pos2(header.max.x - 12.0 * z, header.center().y), vec2(14.0 * z, 14.0 * z));
            let xrr = ui.interact(xr, ui.scope_id().with(("nx", id)), Sense::click());
            painter.text(xr.center(), Align2::CENTER_CENTER, "x", FontId::proportional(12.0 * z), if xrr.hovered() { Color32::WHITE } else { Color32::from_gray(30) });
            if xrr.on_hover_text(crate::lang::tr("Remove the node")).clicked() {
                remove = Some(id);
            }
            if hb.dragged() {
                dragged = Some((id, hb.drag_delta() / z));
            }
            if hb.clicked() || hb.drag_started() {
                clicked = Some((id, shift));
            }
            hb.context_menu(|ui| {
                if crate::mapping::btn(ui, "Duplicate").clicked() {
                    let (x, y) = (n.x + 30.0, n.y + 30.0);
                    let k = n.kind;
                    let nn = self.g.add(k, x, y);
                    if nn != n.id {
                        if let Some(m) = self.g.node_mut(nn) {
                            m.p = n.p.clone();
                            m.s = n.s.clone();
                            m.bypass = n.bypass;
                        }
                        changed_graph = true;
                    }
                    ui.close();
                }
                if crate::mapping::btn(ui, "Remove").clicked() {
                    remove = Some(id);
                    ui.close();
                }
            });
            // ports
            // a port carries a stereo pair: the dot shows the left half in cyan and the right half in pink;
            // the ports of split / merge nodes carry one channel and are drawn in that channel's colour
            let dot = |ui: &mut Ui, c: Pos2, tag: (u8, NodeId, u8), wired: bool, lr: u8| {
                let pr = PORT_R * z.max(0.7);
                let rr = Rect::from_center_size(c, vec2(pr * 3.2, pr * 3.2));
                let r = ui.interact(rr, ui.scope_id().with(("port", tag)), Sense::click_and_drag());
                let (lc, rc) = (Color32::from_rgb(70, 200, 255), Color32::from_rgb(255, 100, 160));
                let k = if r.hovered() || wired { 1.0 } else { 0.55 };
                painter.circle_filled(c, pr + 0.5, Color32::from_rgb(20, 24, 32));
                match lr {
                    1 => {
                        painter.circle_filled(c, pr, lc.gamma_multiply(k));
                    }
                    2 => {
                        painter.circle_filled(c, pr, rc.gamma_multiply(k));
                    }
                    _ => {
                        half_dot(&painter, c, pr, true, lc.gamma_multiply(k));
                        half_dot(&painter, c, pr, false, rc.gamma_multiply(k));
                    }
                }
                painter.circle_stroke(c, pr, Stroke::new(if r.hovered() { 2.0 } else { 1.2 }, if r.hovered() { Color32::WHITE } else { Color32::from_rgb(150, 175, 215) }));
                r.on_hover_text(match lr {
                    1 => "Left channel",
                    2 => "Right channel",
                    _ => "Stereo: left (cyan half) and right (pink half) travel together in one wire",
                })
            };
            for p in 0..n.inputs() {
                let c = sp(in_pos(&n, p));
                let wired = self.g.edge_into(id, p as u8).is_some();
                let lr = if n.kind == Kind::Merge || (n.kind == Kind::Output && n.inputs() == 2) { 1 + p as u8 } else { 0 };
                let r = dot(ui, c, (0, id, p as u8), wired, lr);
                if r.drag_started() {
                    // dragging a connected input moves its wire; an empty one starts a new wire backwards
                    port_drag = Some(match self.g.edge_into(id, p as u8).copied() {
                        Some(e) => {
                            self.g.disconnect(id, p as u8);
                            changed_graph = true;
                            (WireDrag::FromOut(e.from, e.from_port), false)
                        }
                        None => (WireDrag::FromIn(id, p as u8), false),
                    });
                }
            }
            for op in 0..n.outputs() {
                let c = sp(out_pos(&n, op));
                let lr = if n.kind == Kind::Split { 1 + op as u8 } else { 0 };
                let r = dot(ui, c, (1, id, op as u8), self.g.edges.iter().any(|e| e.from == id && e.from_port as usize == op), lr);
                if r.drag_started() {
                    port_drag = Some((WireDrag::FromOut(id, op as u8), false));
                }
                let mono = self.g.carries_mono(id, 0);
                r.context_menu(|ui| {
                    if crate::mapping::btn(ui, "Split here into two stereo lines").on_hover_text(crate::lang::tr("A node with two outputs that both carry the whole stereo signal")).clicked() {
                        let at = self.free_place(pos2(n.x + node_w(n.kind) + 40.0, n.y + 60.0), Kind::Dual);
                        if let Some(s) = self.g.split_output_as(Kind::Dual, id, op as u8, at.x, at.y) {
                            self.sel.clear();
                            self.sel.insert(s);
                            changed_graph = true;
                        }
                        ui.close();
                    }
                    if mono {
                        return;
                    }
                    if crate::mapping::btn(ui, "Split here into left and right channels").on_hover_text(crate::lang::tr("A split node on this output: its two outputs carry the left and the right channel, to route them one by one")).clicked() {
                        let at = self.free_place(pos2(n.x + node_w(n.kind) + 40.0, n.y + 60.0), Kind::Split);
                        if let Some(s) = self.g.split_output(id, op as u8, at.x, at.y) {
                            self.sel.clear();
                            self.sel.insert(s);
                            changed_graph = true;
                        }
                        ui.close();
                    }
                });
            }
            if body_changed {
                let still = n.inputs();
                self.g.nodes[idx] = n;
                self.g.edges.retain(|e| e.to != id || (e.port as usize) < still);
                changed_graph = true;
            }
        }
        if let Some((w, _)) = port_drag {
            self.wire = Some(w);
        }

        // moving, selecting, removing
        if let Some((id, d)) = clicked {
            if shift {
                if !self.sel.remove(&id) {
                    self.sel.insert(id);
                }
            } else if !self.sel.contains(&id) {
                self.sel.clear();
                self.sel.insert(id);
            }
            self.sel_wire = None;
            // bring to front
            if let Some(pos) = self.g.nodes.iter().position(|n| n.id == id) {
                let n = self.g.nodes.remove(pos);
                self.g.nodes.push(n);
            }
            let _ = d;
        }
        if let Some((id, d)) = dragged {
            let moving: Vec<NodeId> = if self.sel.contains(&id) { self.sel.iter().copied().collect() } else { vec![id] };
            for m in moving {
                if let Some(n) = self.g.node_mut(m) {
                    n.x += d.x;
                    n.y += d.y;
                }
            }
            self.dirty = true;
        }
        if let Some(id) = remove {
            self.g.remove_bridged(id);
            self.sel.remove(&id);
            changed_graph = true;
        }
        // rubber band
        if let Some(a) = self.band {
            if let Some(p) = pointer {
                let r = Rect::from_two_pos(a, p);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(140, 185, 255)), egui::StrokeKind::Inside);
                painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 160, 255, 24));
                if ui.input(|i| i.pointer.any_released()) {
                    if !ui.input(|i| i.modifiers.shift) {
                        self.sel.clear();
                    }
                    for n in &self.g.nodes {
                        let nr = Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(node_w(n.kind), 60.0) * z);
                        if r.intersects(nr) {
                            self.sel.insert(n.id);
                        }
                    }
                    self.band = None;
                }
            } else {
                self.band = None;
            }
        }

        // wire being dragged
        if let (Some(w), Some(p)) = (self.wire, pointer) {
            let (a, b) = match w {
                WireDrag::FromOut(id, op) => (self.g.node(id).map(|n| sp(out_pos(n, op as usize))), p),
                WireDrag::FromIn(id, port) => (Some(p), self.g.node(id).map_or(p, |n| sp(in_pos(n, port as usize)))),
            };
            if let Some(a) = a {
                painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(wire_pts(a, b), false, Color32::TRANSPARENT, Stroke::new(2.4, Color32::from_rgb(255, 214, 80)))));
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.wire = None;
                // the dots of a mixer are a row apart (22 pixels at full size): the radius is smaller than half of that
                let radius = (15.0 * z.max(0.6)).min(ROW * z * 0.48);
                match w {
                    WireDrag::FromOut(from, fport) => {
                        // onto the control input of a value?
                        let port_hit = MOD_ROWS.lock().iter().filter(|(k, _)| k.0 != from).map(|(k, pos)| (pos.distance(p), *k)).filter(|d| d.0 < 11.0 * z.max(0.7)).min_by(|a, b| a.0.total_cmp(&b.0)).map(|d| d.1);
                        if let Some((to, param)) = port_hit {
                            if let Some((lo, hi, log, _)) = self.g.node(to).and_then(|n| param_range(n, param)) {
                                let switch = self.g.node(to).is_some_and(|n| param_range(n, param).is_some_and(|r| r.3 == "switch"));
                                let (in_lo, in_hi) = if switch { (-0.05, 0.05) } else { (-1.0, 1.0) };
                                let m = Modulation { node: to, param, src: from, in_lo, in_hi, out_lo: lo, out_hi: hi, log };
                                if self.g.set_modulation(m) {
                                    changed_graph = true;
                                } else {
                                    self.note = "That cannot control this value".into();
                                }
                            }
                        } else {
                        let cands: Vec<(NodeId, u8, Pos2)> = self.g.nodes.iter().flat_map(|n| (0..n.inputs()).map(move |k| (n.id, k as u8, sp(in_pos(n, k))))).collect();
                        let target = pick_port(&cands, p, radius);
                        // dropped on a node body: first free input
                        let target = target.or_else(|| {
                            self.g.nodes.iter().find_map(|n| {
                                let r = Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(node_w(n.kind), HDR + 20.0) * z);
                                if r.contains(p) && n.id != from {
                                    (0..n.inputs()).find(|&k| self.g.edge_into(n.id, k as u8).is_none()).map(|k| (n.id, k as u8))
                                } else {
                                    None
                                }
                            })
                        });
                        match target {
                            Some((to, port)) => {
                                if let Err(m) = self.g.connect_from(from, fport, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: mp(p), text: String::new(), sel: 0, link: Link::FromOut(from, fport), focused: false });
                            }
                            None => {}
                        }
                        }
                    }
                    WireDrag::FromIn(to, port) => {
                        let cands: Vec<(NodeId, u8, Pos2)> = self.g.nodes.iter().flat_map(|n| (0..n.outputs()).map(move |k| (n.id, k as u8, sp(out_pos(n, k))))).collect();
                        let src = pick_port(&cands, p, radius);
                        match src {
                            Some((from, fport)) => {
                                if let Err(m) = self.g.connect_from(from, fport, to, port) {
                                    self.note = m.into();
                                }
                                changed_graph = true;
                            }
                            None if canvas.contains(p) => {
                                self.search = Some(Search { at: mp(p) - vec2(260.0, 0.0), text: String::new(), sel: 0, link: Link::ToIn(to, port), focused: false });
                            }
                            None => {}
                        }
                    }
                }
            }
        } else if self.wire.is_some() && ui.input(|i| i.pointer.any_released()) {
            self.wire = None;
        }

        // keyboard: Space = search, Delete = remove, F = fit
        let typing = ui.ctx().egui_wants_keyboard_input();
        if self.search.is_none() && !typing && ui.rect_contains_pointer(canvas) {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
                let at = pointer.map(mp).unwrap_or(pos2(100.0, 100.0));
                self.search = Some(Search { at, text: String::new(), sel: 0, link: Link::None, focused: false });
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
                if let Some((to, port)) = self.sel_wire.take() {
                    self.g.disconnect(to, port);
                    changed_graph = true;
                }
                let gone: Vec<NodeId> = self.sel.drain().collect();
                for id in gone {
                    self.g.remove_bridged(id);
                    changed_graph = true;
                }
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y)) {
                self.redo_graph();
            } else if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
                self.undo_graph();
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)) {
                self.fit_next = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::P)) {
                self.probe_on = !self.probe_on;
            }
        }
        // where the probe listens: a ring around the dot, or an outline around the node
        if let Some(pr) = self.probe_hover.filter(|_| self.probe_on)
            && let Some(n) = self.g.node(pr.node) {
                let col = Color32::from_rgb(120, 220, 230);
                if self.probe_body {
                    let (w, h) = self.sizes.get(&n.id).copied().unwrap_or((node_w(n.kind), 150.0));
                    painter.rect_stroke(Rect::from_min_size(sp(pos2(n.x, n.y)), vec2(w * z, h * z)).expand(2.0), 6.0, Stroke::new(2.0, col), egui::StrokeKind::Outside);
                } else {
                    let c = sp(if pr.input { in_pos(n, pr.port as usize) } else { out_pos(n, pr.port as usize) });
                    painter.circle_stroke(c, 9.0 * z, Stroke::new(2.5, col));
                }
            }
        self.search_ui(ui, cx, canvas);
        if self.fit_next && !self.g.nodes.is_empty() {
            self.fit_next = false;
            let (mut lo, mut hi) = (pos2(f32::MAX, f32::MAX), pos2(f32::MIN, f32::MIN));
            for n in &self.g.nodes {
                lo = pos2(lo.x.min(n.x), lo.y.min(n.y));
                hi = pos2(hi.x.max(n.x + node_w(n.kind)), hi.y.max(n.y + 120.0));
            }
            let centre = (lo.to_vec2() + hi.to_vec2()) * 0.5;
            // zoom out when everything does not fit, but never in beyond 100 %
            let size = (hi - lo).max(vec2(1.0, 1.0));
            if std::env::var_os("VISUSCOPE_ZOOM").is_none() {
                self.zoom = ((canvas.width() / size.x).min(canvas.height() / (size.y + 40.0)) * 0.92).clamp(0.15, 1.0);
            }
            self.pan = canvas.size() * 0.5 - centre * self.zoom;
        }
        if changed_graph {
            self.push();
        }
    }
}
