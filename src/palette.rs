//! Command palette (Ctrl+K): type a few letters of any action (a window, a layout, the generator, the timeline transport ...)
//! and press Enter. The actions are the ones that can be bound to keys and controllers in Options.

use crate::app::App;
use eframe::egui::{self, Key, Modifiers};

#[derive(Clone, Default)]
struct State {
    open: bool,
    q: String,
    sel: usize,
    focus: bool,
}

/// Every word of `q` occurs in `label` (case does not matter).
pub fn matches(label: &str, q: &str) -> bool {
    let l = label.to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| l.contains(w))
}

impl App {
    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        let id = egui::Id::new("command_palette");
        let mut st: State = ctx.data_mut(|d| d.get_temp(id)).unwrap_or_default();
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
            st = State { open: !st.open, focus: true, ..Default::default() };
        }
        if !st.open {
            ctx.data_mut(|d| d.insert_temp(id, st));
            return;
        }
        let acts: Vec<(String, String)> = crate::mapping::actions().into_iter().filter(|a| a.slider.is_none() && matches(&a.label, &st.q)).map(|a| (a.id, a.label)).take(14).collect();
        let (up, down, enter, esc) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        if down {
            st.sel += 1;
        }
        if up {
            st.sel = st.sel.saturating_sub(1);
        }
        st.sel = st.sel.min(acts.len().saturating_sub(1));
        let mut run: Option<String> = None;
        egui::Window::new(crate::lang::tr("Command palette")).id(id).title_bar(false).resizable(false).collapsible(false).anchor(egui::Align2::CENTER_TOP, [0.0, 70.0]).fixed_size([420.0, 0.0]).show(ctx, |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut st.q).hint_text(crate::lang::tr("Type a command: a window, a layout, play, record ...")).desired_width(f32::INFINITY));
            if st.focus {
                r.request_focus();
                st.focus = false;
            }
            if r.changed() {
                st.sel = 0;
            }
            for (k, (aid, label)) in acts.iter().enumerate() {
                if ui.selectable_label(k == st.sel, label).clicked() {
                    run = Some(aid.clone());
                }
            }
            if acts.is_empty() {
                ui.weak(crate::lang::tr("nothing found"));
            }
            ui.weak(crate::lang::tr("Enter runs, Esc closes, Ctrl+K opens"));
        });
        if enter {
            run = acts.get(st.sel).map(|a| a.0.clone());
        }
        if esc || run.is_some() {
            st.open = false;
        }
        ctx.data_mut(|d| d.insert_temp(id, st));
        if let Some(a) = run {
            self.run_action(ctx, &a, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn words_in_any_order() {
        assert!(super::matches("Timeline: play / pause", "play time"));
        assert!(!super::matches("Timeline: play / pause", "record"));
        assert!(crate::mapping::actions().iter().any(|a| super::matches(&a.label, "speaker window")));
    }
}
