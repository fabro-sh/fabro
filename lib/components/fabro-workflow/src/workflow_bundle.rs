use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use fabro_types::ManifestPath;
use serde::{Deserialize, Serialize};

use crate::file_resolver::{BundleFileResolver, FileResolver};

pub const WORKFLOW_PACKAGE_DIR_NAMES: [&str; 3] = ["scripts", "references", "assets"];
pub const MAX_WORKFLOW_RUNTIME_FILES: usize = 256;
pub const MAX_WORKFLOW_RUNTIME_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParsedWorkflowConfig {
    pub path:   ManifestPath,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BundledWorkflow {
    pub path:          ManifestPath,
    pub source:        String,
    pub config:        Option<ParsedWorkflowConfig>,
    pub files:         HashMap<ManifestPath, String>,
    #[serde(default)]
    pub runtime_files: HashMap<ManifestPath, String>,
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

#[must_use]
pub fn is_workflow_runtime_file_path(
    workflow_path: &ManifestPath,
    candidate: &ManifestPath,
) -> bool {
    if candidate
        .as_path()
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }

    let package_dir = workflow_path.parent_or_dot();
    let relative = if package_dir.as_os_str().is_empty() || package_dir == Path::new(".") {
        candidate.as_path()
    } else {
        let Ok(relative) = candidate.as_path().strip_prefix(package_dir) else {
            return false;
        };
        relative
    };
    let mut components = relative.components();
    let Some(Component::Normal(package_dir_name)) = components.next() else {
        return false;
    };
    if !WORKFLOW_PACKAGE_DIR_NAMES
        .iter()
        .any(|allowed| package_dir_name == *allowed)
    {
        return false;
    }

    matches!(components.next(), Some(Component::Normal(_)))
        && components.all(|component| matches!(component, Component::Normal(_)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("test path should be valid")
    }

    #[test]
    fn workflow_runtime_file_path_accepts_reserved_package_directories() {
        let workflow = manifest_path(".fabro/workflows/security-review/workflow.fabro");

        for path in [
            ".fabro/workflows/security-review/scripts/review.py",
            ".fabro/workflows/security-review/references/policy.md",
            ".fabro/workflows/security-review/assets/fixtures/input.json",
        ] {
            assert!(is_workflow_runtime_file_path(
                &workflow,
                &manifest_path(path)
            ));
        }
    }

    #[test]
    fn workflow_runtime_file_path_rejects_paths_outside_reserved_directories() {
        let workflow = manifest_path(".fabro/workflows/security-review/workflow.fabro");

        for path in [
            "../secrets.txt",
            ".fabro/workflows/other/scripts/review.py",
            ".fabro/workflows/security-review/workflow.toml",
            ".fabro/workflows/security-review/scripts",
        ] {
            assert!(!is_workflow_runtime_file_path(
                &workflow,
                &manifest_path(path)
            ));
        }
    }

    #[test]
    fn workflow_runtime_file_path_supports_a_workflow_at_the_workspace_root() {
        assert!(is_workflow_runtime_file_path(
            &manifest_path("workflow.fabro"),
            &manifest_path("scripts/review.py")
        ));
    }

    #[test]
    fn bundled_workflow_deserializes_definitions_without_runtime_files() {
        let workflow: BundledWorkflow = serde_json::from_value(serde_json::json!({
            "path": "workflow.fabro",
            "source": "digraph Demo {}",
            "config": null,
            "files": {}
        }))
        .expect("existing bundled workflow should deserialize");

        assert!(workflow.runtime_files.is_empty());
    }
}
