//! CLI domain.
//!
//! `[cli]` is owner-first: the CLI process reads its settings from
//! `~/.fabro/settings.toml` plus process-local overrides. `cli.*` stanzas in
//! `fabro.toml` and `workflow.toml` remain schema-valid but runtime-inert.
//! This file holds only the Stage-1 skeleton; Stage 2 fleshes out the full
//! subtree (target, auth, exec, output, updates, logging).

use serde::{Deserialize, Serialize};

/// A sparse `[cli]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliLayer;
