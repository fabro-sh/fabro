//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains. This file holds only the Stage-1 skeleton; Stage 2 fleshes out
//! the full subtree (listen, api, web, auth, storage, artifacts, slatedb,
//! scheduler, logging, integrations).

use serde::{Deserialize, Serialize};

/// A sparse `[server]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLayer;
