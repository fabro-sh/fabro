use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::settings::{
    CliNamespace, FeaturesNamespace, InterpString, ObjectStoreSettings, ProjectNamespace,
    RunNamespace, ServerNamespace, WorkflowNamespace,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub server:   ServerNamespace,
    pub features: FeaturesNamespace,
}

impl ServerSettings {
    #[must_use]
    pub fn with_storage_override(mut self, path: &Path) -> Self {
        self.server.storage.root = InterpString::parse(&path.display().to_string());
        override_local_object_store_root(&mut self.server.artifacts.store, path, "artifacts");
        override_local_object_store_root(&mut self.server.slatedb.store, path, "slatedb");
        self
    }
}

fn override_local_object_store_root(
    store: &mut ObjectStoreSettings,
    storage_root: &Path,
    domain: &str,
) {
    let ObjectStoreSettings::Local { root } = store else {
        return;
    };
    *root = InterpString::parse(
        &storage_root
            .join("objects")
            .join(domain)
            .display()
            .to_string(),
    );
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSettings {
    pub cli:      CliNamespace,
    pub features: FeaturesNamespace,
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
