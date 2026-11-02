//! The routing graph as plain data: nodes with parameters, stereo connections, editing rules and the
//! text format it is stored in. No audio and no UI in here.

use std::collections::HashSet;

pub type NodeId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Input,
    Generator,
    TimelineOut,
    Gain,
    Eq,
    Filter,
    Comp,
    Limiter,
    Gate,
    Delay,
    Distortion,
    Fuzz,
    Crusher,
    Jitter,
    Tremolo,
    Stereo,
    Utility,
    Phase,
    Convolver,
    NoiseRed,
    Mixer,
    /// stereo in, two outputs: the left channel and the right channel, each on both sides
    Split,
    /// two inputs (left, right) made into one stereo signal
    Merge,
    Analyzer,
    Output,
    TimelineRec,
}

/// One adjustable value of a node. `choices` turns it into a drop-down (the value is the index).
pub struct ParamDef {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub def: f32,
    pub unit: &'static str,
    pub log: bool,
    pub choices: &'static [&'static str],
}

const fn s(name: &'static str, min: f32, max: f32, def: f32, unit: &'static str) -> ParamDef {
    ParamDef { name, min, max, def, unit, log: false, choices: &[] }
}
const fn l(name: &'static str, min: f32, max: f32, def: f32, unit: &'static str) -> ParamDef {
    ParamDef { name, min, max, def, unit, log: true, choices: &[] }
}
const fn c(name: &'static str, def: usize, choices: &'static [&'static str]) -> ParamDef {
    ParamDef { name, min: 0.0, max: (choices.len() - 1) as f32, def: def as f32, unit: "", log: false, choices }
}

pub const EQ_TYPES: &[&str] = &["Bell", "Low shelf", "High shelf", "High-pass", "Low-pass", "Notch", "Off"];
pub const EQ_BANDS: usize = 4;
pub const MIX_MAX: usize = 8;

const P_NONE: &[ParamDef] = &[];
const P_INPUT: &[ParamDef] = &[s("Left ch", 1.0, 32.0, 1.0, ""), s("Right ch", 1.0, 32.0, 2.0, ""), s("Trim", -40.0, 40.0, 0.0, " dB"), s("Delay", 0.0, 500.0, 0.0, " ms")];
const P_GEN: &[ParamDef] = &[s("Generator", 1.0, 8.0, 1.0, "")];
const P_GAIN: &[ParamDef] = &[s("Gain", -60.0, 24.0, 0.0, " dB"), c("Phase", 0, &["normal", "inverted"])];
const P_EQ: &[ParamDef] = &[
    c("1 type", 1, EQ_TYPES), l("1 freq", 20.0, 20000.0, 100.0, " Hz"), s("1 gain", -60.0, 24.0, 0.0, " dB"), l("1 Q", 0.1, 10.0, 0.7, ""),
    c("2 type", 0, EQ_TYPES), l("2 freq", 20.0, 20000.0, 500.0, " Hz"), s("2 gain", -60.0, 24.0, 0.0, " dB"), l("2 Q", 0.1, 10.0, 1.0, ""),
    c("3 type", 0, EQ_TYPES), l("3 freq", 20.0, 20000.0, 2500.0, " Hz"), s("3 gain", -60.0, 24.0, 0.0, " dB"), l("3 Q", 0.1, 10.0, 1.0, ""),
    c("4 type", 2, EQ_TYPES), l("4 freq", 20.0, 20000.0, 8000.0, " Hz"), s("4 gain", -60.0, 24.0, 0.0, " dB"), l("4 Q", 0.1, 10.0, 0.7, ""),
];
const P_FILTER: &[ParamDef] = &[c("Type", 0, &["High-pass", "Low-pass"]), l("Freq", 20.0, 20000.0, 80.0, " Hz"), c("Slope", 1, &["12 dB/oct", "24 dB/oct", "48 dB/oct"])];
const P_COMP: &[ParamDef] = &[
    s("Threshold", -60.0, 0.0, -18.0, " dB"),
    s("Ratio", 1.0, 20.0, 4.0, ":1"),
    l("Attack", 0.1, 200.0, 10.0, " ms"),
    l("Release", 5.0, 2000.0, 120.0, " ms"),
    s("Knee", 0.0, 24.0, 6.0, " dB"),
    s("Makeup", 0.0, 24.0, 0.0, " dB"),
];
const P_LIMIT: &[ParamDef] = &[s("Ceiling", -24.0, 0.0, -1.0, " dB"), l("Release", 1.0, 500.0, 60.0, " ms")];
const P_GATE: &[ParamDef] = &[s("Threshold", -90.0, 0.0, -50.0, " dB"), l("Attack", 0.1, 50.0, 2.0, " ms"), l("Release", 5.0, 1000.0, 120.0, " ms"), s("Range", -90.0, 0.0, -80.0, " dB")];
const P_DELAY: &[ParamDef] = &[s("Time", 0.0, 2000.0, 250.0, " ms"), s("Feedback", 0.0, 95.0, 30.0, " %"), s("Mix", 0.0, 100.0, 50.0, " %")];
const P_DIST: &[ParamDef] = &[
    s("Drive", 0.0, 48.0, 12.0, " dB"),
    c("Type", 0, DIST_TYPES),
    l("Tone", 500.0, 20000.0, 12000.0, " Hz"),
    s("Mix", 0.0, 100.0, 100.0, " %"),
    s("Output", -36.0, 12.0, -6.0, " dB"),
    s("Even", 0.0, 100.0, 40.0, " %"),
    s("Odd", 0.0, 100.0, 0.0, " %"),
    s("Richness", 0.0, 100.0, 50.0, " %"),
];
/// The last type is a series of harmonics instead of a clipper.
pub const DIST_TYPES: &[&str] = &["Soft (tube-like)", "Hard clip", "Asymmetric (even)", "Fold-back", "Harmonic series (even / odd)"];
pub const DIST_HARMONIC: usize = 4;
const P_PHASE: &[ParamDef] = &[
    c("Polarity", 0, &["normal", "inverted"]),
    c("Shift", 0, &["off", "all frequencies", "all-pass at frequency"]),
    s("Angle", 0.0, 360.0, 90.0, " deg"),
    l("Freq", 20.0, 20000.0, 1000.0, " Hz"),
    l("Q", 0.1, 10.0, 0.7, ""),
];
const P_CONV: &[ParamDef] = &[s("Mix", 0.0, 100.0, 30.0, " %"), s("Level", -24.0, 24.0, 0.0, " dB")];
const P_NR: &[ParamDef] = &[s("Reduce", 0.0, 40.0, 12.0, " dB"), s("Strength", 0.0, 100.0, 50.0, " %"), c("Noise", 0, &["follow", "learn", "frozen"])];
const P_FUZZ: &[ParamDef] = &[s("Fuzz", 0.0, 100.0, 70.0, " %"), s("Gate", 0.0, 100.0, 10.0, " %"), l("Tone", 300.0, 20000.0, 5000.0, " Hz"), s("Output", -36.0, 12.0, -10.0, " dB")];
const P_CRUSH: &[ParamDef] = &[s("Bits", 1.0, 24.0, 8.0, ""), s("Hold", 1.0, 64.0, 1.0, ""), s("Mix", 0.0, 100.0, 100.0, " %")];
const P_JITTER: &[ParamDef] = &[s("Depth", 0.0, 2000.0, 100.0, " us"), l("Rate", 0.1, 5000.0, 8.0, " Hz"), c("Shape", 0, &["Random", "Sine"]), s("Mix", 0.0, 100.0, 100.0, " %")];
const P_TREM: &[ParamDef] = &[l("Rate", 0.1, 30.0, 5.0, " Hz"), s("Depth", 0.0, 100.0, 50.0, " %"), c("Shape", 0, &["Sine", "Triangle", "Square"])];
const P_STEREO: &[ParamDef] = &[s("Width", 0.0, 200.0, 100.0, " %"), s("Balance", -100.0, 100.0, 0.0, " %")];
const P_UTIL: &[ParamDef] = &[c("Mode", 0, &["Stereo (through)", "Left to both", "Right to both", "Mono sum", "Swap L/R", "Invert left", "Invert right", "Invert both"])];
const P_MIXER: &[ParamDef] = &[
    s("Inputs", 1.0, MIX_MAX as f32, 4.0, ""),
    s("Master", -60.0, 12.0, 0.0, " dB"),
    s("In 1", -60.0, 12.0, 0.0, " dB"), s("In 2", -60.0, 12.0, 0.0, " dB"), s("In 3", -60.0, 12.0, 0.0, " dB"), s("In 4", -60.0, 12.0, 0.0, " dB"),
    s("In 5", -60.0, 12.0, 0.0, " dB"), s("In 6", -60.0, 12.0, 0.0, " dB"), s("In 7", -60.0, 12.0, 0.0, " dB"), s("In 8", -60.0, 12.0, 0.0, " dB"),
    s("Mute 1", 0.0, 1.0, 0.0, ""), s("Mute 2", 0.0, 1.0, 0.0, ""), s("Mute 3", 0.0, 1.0, 0.0, ""), s("Mute 4", 0.0, 1.0, 0.0, ""),
    s("Mute 5", 0.0, 1.0, 0.0, ""), s("Mute 6", 0.0, 1.0, 0.0, ""), s("Mute 7", 0.0, 1.0, 0.0, ""), s("Mute 8", 0.0, 1.0, 0.0, ""),
];
const P_OUTPUT: &[ParamDef] = &[s("Gain", -60.0, 12.0, 0.0, " dB"), c("Mute", 0, &["on", "muted"]), c("Input", 0, &["Stereo pair", "Separate L / R"])];

pub struct KindInfo {
    pub name: &'static str,
    pub group: &'static str,
    pub keywords: &'static str,
    /// stereo input ports (a mixer has `inputs()` instead)
    pub inputs: usize,
    pub outputs: usize,
    pub params: &'static [ParamDef],
    /// header colour
    pub color: (u8, u8, u8),
}

const SRC: (u8, u8, u8) = (70, 140, 100);
const FX: (u8, u8, u8) = (90, 110, 170);
const SNK: (u8, u8, u8) = (170, 100, 80);

impl Kind {
    pub const ALL: [Kind; 26] = [
        Kind::Input,
        Kind::Generator,
        Kind::TimelineOut,
        Kind::Gain,
        Kind::Eq,
        Kind::Filter,
        Kind::Comp,
        Kind::Limiter,
        Kind::Gate,
        Kind::Delay,
        Kind::Distortion,
        Kind::Fuzz,
        Kind::Crusher,
        Kind::Jitter,
        Kind::Tremolo,
        Kind::Stereo,
        Kind::Utility,
        Kind::Phase,
        Kind::Convolver,
        Kind::NoiseRed,
        Kind::Mixer,
        Kind::Split,
        Kind::Merge,
        Kind::Analyzer,
        Kind::Output,
        Kind::TimelineRec,
    ];

    pub fn info(self) -> KindInfo {
        let k = |name, group, keywords, inputs, outputs, params, color| KindInfo { name, group, keywords, inputs, outputs, params, color };
        match self {
            Kind::Input => k("Input device", "Source", "microphone line interface loopback capture adc", 0, 1, P_INPUT, SRC),
            Kind::Generator => k("Generator", "Source", "tone sine square noise sweep signal", 0, 1, P_GEN, SRC),
            Kind::TimelineOut => k("Timeline out", "Source", "daw track master playback", 0, 1, P_NONE, SRC),
            Kind::Gain => k("Gain", "Effect", "volume level trim phase invert", 1, 1, P_GAIN, FX),
            Kind::Eq => k("Equalizer", "Effect", "eq parametric bell shelf tone", 1, 1, P_EQ, FX),
            Kind::Filter => k("Filter", "Effect", "highpass lowpass hp lp cut butterworth", 1, 1, P_FILTER, FX),
            Kind::Comp => k("Compressor", "Effect", "dynamics compress threshold ratio", 1, 1, P_COMP, FX),
            Kind::Limiter => k("Limiter", "Effect", "dynamics ceiling brickwall peak", 1, 1, P_LIMIT, FX),
            Kind::Gate => k("Noise gate", "Effect", "dynamics expander silence", 1, 1, P_GATE, FX),
            Kind::Delay => k("Delay", "Effect", "echo time feedback", 1, 1, P_DELAY, FX),
            Kind::Distortion => k("Distortion / harmonics", "Effect", "overdrive distort clip saturate drive tube fold harmonics even odd exciter second third overtones", 1, 1, P_DIST, FX),
            Kind::Fuzz => k("Fuzz", "Effect", "fuzz box guitar clip sputter", 1, 1, P_FUZZ, FX),
            Kind::Crusher => k("Bit crusher", "Effect", "bitcrusher bit depth sample rate reduce lofi quantise", 1, 1, P_CRUSH, FX),
            Kind::Jitter => k("Jitter / wow & flutter", "Effect", "jitter clock timing wow flutter vibrato modulation", 1, 1, P_JITTER, FX),
            Kind::Tremolo => k("Tremolo", "Effect", "tremolo amplitude modulation lfo", 1, 1, P_TREM, FX),
            Kind::Stereo => k("Stereo width", "Effect", "width balance pan mid side", 1, 1, P_STEREO, FX),
            Kind::Utility => k("Channel utility", "Effect", "mono swap invert left right channel phase", 1, 1, P_UTIL, FX),
            Kind::Phase => k("Phase shift / invert", "Effect", "phase invert polarity shift allpass hilbert rotate null", 1, 1, P_PHASE, FX),
            Kind::Convolver => k("Convolution (IR / reverb)", "Effect", "convolution impulse response ir reverb cabinet room load wav", 1, 1, P_CONV, FX),
            Kind::NoiseRed => k("Noise reduction", "Effect", "denoise noise reduction hiss hum spectral subtraction gate profile", 1, 1, P_NR, FX),
            Kind::Mixer => k("Mixer", "Routing", "mix sum combine merge bus", MIX_MAX, 1, P_MIXER, (150, 120, 70)),
            Kind::Split => k("Split left / right", "Routing", "split channels left right separate mono dual", 1, 2, P_NONE, (150, 120, 70)),
            Kind::Merge => k("Merge left / right", "Routing", "merge combine channels left right stereo join", 2, 1, P_NONE, (150, 120, 70)),
            Kind::Analyzer => k("Analyzer", "Sink", "spectrum scope analysis meter display", 1, 0, P_NONE, SNK),
            Kind::Output => k("Output device", "Sink", "speakers headphones playback monitor dac", 1, 0, P_OUTPUT, SNK),
            Kind::TimelineRec => k("Timeline record", "Sink", "daw track record arm", 1, 0, P_NONE, SNK),
        }
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    fn code(self) -> &'static str {
        match self {
            Kind::Input => "in",
            Kind::Generator => "gen",
            Kind::TimelineOut => "tlo",
            Kind::Gain => "gain",
            Kind::Eq => "eq",
            Kind::Filter => "filt",
            Kind::Comp => "comp",
            Kind::Limiter => "lim",
            Kind::Gate => "gate",
            Kind::Delay => "dly",
            Kind::Distortion => "dist",
            Kind::Fuzz => "fuzz",
            Kind::Crusher => "crush",
            Kind::Jitter => "jit",
            Kind::Tremolo => "trem",
            Kind::Stereo => "wid",
            Kind::Utility => "util",
            Kind::Phase => "phase",
            Kind::Convolver => "conv",
            Kind::NoiseRed => "nr",
            Kind::Mixer => "mix",
            Kind::Split => "split",
            Kind::Merge => "merge",
            Kind::Analyzer => "ana",
            Kind::Output => "out",
            Kind::TimelineRec => "tlr",
        }
    }

    fn from_code(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.code() == s)
    }

    pub fn is_source(self) -> bool {
        self.info().inputs == 0
    }

    pub fn is_sink(self) -> bool {
        self.info().outputs == 0
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: NodeId,
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub p: Vec<f32>,
    /// device id (input / output) or timeline track id
    pub s: String,
    pub bypass: bool,
}

impl Node {
    pub fn new(id: NodeId, kind: Kind, x: f32, y: f32) -> Self {
        Self { id, kind, x, y, p: kind.info().params.iter().map(|d| d.def).collect(), s: String::new(), bypass: false }
    }

    /// False for a parameter that means nothing in the node's current mode (the editor leaves its row out).
    pub fn shows(&self, i: usize) -> bool {
        match self.kind {
            Kind::Distortion => {
                let harmonic = self.param(1).round() as usize == DIST_HARMONIC;
                match i {
                    0 | 2 => !harmonic,
                    5..=7 => harmonic,
                    _ => true,
                }
            }
            Kind::Phase => {
                let mode = self.param(1).round() as usize;
                match i {
                    2 => mode == 1,
                    3 | 4 => mode == 2,
                    _ => true,
                }
            }
            _ => true,
        }
    }

    /// Number of input ports (a mixer has as many as its "Inputs" parameter says).
    pub fn inputs(&self) -> usize {
        match self.kind {
            Kind::Mixer => (self.p.first().copied().unwrap_or(4.0).round() as usize).clamp(1, MIX_MAX),
            // an output can take a stereo pair, or the two channels one by one (each input made mono, like a merge node)
            Kind::Output if self.param(2).round() as usize == 1 => 2,
            k => k.info().inputs,
        }
    }

    pub fn outputs(&self) -> usize {
        self.kind.info().outputs
    }

    pub fn param(&self, i: usize) -> f32 {
        let defs = self.kind.info().params;
        let d = if self.kind == Kind::Eq { defs.get(i % 16) } else { defs.get(i) };
        match (self.p.get(i).copied(), d) {
            (Some(v), Some(d)) if v.is_finite() => v.clamp(d.min, d.max),
            (_, Some(d)) => d.def,
            (Some(v), None) if v.is_finite() => v,
            _ => 0.0,
        }
    }

    /// The parameters limited to the range of their definitions (a damaged file or a typo cannot make a filter blow up);
    /// the extra bands of an equaliser follow the first four.
    pub fn clamped(&self) -> Vec<f32> {
        let defs = self.kind.info().params;
        self.p
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let d = if self.kind == Kind::Eq { defs.get(i % 16) } else { defs.get(i) };
                match d {
                    Some(d) if v.is_finite() => v.clamp(d.min, d.max),
                    Some(d) => d.def,
                    None => 0.0,
                }
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    pub from: NodeId,
    /// output of `from` (only a split node has a second one)
    pub from_port: u8,
    pub to: NodeId,
    /// input port of `to`
    pub port: u8,
}

#[derive(Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    next: NodeId,
}

impl Graph {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Add a node; there is only one analyzer, so a second request returns the existing one.
    pub fn add(&mut self, kind: Kind, x: f32, y: f32) -> NodeId {
        if kind == Kind::Analyzer {
            if let Some(n) = self.nodes.iter().find(|n| n.kind == Kind::Analyzer) {
                return n.id;
            }
        }
        self.next += 1;
        let id = self.next;
        self.nodes.push(Node::new(id, kind, x, y));
        id
    }

    pub fn remove(&mut self, id: NodeId) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    /// Remove a node; an effect with one input and one output is cut out of its line: whatever fed it is connected straight to
    /// whatever it fed (where that is possible; the other kinds of node just leave their connections).
    pub fn remove_bridged(&mut self, id: NodeId) {
        let one_in_one_out = self.node(id).is_some_and(|n| n.kind.info().inputs == 1 && n.kind.info().outputs == 1);
        let incoming = if one_in_one_out { self.edge_into(id, 0).copied() } else { None };
        let outgoing: Vec<Edge> = self.edges.iter().filter(|e| e.from == id).copied().collect();
        self.remove(id);
        if let Some(i) = incoming {
            for o in outgoing {
                let _ = self.connect_from(i.from, i.from_port, o.to, o.port);
            }
        }
    }

    /// Put a split node into the line that feeds `port` of `to`: the left channel goes on to `to`, the right output is free
    /// to be connected anywhere. Returns the new node.
    pub fn split_wire(&mut self, to: NodeId, port: u8, x: f32, y: f32) -> Option<NodeId> {
        let e = *self.edge_into(to, port)?;
        let s = self.add(Kind::Split, x, y);
        if self.connect_from(e.from, e.from_port, s, 0).is_err() || self.connect_from(s, 0, to, port).is_err() {
            self.remove(s);
            // the old line is still there if the new one could not be made
            let _ = self.connect_from(e.from, e.from_port, to, port);
            return None;
        }
        Some(s)
    }

    /// A split node on an output, next to the lines that already leave it (they stay).
    pub fn split_output(&mut self, from: NodeId, from_port: u8, x: f32, y: f32) -> Option<NodeId> {
        let s = self.add(Kind::Split, x, y);
        if self.connect_from(from, from_port, s, 0).is_err() {
            self.remove(s);
            return None;
        }
        Some(s)
    }

    /// Edge feeding `port` of `to`.
    pub fn edge_into(&self, to: NodeId, port: u8) -> Option<&Edge> {
        self.edges.iter().find(|e| e.to == to && e.port == port)
    }

    pub fn disconnect(&mut self, to: NodeId, port: u8) {
        self.edges.retain(|e| !(e.to == to && e.port == port));
    }

    /// True when `b` can be reached from `a` by following connections.
    pub fn reaches(&self, a: NodeId, b: NodeId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![a];
        while let Some(n) = stack.pop() {
            if n == b {
                return true;
            }
            if seen.insert(n) {
                stack.extend(self.edges.iter().filter(|e| e.from == n).map(|e| e.to));
            }
        }
        false
    }

    /// Connect the (first) output of `from` to an input port of `to`. An existing connection on that port is replaced.
    pub fn connect(&mut self, from: NodeId, to: NodeId, port: u8) -> Result<(), &'static str> {
        self.connect_from(from, 0, to, port)
    }

    /// Same, from a chosen output of `from`.
    pub fn connect_from(&mut self, from: NodeId, from_port: u8, to: NodeId, port: u8) -> Result<(), &'static str> {
        let (Some(f), Some(t)) = (self.node(from), self.node(to)) else { return Err("unknown node") };
        if f.outputs() == 0 {
            return Err("this node has no output");
        }
        if from_port as usize >= f.outputs() {
            return Err("this node has no such output");
        }
        if port as usize >= t.inputs() {
            return Err("this node has no such input");
        }
        if from == to || self.reaches(to, from) {
            return Err("that would make a loop");
        }
        self.disconnect(to, port);
        self.edges.push(Edge { from, from_port, to, port });
        Ok(())
    }

    /// Nodes in processing order (every node after the nodes that feed it).
    pub fn order(&self) -> Vec<NodeId> {
        let mut indeg: Vec<(NodeId, usize)> = self.nodes.iter().map(|n| (n.id, self.edges.iter().filter(|e| e.to == n.id).count())).collect();
        let mut out = Vec::with_capacity(self.nodes.len());
        while let Some(pos) = indeg.iter().position(|d| d.1 == 0) {
            let (id, _) = indeg.remove(pos);
            out.push(id);
            for e in self.edges.iter().filter(|e| e.from == id) {
                if let Some(d) = indeg.iter_mut().find(|d| d.0 == e.to) {
                    d.1 -= 1;
                }
            }
        }
        out
    }

    /// Nodes that lead to a sink (the rest does not have to be processed).
    pub fn live(&self) -> HashSet<NodeId> {
        let mut live: HashSet<NodeId> = self.nodes.iter().filter(|n| n.kind.is_sink()).map(|n| n.id).collect();
        let mut grew = true;
        while grew {
            grew = false;
            for e in &self.edges {
                if live.contains(&e.to) && live.insert(e.from) {
                    grew = true;
                }
            }
        }
        live
    }

    /// The graph a new user starts with: the chosen input straight into the analyzer.
    pub fn starter(dev: &str, rate_hint: Option<u32>) -> Graph {
        let _ = rate_hint;
        let mut g = Graph::default();
        let i = g.add(Kind::Input, 40.0, 60.0);
        g.node_mut(i).unwrap().s = dev.to_string();
        let a = g.add(Kind::Analyzer, 360.0, 60.0);
        let _ = g.connect(i, a, 0);
        g
    }

    // ------------------------------------------------------------------ text format

    pub fn encode(&self) -> String {
        let mut out = String::from("VSGRAPH 1\n");
        for n in &self.nodes {
            let p: Vec<String> = n.p.iter().map(|v| format!("{v}")).collect();
            out.push_str(&format!("node {}|{}|{}|{}|{}|{}|{}\n", n.id, n.kind.code(), n.x, n.y, n.bypass as u8, esc(&n.s), p.join(",")));
        }
        for e in &self.edges {
            out.push_str(&format!("edge {}|{}|{}|{}\n", e.from, e.to, e.port, e.from_port));
        }
        out
    }

    pub fn decode(text: &str) -> Option<Graph> {
        let mut lines = text.lines();
        if lines.next()?.trim() != "VSGRAPH 1" {
            return None;
        }
        let mut g = Graph::default();
        for l in lines {
            if let Some(r) = l.strip_prefix("node ") {
                let f: Vec<&str> = r.split('|').collect();
                if f.len() < 7 {
                    continue;
                }
                // files of older versions have a separate harmonics node: it is the "harmonic series" type of the distortion now
                let legacy_harm = f[1] == "harm";
                let (Ok(id), Some(kind)) = (f[0].parse::<NodeId>(), if legacy_harm { Some(Kind::Distortion) } else { Kind::from_code(f[1]) }) else { continue };
                if g.nodes.iter().any(|n| n.id == id || (kind == Kind::Analyzer && n.kind == kind)) {
                    continue;
                }
                let mut n = Node::new(id, kind, f[2].parse().unwrap_or(0.0), f[3].parse().unwrap_or(0.0));
                n.bypass = f[4] == "1";
                n.s = unesc(f[5]);
                if legacy_harm {
                    n.p[1] = DIST_HARMONIC as f32;
                    n.p[3] = 100.0;
                    n.p[4] = 0.0;
                }
                for (i, v) in f[6].split(',').enumerate() {
                    let Ok(v) = v.parse::<f32>() else { continue };
                    if !v.is_finite() {
                        continue;
                    }
                    // old harmonics: even, odd, richness, output -> parameters 5, 6, 7 and 4
                    let i = if legacy_harm { [5, 6, 7, 4].get(i).copied().unwrap_or(99) } else { i };
                    if let Some(slot) = n.p.get_mut(i) {
                        *slot = v;
                    } else if kind == Kind::Eq && i == n.p.len() && i < 4 * 12 {
                        // an equaliser keeps four values per extra band
                        n.p.push(v);
                    }
                }
                n.p = n.clamped();
                g.next = g.next.max(id);
                g.nodes.push(n);
            } else if let Some(r) = l.strip_prefix("edge ") {
                let f: Vec<&str> = r.split('|').collect();
                if f.len() >= 3 {
                    if let (Ok(a), Ok(b), Ok(p)) = (f[0].parse(), f[1].parse(), f[2].parse::<u8>()) {
                        let _ = g.connect_from(a, f.get(3).and_then(|v| v.parse().ok()).unwrap_or(0), b, p);
                    }
                }
            }
        }
        Some(g)
    }
}

fn esc(s: &str) -> String {
    s.replace('%', "%25").replace('|', "%7C").replace('\n', "%0A").replace('\r', "")
}

fn unesc(s: &str) -> String {
    s.replace("%0A", "\n").replace("%7C", "|").replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_rules() {
        let mut g = Graph::default();
        let a = g.add(Kind::Input, 0.0, 0.0);
        let f = g.add(Kind::Gain, 0.0, 0.0);
        let o = g.add(Kind::Output, 0.0, 0.0);
        assert!(g.connect(a, f, 0).is_ok());
        assert!(g.connect(f, o, 0).is_ok());
        // sinks have no output, sources no input, no loops
        assert!(g.connect(o, f, 0).is_err());
        assert!(g.connect(f, a, 0).is_err());
        assert!(g.connect(f, f, 0).is_err());
        let f2 = g.add(Kind::Gain, 0.0, 0.0);
        g.connect(f, f2, 0).unwrap();
        assert!(g.connect(f2, f, 0).is_err(), "loop through two nodes");
        // a port takes one connection: the new one replaces the old
        let b = g.add(Kind::Generator, 0.0, 0.0);
        g.connect(b, f, 0).unwrap();
        assert_eq!(g.edge_into(f, 0).unwrap().from, b);
        assert_eq!(g.edges.iter().filter(|e| e.to == f).count(), 1);
        // fan-out is fine
        g.connect(b, f2, 0).unwrap();
        assert_eq!(g.edges.iter().filter(|e| e.from == b).count(), 2);
    }

    #[test]
    fn deleting_an_effect_joins_what_fed_it_to_what_it_fed() {
        let mut g = Graph::default();
        let (a, e1, e2, o1, o2, mx) = (g.add(Kind::Generator, 0.0, 0.0), g.add(Kind::Gain, 0.0, 0.0), g.add(Kind::Eq, 0.0, 0.0), g.add(Kind::Output, 0.0, 0.0), g.add(Kind::Analyzer, 0.0, 0.0), g.add(Kind::Mixer, 0.0, 0.0));
        g.connect(a, e1, 0).unwrap();
        g.connect(e1, e2, 0).unwrap();
        g.connect(e2, o1, 0).unwrap();
        g.connect(e2, o2, 0).unwrap();
        g.remove_bridged(e2);
        assert!(g.node(e2).is_none());
        assert_eq!(g.edge_into(o1, 0).map(|e| e.from), Some(e1));
        assert_eq!(g.edge_into(o2, 0).map(|e| e.from), Some(e1));
        g.remove_bridged(e1);
        assert_eq!(g.edge_into(o1, 0).map(|e| e.from), Some(a));
        assert_eq!(g.edge_into(o2, 0).map(|e| e.from), Some(a));
        // the first effect of a chain has nothing to take over: the line just ends
        let e3 = g.add(Kind::Gain, 0.0, 0.0);
        g.connect(e3, o1, 0).unwrap();
        g.remove_bridged(e3);
        assert!(g.edge_into(o1, 0).is_none());
        // a source, a sink and a mixer are removed without bridging (a mixer has several inputs)
        g.connect(a, mx, 0).unwrap();
        g.connect(mx, o1, 0).unwrap();
        g.remove_bridged(mx);
        assert!(g.edge_into(o1, 0).is_none() && g.node(mx).is_none());
        g.remove_bridged(o2);
        g.remove_bridged(a);
        assert!(g.edges.is_empty());
        // a port that already had another line keeps the line that was bridged in (a port takes one connection)
        let (s1, f, m2) = (g.add(Kind::Generator, 0.0, 0.0), g.add(Kind::Gain, 0.0, 0.0), g.add(Kind::Mixer, 0.0, 0.0));
        g.connect(s1, f, 0).unwrap();
        g.connect(f, m2, 2).unwrap();
        g.remove_bridged(f);
        assert_eq!(g.edge_into(m2, 2).map(|e| e.from), Some(s1));
    }

    #[test]
    fn splitting_a_line_and_an_output_into_channels() {
        let mut g = Graph::default();
        let (a, f, o) = (g.add(Kind::Generator, 0.0, 0.0), g.add(Kind::Gain, 0.0, 0.0), g.add(Kind::Analyzer, 0.0, 0.0));
        g.connect(a, f, 0).unwrap();
        g.connect(f, o, 0).unwrap();
        // in the middle of a line: source -> split -> (left) -> target, the right output is free
        let s = g.split_wire(o, 0, 5.0, 5.0).unwrap();
        assert_eq!(g.node(s).unwrap().kind, Kind::Split);
        assert_eq!(g.edge_into(s, 0).map(|e| (e.from, e.from_port)), Some((f, 0)));
        assert_eq!(g.edge_into(o, 0).map(|e| (e.from, e.from_port)), Some((s, 0)));
        assert!(!g.edges.iter().any(|e| e.from == s && e.from_port == 1));
        // no line, nothing to split
        let lone = g.add(Kind::Output, 0.0, 0.0);
        assert!(g.split_wire(lone, 0, 0.0, 0.0).is_none());
        let n = g.nodes.len();
        // an output: the old lines stay
        let s2 = g.split_output(a, 0, 1.0, 1.0).unwrap();
        assert_eq!(g.edge_into(s2, 0).map(|e| e.from), Some(a));
        assert!(g.edges.iter().any(|e| e.from == a && e.to == f));
        assert_eq!(g.nodes.len(), n + 1);
        // a node without an output cannot be split
        assert!(g.split_output(o, 0, 0.0, 0.0).is_none());
        assert_eq!(g.nodes.len(), n + 1, "the failed attempt leaves no node behind");
    }

    #[test]
    fn an_output_takes_a_stereo_pair_or_two_single_channels() {
        let mut g = Graph::default();
        let (a, sp, out) = (g.add(Kind::Generator, 0.0, 0.0), g.add(Kind::Split, 0.0, 0.0), g.add(Kind::Output, 0.0, 0.0));
        g.connect(a, sp, 0).unwrap();
        assert_eq!(g.node(out).unwrap().inputs(), 1);
        assert!(g.connect_from(sp, 1, out, 1).is_err(), "a stereo output has one input");
        g.node_mut(out).unwrap().p[2] = 1.0;
        assert_eq!(g.node(out).unwrap().inputs(), 2);
        g.connect_from(sp, 0, out, 0).unwrap();
        g.connect_from(sp, 1, out, 1).unwrap();
        // it survives the text format, and an old file (two parameters) still loads as a stereo output
        let h = Graph::decode(&g.encode()).unwrap();
        assert_eq!(h.node(out).unwrap().inputs(), 2);
        assert_eq!(h.edges.len(), g.edges.len());
        let old = Graph::decode("VSGRAPH 1\nnode 1|out|0|0|0||0,0\n").unwrap();
        assert_eq!(old.node(1).unwrap().inputs(), 1);
        assert_eq!(old.node(1).unwrap().p.len(), 3);
    }

    #[test]
    fn order_and_liveness() {
        let mut g = Graph::default();
        let (a, m, o) = (g.add(Kind::Input, 0.0, 0.0), g.add(Kind::Mixer, 0.0, 0.0), g.add(Kind::Output, 0.0, 0.0));
        let dead = g.add(Kind::Generator, 0.0, 0.0);
        g.connect(a, m, 1).unwrap();
        g.connect(m, o, 0).unwrap();
        let ord = g.order();
        let pos = |id| ord.iter().position(|&x| x == id).unwrap();
        assert!(pos(a) < pos(m) && pos(m) < pos(o));
        let live = g.live();
        assert!(live.contains(&a) && live.contains(&m) && !live.contains(&dead));
        g.remove(m);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn an_equaliser_keeps_its_extra_bands_when_saved() {
        let mut g = Graph::default();
        let e = g.add(Kind::Eq, 0.0, 0.0);
        g.node_mut(e).unwrap().p.extend([5.0, 2500.0, -20.0, 3.0, 0.0, 6000.0, 4.0, 1.5]);
        let h = Graph::decode(&g.encode()).unwrap();
        let p = &h.node(e).unwrap().p;
        assert_eq!(p.len(), 24);
        assert_eq!(&p[16..20], &[5.0, 2500.0, -20.0, 3.0]);
        assert_eq!(p[23], 1.5);
    }

    #[test]
    fn damaged_graph_text_never_panics_and_is_limited() {
        let mut g = Graph::starter("dev", None);
        for k in Kind::ALL {
            if k != Kind::Analyzer {
                g.add(k, 10.0, 10.0);
            }
        }
        let good = g.encode();
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..1500 {
            let mut b = good.clone().into_bytes();
            for _ in 0..1 + next() % 6 {
                let i = (next() % b.len() as u64) as usize;
                b[i] = match next() % 4 {
                    0 => b'9',
                    1 => b'|',
                    2 => b'-',
                    _ => next() as u8,
                };
            }
            if next() % 4 == 0 {
                b.truncate((next() % b.len() as u64) as usize);
            }
            let Ok(text) = String::from_utf8(b) else { continue };
            let result = std::panic::catch_unwind(|| {
                if let Some(h) = Graph::decode(&text) {
                    let _ = (h.order(), h.live(), h.encode());
                    for n in &h.nodes {
                        // whatever was in the file, the values are inside their ranges
                        let defs = n.kind.info().params;
                        for (i, v) in n.p.iter().enumerate() {
                            let d = if n.kind == Kind::Eq { defs.get(i % 16) } else { defs.get(i) };
                            if let Some(d) = d {
                                assert!(v.is_finite() && *v >= d.min && *v <= d.max, "{:?} param {i} = {v}", n.kind);
                            }
                        }
                        assert!(n.inputs() >= 1 || n.kind.is_source());
                    }
                }
            });
            assert!(result.is_ok(), "decode panicked on: {text:?}");
        }
    }

    #[test]
    fn one_analyzer_and_roundtrip() {
        let mut g = Graph::starter("dev|1%2", None);
        assert_eq!(g.add(Kind::Analyzer, 5.0, 5.0), g.nodes.iter().find(|n| n.kind == Kind::Analyzer).unwrap().id);
        assert_eq!(g.nodes.iter().filter(|n| n.kind == Kind::Analyzer).count(), 1);
        let e = g.add(Kind::Eq, 10.0, 20.0);
        g.node_mut(e).unwrap().p[2] = 6.5;
        g.node_mut(e).unwrap().bypass = true;
        let m = g.add(Kind::Mixer, 0.0, 0.0);
        g.node_mut(m).unwrap().p[0] = 6.0;
        g.connect(1, m, 5).unwrap();
        let h = Graph::decode(&g.encode()).unwrap();
        assert_eq!(h.nodes.len(), g.nodes.len());
        assert_eq!(h.edges.len(), g.edges.len());
        assert_eq!(h.node(1).unwrap().s, "dev|1%2");
        assert_eq!(h.node(e).unwrap().p[2], 6.5);
        assert!(h.node(e).unwrap().bypass);
        assert_eq!(h.node(m).unwrap().inputs(), 6);
        assert!(Graph::decode("nonsense").is_none());
        // new ids never collide with loaded ones
        let mut h = h;
        let n = h.add(Kind::Gain, 0.0, 0.0);
        assert!(g.nodes.iter().all(|x| x.id != n));
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn old_harmonics_nodes_load_as_the_harmonic_series_of_the_distortion() {
        let text = "VSGRAPH 1\nnode 3|harm|10|20|0||40,15,70,-3\nnode 4|dist|10|20|1||24,2,8000,50,-9\n";
        let g = Graph::decode(text).unwrap();
        let h = g.node(3).unwrap();
        assert_eq!(h.kind, Kind::Distortion);
        assert_eq!(h.param(1) as usize, DIST_HARMONIC);
        assert_eq!((h.p[5], h.p[6], h.p[7], h.p[4]), (40.0, 15.0, 70.0, -3.0));
        assert!(h.shows(5) && !h.shows(0));
        // an old distortion keeps its values and the new ones get their defaults
        let d = g.node(4).unwrap();
        assert_eq!((d.p[0], d.p[1], d.p[2], d.p[3], d.p[4]), (24.0, 2.0, 8000.0, 50.0, -9.0));
        assert_eq!(d.p.len(), 8);
        assert!(d.shows(0) && !d.shows(5));
    }

    #[test]
    fn every_kind_has_a_code_and_round_trips() {
        let mut g = Graph::default();
        for k in Kind::ALL {
            g.add(k, 0.0, 0.0);
        }
        let h = Graph::decode(&g.encode()).unwrap();
        assert_eq!(h.nodes.len(), g.nodes.len());
    }
}
