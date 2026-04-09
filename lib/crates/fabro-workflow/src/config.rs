//! Workflow runtime configuration shapes.
//!
//! Runtime-side types consumed by the pipeline. The v2 parse tree lives in
//! `fabro_types::settings::v2::run::{RunPullRequestLayer, MergeStrategy,
//! RunArtifactsLayer}`. Conversion from v2 lives in [`bridge_pull_request`]
//! / [`bridge_run_artifacts`].

use fabro_types::settings::v2::run::{
    MergeStrategy as V2MergeStrategy, RunArtifactsLayer, RunPullRequestLayer,
};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PullRequestSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub draft: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub merge_strategy: MergeStrategy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    #[default]
    Squash,
    Merge,
    Rebase,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArtifactsSettings {
    #[serde(default)]
    pub include: Vec<String>,
}

#[must_use]
pub fn bridge_merge_strategy(m: V2MergeStrategy) -> MergeStrategy {
    match m {
        V2MergeStrategy::Squash => MergeStrategy::Squash,
        V2MergeStrategy::Merge => MergeStrategy::Merge,
        V2MergeStrategy::Rebase => MergeStrategy::Rebase,
    }
}

#[must_use]
pub fn bridge_pull_request(pr: &RunPullRequestLayer) -> PullRequestSettings {
    PullRequestSettings {
        enabled: pr.enabled.unwrap_or(false),
        draft: pr.draft.unwrap_or(true),
        auto_merge: pr.auto_merge.unwrap_or(false),
        merge_strategy: pr
            .merge_strategy
            .map(bridge_merge_strategy)
            .unwrap_or_default(),
    }
}

#[must_use]
pub fn bridge_run_artifacts(artifacts: &RunArtifactsLayer) -> ArtifactsSettings {
    ArtifactsSettings {
        include: artifacts.include.clone(),
    }
}
