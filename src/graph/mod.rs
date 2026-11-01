//! Routing graph: every input and output of the program as a node, connected like a patch bay, with
//! effects in between. The graph is optional (Options); while it is on it replaces the fixed
//! "input device -> analysers" path and the separate device pickers of the windows.

pub mod dsp;
pub mod editor;
pub mod engine;
pub mod model;

pub use engine::bus;
