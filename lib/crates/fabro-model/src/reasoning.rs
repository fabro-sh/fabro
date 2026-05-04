//! Shared reasoning-effort enum.
//!
//! `ReasoningEffort` is a Rust-owned vocabulary type. Catalog data, request
//! validation, OpenAPI replacement types, and the LLM client all share one
//! enum so that adding a new effort value remains a Rust change.

use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use strum::VariantArray;

    use super::*;

    #[test]
    fn parses_canonical_lowercase_strings() {
        assert_eq!(ReasoningEffort::from_str("low").unwrap(), ReasoningEffort::Low);
        assert_eq!(
            ReasoningEffort::from_str("medium").unwrap(),
            ReasoningEffort::Medium
        );
        assert_eq!(ReasoningEffort::from_str("high").unwrap(), ReasoningEffort::High);
        assert_eq!(
            ReasoningEffort::from_str("xhigh").unwrap(),
            ReasoningEffort::XHigh
        );
        assert_eq!(ReasoningEffort::from_str("max").unwrap(), ReasoningEffort::Max);
    }

    #[test]
    fn rejects_unknown_strings() {
        assert!(ReasoningEffort::from_str("none").is_err());
        assert!(ReasoningEffort::from_str("").is_err());
        assert!(ReasoningEffort::from_str("HIGH").is_err());
    }

    #[test]
    fn display_matches_serde_lowercase() {
        assert_eq!(ReasoningEffort::XHigh.to_string(), "xhigh");
        assert_eq!(<&'static str>::from(ReasoningEffort::Max), "max");
    }

    #[test]
    fn variants_in_ordered_progression() {
        let v = ReasoningEffort::VARIANTS;
        assert_eq!(v[0], ReasoningEffort::Low);
        assert_eq!(v[v.len() - 1], ReasoningEffort::Max);
    }

    #[test]
    fn round_trip_through_json() {
        let json = serde_json::to_string(&ReasoningEffort::High).unwrap();
        assert_eq!(json, "\"high\"");
        let parsed: ReasoningEffort = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ReasoningEffort::High);
    }
}
