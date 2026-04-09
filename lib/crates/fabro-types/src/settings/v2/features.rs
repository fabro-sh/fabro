//! Features domain.
//!
//! `[features]` is a reserved cross-cutting namespace for Fabro capability
//! flags only. It has a high admission bar and must not become a junk drawer.

use serde::{Deserialize, Serialize};

/// A sparse `[features]` layer as it appears in a single settings file.
///
/// Every field is an `Option<bool>` so layers can independently set or
/// override a flag without forcing a default that hides an unset value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturesLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_sandboxes: Option<bool>,
}
