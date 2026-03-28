use fabro_graphviz::graph::Graph;

/// A transform that modifies the pipeline graph after parsing and before validation.
pub trait Transform {
    fn apply(&self, graph: &mut Graph);
}

mod file_inlining;
mod graph_merge;
mod model_resolution;
mod preamble;
pub mod stylesheet;
mod stylesheet_application;
pub mod variable_expansion;

pub use file_inlining::{FileInliningTransform, resolve_file_ref};
pub use graph_merge::GraphMergeTransform;
pub use model_resolution::ModelResolutionTransform;
pub use preamble::PreambleTransform;
pub use stylesheet_application::StylesheetApplicationTransform;
pub use variable_expansion::{VariableExpansionTransform, expand_vars};
