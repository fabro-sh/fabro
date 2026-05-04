use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

/// Reasoning effort level for models that support native effort control.
///
/// Values are code-owned; adding a new level is a Rust change.
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
    Display,
    EnumString,
    IntoStaticStr,
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

    use super::*;

    #[test]
    fn from_str_round_trip() {
        assert_eq!(ReasoningEffort::from_str("low"), Ok(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::from_str("medium"),
            Ok(ReasoningEffort::Medium)
        );
        assert_eq!(ReasoningEffort::from_str("high"), Ok(ReasoningEffort::High));
        assert_eq!(
            ReasoningEffort::from_str("xhigh"),
            Ok(ReasoningEffort::XHigh)
        );
        assert_eq!(ReasoningEffort::from_str("max"), Ok(ReasoningEffort::Max));
        assert_eq!(ReasoningEffort::XHigh.to_string(), "xhigh");
        assert_eq!(<&'static str>::from(ReasoningEffort::XHigh), "xhigh");
        assert_eq!(ReasoningEffort::Max.to_string(), "max");
        assert_eq!(<&'static str>::from(ReasoningEffort::Max), "max");
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!(ReasoningEffort::from_str("bogus").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let effort = ReasoningEffort::High;
        let json = serde_json::to_string(&effort).unwrap();
        assert_eq!(json, "\"high\"");
        let parsed: ReasoningEffort = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, effort);
    }

    #[test]
    fn ord_ordering() {
        assert!(ReasoningEffort::Low < ReasoningEffort::Medium);
        assert!(ReasoningEffort::Medium < ReasoningEffort::High);
        assert!(ReasoningEffort::High < ReasoningEffort::XHigh);
        assert!(ReasoningEffort::XHigh < ReasoningEffort::Max);
    }
}
