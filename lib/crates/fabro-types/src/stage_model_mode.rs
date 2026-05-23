//! How a stage produced its `StageModelUsage` value.
//!
//! Stage-prompt events and projections carry a small closed-set tag indicating
//! whether the LLM-related metadata came from an `agent` activation, a one-shot
//! `prompt`, a fan-in reducer, or an external `acp` session. Keeping this as a
//! typed enum (rather than a free-form `String`) keeps producer and consumer
//! crates aligned and gives the OpenAPI schema a proper enum constraint.

use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageModelMode {
    Prompt,
    Agent,
    Acp,
    FanIn,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&StageModelMode::FanIn).unwrap(),
            "\"fan_in\""
        );
        let parsed: StageModelMode = serde_json::from_str("\"acp\"").unwrap();
        assert_eq!(parsed, StageModelMode::Acp);
    }

    #[test]
    fn display_and_from_str_round_trip() {
        for mode in [
            StageModelMode::Prompt,
            StageModelMode::Agent,
            StageModelMode::Acp,
            StageModelMode::FanIn,
        ] {
            let s = mode.to_string();
            assert_eq!(StageModelMode::from_str(&s).unwrap(), mode);
        }
    }
}
