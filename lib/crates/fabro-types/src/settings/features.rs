//! Features domain.
//!
//! `[features]` is a reserved cross-cutting namespace for Fabro capability
//! flags only. It has a high admission bar and must not become a junk drawer.

use serde::{Deserialize, Serialize};

/// A structurally resolved `[features]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FeaturesNamespace {
    pub session_sandboxes: bool,
}
