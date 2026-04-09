//! Run domain.
//!
//! `[run]` is the shared execution domain. It may appear in all three config
//! files and layer normally. This file holds only the Stage-1 skeleton; the
//! rich subtree (model, git, prepare, execution, checkpoint, sandbox,
//! notifications, interviews, agent, hooks, scm, pull_request, artifacts) is
//! filled in during Stage 2.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A sparse `[run]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Run-time inputs. Stage 2 will widen the value type beyond strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, toml::Value>>,
}
