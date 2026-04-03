use std::path::Path;

pub use fabro_types::run::RunRecord;

use crate::error::{FabroError, Result as CrateResult};

const FILE_NAME: &str = "run.json";

pub trait RunRecordExt {
    fn save(&self, run_dir: &Path) -> CrateResult<()>;
    fn load(run_dir: &Path) -> CrateResult<Self>
    where
        Self: Sized;
    fn workflow_name(&self) -> &str;
    fn goal(&self) -> &str;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
}

impl RunRecordExt for RunRecord {
    fn save(&self, run_dir: &Path) -> CrateResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| FabroError::Checkpoint(format!("run record serialize failed: {e}")))?;
        std::fs::write(run_dir.join(FILE_NAME), json)?;
        Ok(())
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
