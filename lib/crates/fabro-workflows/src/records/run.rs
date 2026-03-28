use std::path::Path;

pub use fabro_types::run::RunRecord;

const FILE_NAME: &str = "run.json";

pub trait RunRecordExt {
    fn file_name() -> &'static str
    where
        Self: Sized;
    fn save(&self, run_dir: &Path) -> crate::error::Result<()>;
    fn load(run_dir: &Path) -> crate::error::Result<Self>
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

    fn save(&self, run_dir: &Path) -> crate::error::Result<()> {
        crate::save_json(self, &run_dir.join(FILE_NAME), "run record")
    }

    fn load(run_dir: &Path) -> crate::error::Result<Self> {
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
