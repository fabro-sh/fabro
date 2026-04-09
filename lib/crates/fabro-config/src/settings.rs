//! Empty module retained for backwards-compatible imports.
//!
//! The legacy `TryFrom<ConfigLayer> for Settings` impl was replaced by
//! [`crate::ConfigLayer::resolve`], which delegates to the v2 bridge in
//! `fabro_types::settings::v2::bridge`. Stage 6 deletes this file entirely.
