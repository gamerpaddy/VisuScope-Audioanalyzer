//! The routing graph as plain data: nodes with parameters, stereo connections, editing rules and the
//! text format it is stored in. No audio and no UI in here.

use std::collections::HashSet;

pub type NodeId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Input,
    Generator,
    TimelineOut,
    FilePlayer,
    Gain,
    Eq,
    Filter,
    Comp,
    Limiter,
    Gate,
    Delay,
    Stereo,
    Utility,
    Mixer,
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
    c("1 type", 1, EQ_TYPES), l("1 freq", 20.0, 20000.0, 100.0, " Hz"), s("1 gain", -18.0, 18.0, 0.0, " dB"), l("1 Q", 0.1, 10.0, 0.7, ""),
    c("2 type", 0, EQ_TYPES), l("2 freq", 20.0, 20000.0, 500.0, " Hz"), s("2 gain", -18.0, 18.0, 0.0, " dB"), l("2 Q", 0.1, 10.0, 1.0, ""),
    c("3 type", 0, EQ_TYPES), l("3 freq", 20.0, 20000.0, 2500.0, " Hz"), s("3 gain", -18.0, 18.0, 0.0, " dB"), l("3 Q", 0.1, 10.0, 1.0, ""),
    c("4 type", 2, EQ_TYPES), l("4 freq", 20.0, 20000.0, 8000.0, " Hz"), s("4 gain", -18.0, 18.0, 0.0, " dB"), l("4 Q", 0.1, 10.0, 0.7, ""),
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
const P_STEREO: &[ParamDef] = &[s("Width", 0.0, 200.0, 100.0, " %"), s("Balance", -100.0, 100.0, 0.0, " %")];
const P_UTIL: &[ParamDef] = &[c("Mode", 0, &["Stereo (through)", "Left to both", "Right to both", "Mono sum", "Swap L/R", "Invert left", "Invert right", "Invert both"])];
const P_MIXER: &[ParamDef] = &[
    s("Inputs", 2.0, MIX_MAX as f32, 4.0, ""),
    s("Master", -60.0, 12.0, 0.0, " dB"),
    s("In 1", -60.0, 12.0, 0.0, " dB"), s("In 2", -60.0, 12.0, 0.0, " dB"), s("In 3", -60.0, 12.0, 0.0, " dB"), s("In 4", -60.0, 12.0, 0.0, " dB"),
    s("In 5", -60.0, 12.0, 0.0, " dB"), s("In 6", -60.0, 12.0, 0.0, " dB"), s("In 7", -60.0, 12.0, 0.0, " dB"), s("In 8", -60.0, 12.0, 0.0, " dB"),
    s("Mute 1", 0.0, 1.0, 0.0, ""), s("Mute 2", 0.0, 1.0, 0.0, ""), s("Mute 3", 0.0, 1.0, 0.0, ""), s("Mute 4", 0.0, 1.0, 0.0, ""),
    s("Mute 5", 0.0, 1.0, 0.0, ""), s("Mute 6", 0.0, 1.0, 0.0, ""), s("Mute 7", 0.0, 1.0, 0.0, ""), s("Mute 8", 0.0, 1.0, 0.0, ""),
];
const P_OUTPUT: &[ParamDef] = &[s("Gain", -60.0, 12.0, 0.0, " dB"), c("Mute", 0, &["on", "muted"])];

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
    pub const ALL: [Kind; 17] = [
        Kind::Input,
        Kind::Generator,
        Kind::TimelineOut,
        Kind::FilePlayer,
        Kind::Gain,
        Kind::Eq,
        Kind::Filter,
        Kind::Comp,
        Kind::Limiter,
        Kind::Gate,
        Kind::Delay,
        Kind::Stereo,
        Kind::Utility,
        Kind::Mixer,
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
            Kind::FilePlayer => k("File player", "Source", "recorder wav mp3 file playback", 0, 1, P_NONE, SRC),
            Kind::Gain => k("Gain", "Effect", "volume level trim phase invert", 1, 1, P_GAIN, FX),
            Kind::Eq => k("Equalizer", "Effect", "eq parametric bell shelf tone", 1, 1, P_EQ, FX),
            Kind::Filter => k("Filter", "Effect", "highpass lowpass hp lp cut butterworth", 1, 1, P_FILTER, FX),
            Kind::Comp => k("Compressor", "Effect", "dynamics compress threshold ratio", 1, 1, P_COMP, FX),
            Kind::Limiter => k("Limiter", "Effect", "dynamics ceiling brickwall peak", 1, 1, P_LIMIT, FX),
            Kind::Gate => k("Noise gate", "Effect", "dynamics expander silence", 1, 1, P_GATE, FX),
            Kind::Delay => k("Delay", "Effect", "echo time feedback", 1, 1, P_DELAY, FX),
            Kind::Stereo => k("Stereo width", "Effect", "width balance pan mid side", 1, 1, P_STEREO, FX),
            Kind::Utility => k("Channel utility", "Effect", "mono swap invert left right channel phase", 1, 1, P_UTIL, FX),
            Kind::Mixer => k("Mixer", "Routing", "mix sum combine merge bus", MIX_MAX, 1, P_MIXER, (150, 120, 70)),
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
            Kind::FilePlayer => "file",
            Kind::Gain => "gain",
            Kind::Eq => "eq",
            Kind::Filter => "filt",
            Kind::Comp => "comp",
            Kind::Limiter => "lim",
            Kind::Gate => "gate",
            Kind::Delay => "dly",
            Kind::Stereo => "wid",
            Kind::Utility => "util",
            Kind::Mixer => "mix",
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

    /// Number of input ports (a mixer has as many as its "Inputs" parameter says).
    pub fn inputs(&self) -> usize {
        match self.kind {
            Kind::Mixer => (self.p.first().copied().unwrap_or(4.0).round() as usize).clamp(2, MIX_MAX),
            k => k.info().inputs,
        }
    }

    pub fn outputs(&self) -> usize {
        self.kind.info().outputs
    }

    pub fn param(&self, i: usize) -> f32 {
        self.p.get(i).copied().unwrap_or_else(|| self.kind.info().params.get(i).map_or(0.0, |d| d.def))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    pub from: NodeId,
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

    /// Connect the output of `from` to an input port of `to`. An existing connection on that port is replaced.
    pub fn connect(&mut self, from: NodeId, to: NodeId, port: u8) -> Result<(), &'static str> {
        let (Some(f), Some(t)) = (self.node(from), self.node(to)) else { return Err("unknown node") };
        if f.outputs() == 0 {
            return Err("this node has no output");
        }
        if port as usize >= t.inputs() {
            return Err("this node has no such input");
        }
        if from == to || self.reaches(to, from) {
            return Err("that would make a loop");
        }
        self.disconnect(to, port);
        self.edges.push(Edge { from, to, port });
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
            out.push_str(&format!("edge {}|{}|{}\n", e.from, e.to, e.port));
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
                let (Ok(id), Some(kind)) = (f[0].parse::<NodeId>(), Kind::from_code(f[1])) else { continue };
                if g.nodes.iter().any(|n| n.id == id || (kind == Kind::Analyzer && n.kind == kind)) {
                    continue;
                }
                let mut n = Node::new(id, kind, f[2].parse().unwrap_or(0.0), f[3].parse().unwrap_or(0.0));
                n.bypass = f[4] == "1";
                n.s = unesc(f[5]);
                for (i, v) in f[6].split(',').enumerate() {
                    if let (Some(slot), Ok(v)) = (n.p.get_mut(i), v.parse::<f32>()) {
                        if v.is_finite() {
                            *slot = v;
                        }
                    }
                }
                g.next = g.next.max(id);
                g.nodes.push(n);
            } else if let Some(r) = l.strip_prefix("edge ") {
                let f: Vec<&str> = r.split('|').collect();
                if f.len() >= 3 {
                    if let (Ok(a), Ok(b), Ok(p)) = (f[0].parse(), f[1].parse(), f[2].parse::<u8>()) {
                        let _ = g.connect(a, b, p);
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
