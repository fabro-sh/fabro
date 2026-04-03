use std::path::Path;

pub use fabro_types::run::RunRecord;

use crate::error::Result as CrateResult;

const FILE_NAME: &str = "run.json";

pub trait RunRecordExt {
    fn file_name() -> &'static str
    where
        Self: Sized;
    fn load(run_dir: &Path) -> CrateResult<Self>
    where
        Self: Sized;
    fn workflow_name(&self) -> &str;
    fn goal(&self) -> &str;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
}

impl RunRecordExt for RunRecord {
    fn file_name() -> &'static str {
        FILE_NAME
    }

    fn load(run_dir: &Path) -> CrateResult<Self> {
        crate::load_json(&run_dir.join(FILE_NAME), "run record")
    }

    fn workflow_name(&self) -> &str {
        if self.graph.name.is_empty() {
            "unnamed"
        } else {
            &self.graph.name
        }
    }

    fn goal(&self) -> &str {
        self.graph.goal()
    }

    fn node_count(&self) -> usize {
        self.graph.nodes.len()
    }

    fn edge_count(&self) -> usize {
        self.graph.edges.len()
    }
}
