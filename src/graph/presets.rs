//! Ready-made routing patches the assistant can put into the node editor for a task.

use super::model::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    /// the chosen input into the analysers; a generator into the chosen output (sweeps, tones, loopback measurements)
    Measure,
    /// the input into the analysers and to the output (listen while looking)
    Monitor,
    /// the input into the analysers
    Analyze,
    /// the input into the analysers and onto a new armed timeline track
    Record,
    /// the input through an equaliser to the output, and the analysers (tune the sound while looking)
    MonitorEq,
    /// the input through noise reduction to the output, and the analysers
    Denoise,
}

impl Preset {
    /// Every preset, for the menu.
    pub const ALL: [Preset; 6] = [Preset::Analyze, Preset::Monitor, Preset::MonitorEq, Preset::Denoise, Preset::Measure, Preset::Record];

    /// One line for the menu: what the patch is for.
    pub fn about(self) -> &'static str {
        match self {
            Preset::Analyze => "Look at what comes in: spectrum, scope, loudness",
            Preset::Monitor => "Hear the input on the output while looking at it",
            Preset::MonitorEq => "Shape the sound with an equaliser while listening and looking",
            Preset::Denoise => "Take steady noise out of the input (learn the noise in the node)",
            Preset::Measure => "Play test signals out and measure what comes back (sweeps, tones)",
            Preset::Record => "Record the input onto a new timeline track",
        }
    }
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::Measure => "Measure: generator to the output, input to the analysers",
            Preset::Monitor => "Monitor: input to the analysers and to the output",
            Preset::Analyze => "Analyse: input to the analysers",
            Preset::Record => "Record: input to the analysers and to a new timeline track",
            Preset::MonitorEq => "Equaliser: input through an equaliser to the output",
            Preset::Denoise => "Noise reduction: input through noise reduction to the output",
        }
    }

    /// Whether the patch has an output device node (the wizard asks for the output then).
    pub fn uses_output(self) -> bool {
        matches!(self, Preset::Measure | Preset::Monitor | Preset::MonitorEq | Preset::Denoise)
    }
}

/// The patch: `in_dev` / `out_dev` are device ids (empty = the default), the channels are 1 based.
pub fn build(p: Preset, in_dev: &str, in_l: usize, in_r: usize, out_dev: &str) -> Graph {
    let mut g = Graph::default();
    let inp = g.add(Kind::Input, 40.0, 40.0);
    if let Some(n) = g.node_mut(inp) {
        n.s = in_dev.to_string();
        n.p[0] = in_l.clamp(1, 32) as f32;
        n.p[1] = in_r.clamp(1, 32) as f32;
    }
    let an = g.add(Kind::Analyzer, 740.0, 40.0);
    let _ = g.connect(inp, an, 0);
    match p {
        Preset::MonitorEq | Preset::Denoise => {
            let fx = g.add(if p == Preset::MonitorEq { Kind::Eq } else { Kind::NoiseRed }, 400.0, 40.0);
            let out = g.add(Kind::Output, 740.0, 260.0);
            if let Some(n) = g.node_mut(out) {
                n.s = out_dev.to_string();
            }
            let _ = g.connect(inp, fx, 0);
            let _ = g.connect(fx, out, 0);
            // the analysers see what is heard
            let _ = g.connect(fx, an, 0);
        }
        Preset::Measure => {
            let generator = g.add(Kind::Generator, 40.0, 260.0);
            let out = g.add(Kind::Output, 400.0, 260.0);
            if let Some(n) = g.node_mut(out) {
                n.s = out_dev.to_string();
            }
            let _ = g.connect(generator, out, 0);
        }
        Preset::Monitor => {
            let out = g.add(Kind::Output, 400.0, 260.0);
            if let Some(n) = g.node_mut(out) {
                n.s = out_dev.to_string();
            }
            let _ = g.connect(inp, out, 0);
        }
        Preset::Record => {
            let rec = g.add(Kind::TimelineRec, 400.0, 260.0);
            let _ = g.connect(inp, rec, 0);
        }
        Preset::Analyze => {}
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_a_complete_loop_free_patch() {
        for p in Preset::ALL {
            let g = build(p, "dev|1", 3, 4, "out|2");
            let h = Graph::decode(&g.encode()).unwrap();
            assert_eq!(h.nodes.len(), g.nodes.len(), "{p:?}");
            assert_eq!(h.edges.len(), g.edges.len(), "{p:?} keeps its wires through the text format");
            // everything that is not a sink leads to a sink: nothing is left hanging
            assert_eq!(g.live().len(), g.nodes.len(), "{p:?}");
            let i = g.nodes.iter().find(|n| n.kind == Kind::Input).unwrap();
            assert_eq!((i.s.as_str(), i.p[0], i.p[1]), ("dev|1", 3.0, 4.0));
            assert_eq!(g.nodes.iter().any(|n| n.kind == Kind::Output), p.uses_output());
            if p == Preset::Measure {
                assert_eq!(g.nodes.iter().find(|n| n.kind == Kind::Output).unwrap().s, "out|2");
            }
        }
    }
}
