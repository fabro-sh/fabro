use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ManifestPath;
use crate::file_resolver::{BundleFileResolver, FileResolver};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParsedWorkflowConfig {
    pub path:   ManifestPath,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BundledWorkflow {
    pub path:   ManifestPath,
    pub source: String,
    pub config: Option<ParsedWorkflowConfig>,
    pub files:  HashMap<ManifestPath, String>,
}

impl BundledWorkflow {
    #[must_use]
    pub fn file_resolver(&self) -> Arc<dyn FileResolver> {
        Arc::new(BundleFileResolver::new(self.files.clone()))
    }

    #[must_use]
    pub fn current_dir(&self) -> PathBuf {
        self.path.parent_or_dot().to_path_buf()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowBundle {
    workflows: HashMap<ManifestPath, BundledWorkflow>,
}

impl WorkflowBundle {
    #[must_use]
    pub fn new(workflows: HashMap<ManifestPath, BundledWorkflow>) -> Self {
        Self { workflows }
    }

    pub fn workflow(&self, path: &ManifestPath) -> Option<&BundledWorkflow> {
        self.workflows.get(path)
    }

    pub fn resolve_child(
        &self,
        current_workflow_path: &ManifestPath,
        reference: &str,
    ) -> Option<&BundledWorkflow> {
        let path = ManifestPath::from_reference(current_workflow_path.parent_or_dot(), reference)?;
        self.workflows.get(&path)
    }

    #[must_use]
    pub fn workflows(&self) -> &HashMap<ManifestPath, BundledWorkflow> {
        &self.workflows
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunDefinition {
    pub workflow_path: ManifestPath,
    pub workflows:     HashMap<ManifestPath, BundledWorkflow>,
}

impl RunDefinition {
    #[must_use]
    pub fn new(workflow_path: ManifestPath, bundle: WorkflowBundle) -> Self {
        Self {
            workflow_path,
            workflows: bundle.workflows,
        }
    }

    #[must_use]
    pub fn workflow_bundle(&self) -> WorkflowBundle {
        WorkflowBundle::new(self.workflows.clone())
    }
}
