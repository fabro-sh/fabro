use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::FabroError;
use crate::file_resolver::{BundleFileResolver, FileResolver, normalize_logical_path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BundledWorkflow {
    pub logical_path: PathBuf,
    pub source: String,
    pub files: HashMap<PathBuf, String>,
}

impl BundledWorkflow {
    #[must_use]
    pub fn file_resolver(&self) -> Arc<dyn FileResolver> {
        Arc::new(BundleFileResolver::new(self.files.clone()))
    }

    #[must_use]
    pub fn current_dir(&self) -> PathBuf {
        self.logical_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowBundle {
    workflows: HashMap<PathBuf, BundledWorkflow>,
}

impl WorkflowBundle {
    #[must_use]
    pub fn new(workflows: HashMap<PathBuf, BundledWorkflow>) -> Self {
        Self { workflows }
    }

    pub fn workflow(&self, logical_path: &Path) -> Option<&BundledWorkflow> {
        self.workflows.get(logical_path)
    }

    pub fn resolve_child(
        &self,
        current_workflow_path: &Path,
        reference: &str,
    ) -> Option<&BundledWorkflow> {
        let current_dir = current_workflow_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let logical_path = normalize_logical_path(current_dir, reference)?;
        self.workflows.get(&logical_path)
    }

    #[must_use]
    pub fn workflows(&self) -> &HashMap<PathBuf, BundledWorkflow> {
        &self.workflows
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredWorkflowBundle {
    pub workflow_path: PathBuf,
    pub workflows: HashMap<PathBuf, BundledWorkflow>,
}

impl StoredWorkflowBundle {
    #[must_use]
    pub fn new(workflow_path: PathBuf, bundle: WorkflowBundle) -> Self {
        Self {
            workflow_path,
            workflows: bundle.workflows,
        }
    }

    #[must_use]
    pub fn workflow_bundle(&self) -> WorkflowBundle {
        WorkflowBundle::new(self.workflows.clone())
    }

    pub fn load_from_run_dir(run_dir: &Path) -> Result<Option<Self>, FabroError> {
        let path = run_dir.join("workflow_bundle.json");
        if !path.exists() {
            return Ok(None);
        }

        let payload =
            std::fs::read_to_string(&path).map_err(|err| FabroError::Io(err.to_string()))?;
        serde_json::from_str(&payload)
            .map(Some)
            .map_err(|err| FabroError::Parse(err.to_string()))
    }
}
