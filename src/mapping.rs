//! Input mappings: keys, MIDI controls and game controller / joystick buttons and axes bound to actions (buttons) and to settings
//! (sliders). A binding is learned by pressing the key or moving the control; the bindings are stored in `mappings.txt` in the
//! settings folder. The sources deliver events into one queue; the application takes them once per frame.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};

/// Where an input comes from.
#[derive(Clone, PartialEq, Debug)]
pub enum Src {
    Key { key: String, ctrl: bool, shift: bool, alt: bool },
    /// a MIDI note (`cc` false) or controller on a channel (1 to 16)
    Midi { cc: bool, ch: u8, num: u8 },
    /// a button or an axis of a game controller / joystick (`code` is the device's own number for it)
    Pad { dev: String, axis: bool, code: String },
}

impl Src {
    pub fn encode(&self) -> String {
        match self {
            Src::Key { key, ctrl, shift, alt } => format!("key:{}{}{}{key}", if *ctrl { "ctrl+" } else { "" }, if *shift { "shift+" } else { "" }, if *alt { "alt+" } else { "" }),
            Src::Midi { cc, ch, num } => format!("midi:{}:{ch}:{num}", if *cc { "cc" } else { "note" }),
            Src::Pad { dev, axis, code } => format!("pad:{}:{}:{}", dev.replace(':', " ").replace('|', " "), if *axis { "axis" } else { "btn" }, code.replace(':', " ").replace('|', " ")),
        }
    }

    pub fn decode(s: &str) -> Option<Src> {
        let (kind, rest) = s.split_once(':')?;
        match kind {
            "key" => {
                let (mut ctrl, mut shift, mut alt) = (false, false, false);
                let mut key = rest;
                loop {
                    if let Some(r) = key.strip_prefix("ctrl+") {
                        ctrl = true;
                        key = r;
                    } else if let Some(r) = key.strip_prefix("shift+") {
                        shift = true;
                        key = r;
                    } else if let Some(r) = key.strip_prefix("alt+") {
                        alt = true;
                        key = r;
                    } else {
                        break;
                    }
                }
                (!key.is_empty()).then(|| Src::Key { key: key.to_string(), ctrl, shift, alt })
            }
            "midi" => {
                let p: Vec<&str> = rest.split(':').collect();
                let (cc, ch, num) = (match *p.first()? { "cc" => true, "note" => false, _ => return None }, p.get(1)?.parse::<u8>().ok()?, p.get(2)?.parse::<u8>().ok()?);
                ((1..=16).contains(&ch) && num < 128).then_some(Src::Midi { cc, ch, num })
            }
            "pad" => {
                let p: Vec<&str> = rest.split(':').collect();
                if p.len() < 3 {
                    return None;
                }
                let axis = match p[p.len() - 2] {
                    "axis" => true,
                    "btn" => false,
                    _ => return None,
                };
                Some(Src::Pad { dev: p[..p.len() - 2].join(":"), axis, code: p[p.len() - 1].to_string() })
            }
            _ => None,
        }
    }

    /// Text for the list.
    pub fn label(&self) -> String {
        match self {
            Src::Key { key, ctrl, shift, alt } => format!("{}{}{}{key}", if *ctrl { "Ctrl+" } else { "" }, if *shift { "Shift+" } else { "" }, if *alt { "Alt+" } else { "" }),
            Src::Midi { cc, ch, num } => format!("MIDI {} {num} (channel {ch})", if *cc { "controller" } else { "note" }),
            Src::Pad { dev, axis, code } => format!("{dev}: {} {code}", if *axis { "axis" } else { "button" }),
        }
    }

    /// A knob, fader or stick: it delivers values, not presses.
    pub fn analog(&self) -> bool {
        matches!(self, Src::Midi { cc: true, .. } | Src::Pad { axis: true, .. })
    }
}

/// What can be bound.
pub struct ActionDef {
    pub id: String,
    pub label: String,
    pub group: &'static str,
    /// `Some((min, max, logarithmic, unit))` for a setting that follows a slider, `None` for a button action
    pub slider: Option<(f32, f32, bool, &'static str)>,
}

pub fn actions() -> Vec<ActionDef> {
    let b = |id: &str, label: &str, group: &'static str| ActionDef { id: id.into(), label: label.into(), group, slider: None };
    let s = |id: &str, label: &str, group: &'static str, min: f32, max: f32, log: bool, unit: &'static str| ActionDef { id: id.into(), label: label.into(), group, slider: Some((min, max, log, unit)) };
    let mut v = vec![
        b("freeze", "Freeze / resume the analysis", "Analysis"),
        b("reset_minmax", "Reset average / max / min", "Analysis"),
        b("png", "Save a screenshot", "Analysis"),
        b("monitor", "Monitor on / off", "Sound"),
        b("generator", "Generator on / off", "Sound"),
        s("mon_gain", "Monitor volume", "Sound", -60.0, 6.0, false, " dB"),
        s("gen_level", "Generator level", "Sound", -90.0, 0.0, false, " dB"),
        s("gen_freq", "Generator frequency", "Sound", 20.0, 20000.0, true, " Hz"),
        b("tile", "Tile the windows", "Windows"),
        b("assistant", "Open / close the assistant", "Windows"),
        b("daw_play", "Timeline: play / pause", "Timeline"),
        b("daw_record", "Timeline: record / stop", "Timeline"),
        b("daw_loop", "Timeline: loop on / off", "Timeline"),
        b("daw_marker", "Timeline: add a marker at the playhead", "Timeline"),
        b("daw_split", "Timeline: split at the playhead", "Timeline"),
        b("daw_undo", "Timeline: undo", "Timeline"),
        b("daw_redo", "Timeline: redo", "Timeline"),
        s("daw_master", "Timeline: master volume", "Timeline", -60.0, 12.0, false, " dB"),
        b("graph_probe", "Routing: listening probe on / off", "Routing"),
        b("graph_ab", "Routing: switch A / B", "Routing"),
        b("graph_undo", "Routing: undo", "Routing"),
        b("graph_redo", "Routing: redo", "Routing"),
    ];
    for k in 1..=9 {
        v.push(b(&format!("layout_{k}"), &format!("Layout {k}"), "Layouts"));
    }
    for i in 0..crate::windows::WIN_COUNT {
        let n = crate::windows::win_short(i);
        v.push(b(&format!("win:{n}"), &format!("Show / hide the {n} window"), "Windows"));
    }
    v
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub action: String,
    pub src: Src,
    pub min: f32,
    pub max: f32,
    pub invert: bool,
}

/// An input that fired an action: its id and the value (0 to 1 mapped onto the range of a slider; 1 for a press).
#[derive(Debug, PartialEq)]
pub struct Fire {
    pub action: String,
    pub value: f32,
}

static EVENTS: Mutex<VecDeque<(Src, f32)>> = Mutex::new(VecDeque::new());
static DEVICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Called by the sources (the keyboard of the window, MIDI, controllers).
pub fn push_event(src: Src, value: f32) {
    let mut q = EVENTS.lock();
    if q.len() < 512 {
        q.push_back((src, value));
    }
}

/// Names of the MIDI inputs and controllers that are connected (for the editor).
pub fn devices() -> Vec<String> {
    DEVICES.lock().clone()
}

fn set_devices(kind: &str, names: Vec<String>) {
    let mut d = DEVICES.lock();
    d.retain(|n| !n.starts_with(kind));
    d.extend(names.into_iter().map(|n| format!("{kind} {n}")));
}

#[derive(Default)]
pub struct Mapper {
    pub bindings: Vec<Binding>,
    /// the action that waits for the next input
    pub learning: Option<String>,
    /// the last input, for the editor
    pub last: String,
    held: HashMap<usize, bool>,
    pub open: bool,
    pub dirty: bool,
}

fn file() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("VISUSCOPE_MAPPINGS_FILE") {
        return p.into();
    }
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("visuscope").join("mappings.txt")
}

impl Mapper {
    pub fn encode(&self) -> String {
        let mut s = String::from("VSMAP 1\n");
        for b in &self.bindings {
            s += &format!("bind|{}|{}|{}|{}|{}\n", b.action, b.src.encode(), b.min, b.max, b.invert as u8);
        }
        s
    }

    pub fn decode(text: &str) -> Mapper {
        let mut m = Mapper::default();
        let mut lines = text.lines();
        if lines.next().map(|l| l.trim()) != Some("VSMAP 1") {
            return m;
        }
        for l in lines {
            let f: Vec<&str> = l.split('|').collect();
            if f.len() < 6 || f[0] != "bind" || f[1].is_empty() {
                continue;
            }
            let (Some(src), Ok(min), Ok(max)) = (Src::decode(f[2]), f[3].parse::<f32>(), f[4].parse::<f32>()) else { continue };
            if !min.is_finite() || !max.is_finite() || m.bindings.len() >= 500 {
                continue;
            }
            m.bindings.retain(|b| b.action != f[1]);
            m.bindings.push(Binding { action: f[1].to_string(), src, min, max, invert: f[5] == "1" });
        }
        m
    }

    pub fn load() -> Mapper {
        let mut m = std::fs::read_to_string(file()).map(|t| Mapper::decode(&t)).unwrap_or_default();
        // test hook: VISUSCOPE_MAPPINGS=1 opens the editor at the start (screenshots)
        m.open = std::env::var_os("VISUSCOPE_MAPPINGS").is_some();
        m
    }

    pub fn save(&mut self) {
        self.dirty = false;
        if let Some(d) = file().parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(file(), self.encode());
    }

    pub fn binding(&self, action: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.action == action)
    }

    pub fn clear(&mut self, action: &str) {
        self.bindings.retain(|b| b.action != action);
        self.dirty = true;
    }

    /// Bind `src` to `action` (an input drives one action, an action has one input).
    pub fn assign(&mut self, action: &str, src: Src, range: (f32, f32)) {
        self.bindings.retain(|b| b.action != action && b.src != src);
        self.bindings.push(Binding { action: action.to_string(), src, min: range.0, max: range.1, invert: false });
        self.dirty = true;
    }

    /// Take the queued inputs: they teach the action that waits, or fire the actions they are bound to.
    pub fn poll(&mut self, defs: &[ActionDef]) -> Vec<Fire> {
        let events: Vec<(Src, f32)> = EVENTS.lock().drain(..).collect();
        self.process(events, defs)
    }

    pub fn process(&mut self, events: Vec<(Src, f32)>, defs: &[ActionDef]) -> Vec<Fire> {
        let mut out = Vec::new();
        for (src, v) in events {
            self.last = format!("{}  {:.2}", src.label(), v);
            if let Some(action) = self.learning.clone() {
                let slider = defs.iter().find(|d| d.id == action).and_then(|d| d.slider);
                // a slider listens for knobs and faders, a button for keys, notes and buttons (or a stick pushed all the way)
                let ok = match slider {
                    Some(_) => src.analog(),
                    None => !src.analog() && v >= 0.5 || src.analog() && v >= 0.9,
                };
                if ok {
                    let range = slider.map_or((0.0, 1.0), |s| (s.0, s.1));
                    self.assign(&action, src, range);
                    self.learning = None;
                }
                continue;
            }
            for (i, b) in self.bindings.iter().enumerate() {
                if b.src != src {
                    continue;
                }
                let slider = defs.iter().find(|d| d.id == b.action).and_then(|d| d.slider);
                match slider {
                    Some((_, _, log, _)) => {
                        let t = if b.invert { 1.0 - v.clamp(0.0, 1.0) } else { v.clamp(0.0, 1.0) };
                        let x = if log && b.min > 0.0 && b.max > b.min { b.min * (b.max / b.min).powf(t) } else { b.min + (b.max - b.min) * t };
                        out.push(Fire { action: b.action.clone(), value: x });
                    }
                    None => {
                        // a press: the moment the value passes one half (releases and the way back do nothing)
                        let down = v >= 0.5;
                        let was = self.held.insert(i, down).unwrap_or(false);
                        if down && !was {
                            out.push(Fire { action: b.action.clone(), value: 1.0 });
                        }
                    }
                }
            }
        }
        out
    }
}

/// The meaning of a MIDI message: a note on / off or a controller, with a value from 0 to 1.
pub fn parse_midi(status: u8, d1: u8, d2: u8) -> Option<(Src, f32)> {
    let ch = (status & 0x0F) + 1;
    match status & 0xF0 {
        0x90 => Some((Src::Midi { cc: false, ch, num: d1 & 0x7F }, if d2 > 0 { 1.0 } else { 0.0 })),
        0x80 => Some((Src::Midi { cc: false, ch, num: d1 & 0x7F }, 0.0)),
        0xB0 => Some((Src::Midi { cc: true, ch, num: d1 & 0x7F }, (d2 & 0x7F) as f32 / 127.0)),
        _ => None,
    }
}

// ---------------------------------------------------------------------- MIDI (Windows multimedia API)

#[cfg(windows)]
mod midi {
    use super::*;
    use windows_sys::Win32::Media::Audio::{midiInClose, midiInGetDevCapsW, midiInGetNumDevs, midiInOpen, midiInStart, HMIDIIN, MIDIINCAPSW};

    const CALLBACK_FUNCTION: u32 = 0x0003_0000;
    const MIM_DATA: u32 = 0x3C3;

    unsafe extern "system" fn callback(_h: HMIDIIN, msg: u32, _inst: usize, p1: usize, _p2: usize) {
        if msg == MIM_DATA {
            let (status, d1, d2) = ((p1 & 0xFF) as u8, ((p1 >> 8) & 0xFF) as u8, ((p1 >> 16) & 0xFF) as u8);
            if let Some((src, v)) = parse_midi(status, d1, d2) {
                push_event(src, v);
            }
        }
    }

    /// Open every MIDI input that is not open yet. Returns the names of the open ones.
    pub fn scan(open: &mut Vec<(u32, isize)>) -> Vec<String> {
        // SAFETY: plain calls of the multimedia API with valid pointers; the callback only reads its arguments
        unsafe {
            let n = midiInGetNumDevs();
            let mut names = Vec::new();
            for id in 0..n {
                let mut caps: MIDIINCAPSW = std::mem::zeroed();
                if midiInGetDevCapsW(id as usize, &mut caps, std::mem::size_of::<MIDIINCAPSW>() as u32) != 0 {
                    continue;
                }
                // (a copy: the structure is packed)
                let pname = caps.szPname;
                let len = pname.iter().position(|c| *c == 0).unwrap_or(pname.len());
                let name = String::from_utf16_lossy(&pname[..len]);
                if !open.iter().any(|o| o.0 == id) {
                    let mut h: HMIDIIN = std::ptr::null_mut();
                    if midiInOpen(&mut h, id, callback as usize, 0, CALLBACK_FUNCTION) == 0 {
                        midiInStart(h);
                        open.push((id, h as isize));
                    } else {
                        continue;
                    }
                }
                names.push(name);
            }
            // devices that went away
            open.retain(|o| {
                if o.0 < n {
                    true
                } else {
                    midiInClose(o.1 as HMIDIIN);
                    false
                }
            });
            names
        }
    }
}

// ---------------------------------------------------------------------- game controllers and joysticks

fn pad_thread() {
    let Ok(mut g) = gilrs::Gilrs::new() else {
        crate::log::log("input mappings: no controller support on this system");
        return;
    };
    let mut last_list = std::time::Instant::now() - std::time::Duration::from_secs(10);
    loop {
        while let Some(gilrs::Event { id, event, .. }) = g.next_event() {
            let name = || g.gamepad(id).name().to_string();
            match event {
                gilrs::EventType::ButtonPressed(_, code) => push_event(Src::Pad { dev: name(), axis: false, code: code.to_string() }, 1.0),
                gilrs::EventType::ButtonReleased(_, code) => push_event(Src::Pad { dev: name(), axis: false, code: code.to_string() }, 0.0),
                // sticks and sliders go from -1 to 1: 0 to 1 for the mapping
                gilrs::EventType::AxisChanged(_, v, code) => push_event(Src::Pad { dev: name(), axis: true, code: code.to_string() }, ((v + 1.0) / 2.0).clamp(0.0, 1.0)),
                _ => {}
            }
        }
        if last_list.elapsed() > std::time::Duration::from_secs(2) {
            last_list = std::time::Instant::now();
            set_devices("Controller", g.gamepads().map(|(_, p)| p.name().to_string()).collect());
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

/// Start the sources that run on their own threads (MIDI, controllers). Test runs stay quiet.
pub fn start_sources() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if crate::prof::test_mode() || ONCE.is_completed() {
        return;
    }
    ONCE.call_once(|| {});
    std::thread::Builder::new().name("input-pads".into()).spawn(pad_thread).ok();
    #[cfg(windows)]
    std::thread::Builder::new()
        .name("input-midi".into())
        .spawn(|| {
            let mut open: Vec<(u32, isize)> = Vec::new();
            loop {
                let names = midi::scan(&mut open);
                set_devices("MIDI", names);
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<ActionDef> {
        actions()
    }

    #[test]
    fn sources_survive_the_text_format() {
        for s in [
            Src::Key { key: "F5".into(), ctrl: true, shift: false, alt: true },
            Src::Key { key: "Space".into(), ctrl: false, shift: false, alt: false },
            Src::Midi { cc: true, ch: 3, num: 74 },
            Src::Midi { cc: false, ch: 16, num: 0 },
            Src::Pad { dev: "Xbox: Controller | 1".into(), axis: true, code: "Code(2)".into() },
        ] {
            let back = Src::decode(&s.encode()).unwrap();
            // names with the characters of the format in them are cleaned, nothing else changes
            if let (Src::Pad { dev: a, .. }, Src::Pad { dev: b, .. }) = (&s, &back) {
                assert_eq!(a.replace(':', " ").replace('|', " "), *b);
            } else {
                assert_eq!(back, s);
            }
        }
        assert!(Src::decode("midi:cc:0:5").is_none() && Src::decode("midi:cc:3:200").is_none() && Src::decode("nothing").is_none() && Src::decode("key:").is_none() && Src::decode("pad:x").is_none());
    }

    #[test]
    fn midi_messages_are_understood() {
        assert_eq!(parse_midi(0x90, 60, 100), Some((Src::Midi { cc: false, ch: 1, num: 60 }, 1.0)));
        assert_eq!(parse_midi(0x90, 60, 0), Some((Src::Midi { cc: false, ch: 1, num: 60 }, 0.0)), "note on with velocity 0 is a release");
        assert_eq!(parse_midi(0x83, 61, 64), Some((Src::Midi { cc: false, ch: 4, num: 61 }, 0.0)));
        let (s, v) = parse_midi(0xB2, 7, 127).unwrap();
        assert_eq!((s, v), (Src::Midi { cc: true, ch: 3, num: 7 }, 1.0));
        assert!(parse_midi(0xF8, 0, 0).is_none() && parse_midi(0xE0, 1, 2).is_none());
    }

    #[test]
    fn learning_assigns_the_right_kind_of_input_and_replaces_conflicts() {
        let d = defs();
        let mut m = Mapper::default();
        // a button action ignores a knob that is not turned all the way, takes a key press
        m.learning = Some("freeze".into());
        let knob = Src::Midi { cc: true, ch: 1, num: 7 };
        assert!(m.process(vec![(knob.clone(), 0.3)], &d).is_empty());
        assert!(m.learning.is_some());
        let key = Src::Key { key: "F".into(), ctrl: false, shift: false, alt: false };
        m.process(vec![(key.clone(), 1.0)], &d);
        assert!(m.learning.is_none());
        assert_eq!(m.binding("freeze").unwrap().src, key);
        // a slider only takes a knob, not a key
        m.learning = Some("mon_gain".into());
        m.process(vec![(key.clone(), 1.0)], &d);
        assert!(m.learning.is_some());
        m.process(vec![(knob.clone(), 0.5)], &d);
        let b = m.binding("mon_gain").unwrap();
        assert_eq!((b.src.clone(), b.min, b.max), (knob.clone(), -60.0, 6.0));
        // the same input bound to another action takes it over
        m.learning = Some("tile".into());
        m.process(vec![(key.clone(), 1.0)], &d);
        assert!(m.binding("freeze").is_none() && m.binding("tile").is_some());
        assert_eq!(m.bindings.len(), 2);
    }

    #[test]
    fn buttons_fire_once_per_press_and_sliders_follow_their_range() {
        let d = defs();
        let mut m = Mapper::default();
        let key = Src::Key { key: "P".into(), ctrl: false, shift: false, alt: false };
        let pad = Src::Pad { dev: "pad".into(), axis: true, code: "a".into() };
        let knob = Src::Midi { cc: true, ch: 1, num: 1 };
        let freq = Src::Midi { cc: true, ch: 1, num: 2 };
        m.assign("daw_play", key.clone(), (0.0, 1.0));
        m.assign("graph_probe", pad.clone(), (0.0, 1.0));
        m.assign("gen_level", knob.clone(), (-90.0, 0.0));
        m.assign("gen_freq", freq.clone(), (20.0, 20000.0));
        // a press fires, the release and a second release do not, the next press fires again
        let f = m.process(vec![(key.clone(), 1.0), (key.clone(), 0.0), (key.clone(), 0.0), (key.clone(), 1.0)], &d);
        assert_eq!(f, vec![Fire { action: "daw_play".into(), value: 1.0 }, Fire { action: "daw_play".into(), value: 1.0 }]);
        // an axis pushed past the middle is a press once, not on every movement
        let f = m.process(vec![(pad.clone(), 0.2), (pad.clone(), 0.7), (pad.clone(), 0.9), (pad.clone(), 0.1), (pad.clone(), 0.8)], &d);
        assert_eq!(f.len(), 2);
        // sliders: the end points and the middle, linear and logarithmic
        let f = m.process(vec![(knob.clone(), 0.0), (knob.clone(), 1.0), (knob.clone(), 0.5), (freq.clone(), 0.5)], &d);
        assert_eq!(f[0].value, -90.0);
        assert_eq!(f[1].value, 0.0);
        assert!((f[2].value + 45.0).abs() < 1e-3);
        assert!((f[3].value - 632.4555).abs() < 0.5, "geometric middle of 20 and 20000 Hz: {}", f[3].value);
        // inverted
        m.bindings.iter_mut().find(|b| b.action == "gen_level").unwrap().invert = true;
        assert_eq!(m.process(vec![(knob, 1.0)], &d)[0].value, -90.0);
        // an input nobody uses does nothing
        assert!(m.process(vec![(Src::Midi { cc: false, ch: 9, num: 9 }, 1.0)], &d).is_empty());
    }

    #[test]
    fn the_file_round_trips_and_damaged_lines_are_skipped() {
        let mut m = Mapper::default();
        m.assign("freeze", Src::Key { key: "F".into(), ctrl: true, shift: true, alt: false }, (0.0, 1.0));
        m.assign("mon_gain", Src::Midi { cc: true, ch: 1, num: 7 }, (-40.0, 0.0));
        m.bindings[1].invert = true;
        let back = Mapper::decode(&m.encode());
        assert_eq!(back.bindings, m.bindings);
        let damaged = "VSMAP 1\nbind|a|key:X|0|1|0\nbind||key:Y|0|1|0\nbind|b|midi:cc:99:1|0|1|0\nbind|c|key:Z|nan|1|0\nbind|d|key:W|x|1|0\nnonsense\nbind|e|key:V|0|1\n";
        let d = Mapper::decode(damaged);
        assert_eq!(d.bindings.iter().map(|b| b.action.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert!(Mapper::decode("garbage").bindings.is_empty());
        // the list of actions has no id twice
        let a = actions();
        let mut ids: Vec<&str> = a.iter().map(|d| d.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), a.len());
    }
}
