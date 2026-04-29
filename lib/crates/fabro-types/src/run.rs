use std::collections::HashMap;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirtyStatus {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreRunPushOutcome {
    NotAttempted,
    Succeeded {
        remote: String,
        branch: String,
    },
    Failed {
        remote:  String,
        branch:  String,
        message: String,
    },
    SkippedNoRemote,
    SkippedRemoteMismatch {
        remote:          String,
        repo_origin_url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitContext {
    pub origin_url:   String,
    pub branch:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha:          Option<String>,
    pub dirty:        DirtyStatus,
    pub push_outcome: PreRunPushOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkSourceRef {
    pub source_run_id:  RunId,
    pub checkpoint_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub run_id:           RunId,
    pub settings:         WorkflowSettings,
    pub graph:            Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_directory: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels:           HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance:       Option<RunProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_blob:    Option<RunBlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_blob:  Option<RunBlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git:              Option<GitContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_source_ref:  Option<ForkSourceRef>,
    #[serde(default)]
    pub in_place:         bool,
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
    pub fn source_directory(&self) -> Option<&str> {
        self.source_directory.as_deref()
    }

    #[must_use]
    pub fn labels(&self) -> &HashMap<String, String> {
        &self.labels
    }

    #[must_use]
    pub fn repo_origin_url(&self) -> Option<&str> {
        self.git.as_ref().map(|git| git.origin_url.as_str())
    }

    #[must_use]
    pub fn base_branch(&self) -> Option<&str> {
        self.git.as_ref().map(|git| git.branch.as_str())
    }

    #[must_use]
    pub fn git(&self) -> Option<&GitContext> {
        self.git.as_ref()
    }

    #[must_use]
    pub fn fork_source_ref(&self) -> Option<&ForkSourceRef> {
        self.fork_source_ref.as_ref()
    }
}
