use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::settings::{
    CliNamespace, ObjectStoreSettings, ProjectNamespace, RunNamespace, ServerArtifactsSettings,
    ServerNamespace, WorkflowNamespace,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub server: ServerNamespace,
}

impl ServerSettings {
    #[must_use]
    pub fn with_storage_override(mut self, path: &Path) -> Self {
        // Only the derived default follows the storage directory. A custom
        // artifact location is independent of the database and runtime root.
        let default_artifact_root =
            ServerArtifactsSettings::default_local_root(Path::new(&self.server.storage.root));
        if let ObjectStoreSettings::Local { root } = &mut self.server.artifacts.store {
            if *root == default_artifact_root {
                *root = ServerArtifactsSettings::default_local_root(path);
            }
        }
        self.server.storage.root = path.display().to_string();
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSettings {
    pub cli: CliNamespace,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSettings {
    pub project:  ProjectNamespace,
    pub workflow: WorkflowNamespace,
    pub run:      RunNamespace,
}

impl WorkflowSettings {
    #[must_use]
    pub fn combined_labels(&self) -> HashMap<String, String> {
        let mut labels = self.project.metadata.clone();
        labels.extend(self.workflow.metadata.clone());
        labels.extend(self.run.metadata.clone());
        labels
    }
}
