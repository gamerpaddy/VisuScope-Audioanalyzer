//! Right-click on a slider puts it back to its default; a slider that can be bound to an input has a menu on its value box.
//! (A widget laid over the value box would take the clicks that start typing a number: the box must be the real thing.)

use crate::mapping::Target;
use eframe::egui::{self, pos2, Rect, Response, Sense, Ui};

/// The part of a slider that is the track: its value box (about `VALUE_W` wide, at the right end) is left alone.
const VALUE_W: f32 = 80.0;

/// Call right after adding a slider that shows its own value: true when `v` was reset (right-click on the track).
pub fn reset<T>(ui: &Ui, r: &Response, v: &mut T, def: T) -> bool {
    let track = Rect::from_min_max(r.rect.min, pos2((r.rect.right() - VALUE_W).max(r.rect.left() + 30.0), r.rect.max.y));
    if ui.interact(track, r.id.with("rc_reset"), Sense::click()).secondary_clicked() {
        *v = def;
        true
    } else {
        false
    }
}

/// How the value box shows its number.
#[derive(Clone, Copy)]
pub struct Fmt {
    pub suffix: &'static str,
    pub decimals: usize,
    /// microseconds up to milliseconds ("120 us", "5.0 ms"; typing "5 ms" works)
    pub us: bool,
    /// whole numbers
    pub int: bool,
}

impl Fmt {
    pub fn new(suffix: &'static str, decimals: usize) -> Fmt {
        Fmt { suffix, decimals, us: false, int: false }
    }

    fn apply(self, x: egui::DragValue<'_>) -> egui::DragValue<'_> {
        if self.us {
            x.custom_formatter(|v, _| if v >= 1000.0 { format!("{:.1} ms", v / 1000.0) } else { format!("{v:.0} us") }).custom_parser(|t| {
                let t = t.trim().to_lowercase();
                let ms = t.ends_with("ms");
                t.trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace()).parse::<f64>().ok().map(|v| if ms { v * 1000.0 } else { v })
            })
        } else if self.int {
            x.max_decimals(0).speed(0.1)
        } else {
            x.suffix(self.suffix).max_decimals(self.decimals)
        }
    }
}

/// A context menu that stays open while you click inside it (to type a value into a field): it closes on a click outside, or when
/// an entry calls `ui.close()`.
pub fn context_menu(r: &Response, add: impl FnOnce(&mut Ui)) {
    egui::Popup::context_menu(r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(add);
}

/// A slider with a value box of its own (so that clicking the box types a number), that can be bound to an input: right-click the
/// track to reset it, right-click the value box for the menu (reset, `extra`, the input binding). `fmt` says how the value box shows it
/// (suffix, decimals, ...). True when the value changed.
pub fn bound(ui: &mut Ui, v: &mut f32, lo: f32, hi: f32, log: bool, def: f32, target: &Target, fmt: Fmt, extra: impl FnOnce(&mut Ui)) -> bool {
    let track = ui.add(egui::Slider::new(v, lo..=hi).logarithmic(log).clamping(egui::SliderClamping::Always).show_value(false));
    let mut changed = track.changed();
    let speed = if log { (*v).max(lo.max(1e-3)) as f64 * 0.01 } else { (hi - lo) as f64 * 0.004 };
    let field = ui.add(fmt.apply(egui::DragValue::new(v).range(lo..=hi).speed(speed)));
    changed |= field.changed();
    let mut reset = ui.interact(track.rect, track.id.with("rc_track"), Sense::click()).secondary_clicked();
    context_menu(&field, |ui| {
        if ui.button("Reset to the default").clicked() {
            reset = true;
            ui.close();
        }
        extra(ui);
        ui.separator();
        crate::mapping::bind_menu(ui, target);
    });
    if reset {
        *v = def;
        changed = true;
    }
    changed
}
