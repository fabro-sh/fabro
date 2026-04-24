use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::WorkflowSettings;
use crate::graph::Graph;
use crate::run_blob_id::RunBlobId;
use crate::run_id::RunId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAuthMethod {
    Disabled,
    DevToken,
    Github,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunServerProvenance {
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunClientProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSubjectProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login:       Option<String>,
    pub auth_method: RunAuthMethod,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server:  Option<RunServerProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client:  Option<RunClientProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<RunSubjectProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub run_id:            RunId,
    pub settings:          WorkflowSettings,
    pub graph:             Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug:     Option<String>,
    pub working_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_origin_url:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch:       Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels:            HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance:        Option<RunProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_blob:     Option<RunBlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_blob:   Option<RunBlobId>,
}

impl RunSpec {
    #[must_use]
    pub fn id(&self) -> RunId {
        self.run_id
    }

    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    #[must_use]
    pub fn settings(&self) -> &WorkflowSettings {
        &self.settings
    }

    #[must_use]
    pub fn workflow_slug(&self) -> Option<&str> {
        self.workflow_slug.as_deref()
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    #[must_use]
    pub fn labels(&self) -> &HashMap<String, String> {
        &self.labels
    }

    #[must_use]
    pub fn host_repo_path(&self) -> Option<&str> {
        self.host_repo_path.as_deref()
    }

    #[must_use]
    pub fn repo_origin_url(&self) -> Option<&str> {
        self.repo_origin_url.as_deref()
    }

    #[must_use]
    pub fn base_branch(&self) -> Option<&str> {
        self.base_branch.as_deref()
    }
}
