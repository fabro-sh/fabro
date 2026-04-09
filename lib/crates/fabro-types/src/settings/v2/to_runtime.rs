//! v2 → runtime-type conversion helpers.
//!
//! Everything but the pull-request / artifacts / merge-strategy conversion
//! has moved out of this module into the consumer crate that owns the
//! target runtime type:
//!
//! - Hook bridging: [`fabro_hooks::config::bridge_hook`]
//! - MCP bridging: [`fabro_mcp::config::bridge_mcp_entry`] /
//!   [`fabro_mcp::config::bridge_mcps`]
//! - Sandbox bridging: [`fabro_sandbox::config::bridge_sandbox`] /
//!   [`fabro_sandbox::config::bridge_worktree_mode`]
//!
//! Pull-request / artifacts / merge-strategy still live here because their
//! target runtime types (`PullRequestSettings`, `ArtifactsSettings`,
//! `MergeStrategy`) are still in `fabro-types::settings::run`. When that
//! module moves into `fabro-workflow` the remaining helpers will follow.

use super::run::{MergeStrategy as V2MergeStrategy, RunArtifactsLayer, RunPullRequestLayer};
use crate::settings::run::{
    ArtifactsSettings, MergeStrategy as OldMergeStrategy, PullRequestSettings,
};

pub fn bridge_merge_strategy(m: V2MergeStrategy) -> OldMergeStrategy {
    match m {
        V2MergeStrategy::Squash => OldMergeStrategy::Squash,
        V2MergeStrategy::Merge => OldMergeStrategy::Merge,
        V2MergeStrategy::Rebase => OldMergeStrategy::Rebase,
    }
}

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

pub fn bridge_run_artifacts(artifacts: &RunArtifactsLayer) -> ArtifactsSettings {
    ArtifactsSettings {
        include: artifacts.include.clone(),
    }
}
