//! Adapter registry keys shared by the model catalog and LLM factories.
//!
//! Provider/model catalog rows parse adapter strings into [`AdapterKind`].
//! Runtime code should carry the typed kind instead of re-matching on strings.

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

/// Stable adapter identity for protocol/client behavior.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AdapterKind {
    Anthropic,
    #[serde(rename = "openai")]
    #[strum(to_string = "openai")]
    OpenAi,
    Gemini,
    #[serde(rename = "openai_compatible")]
    #[strum(to_string = "openai_compatible")]
    OpenAiCompatible,
    Bedrock,
}

impl AdapterKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

impl AsRef<str> for AdapterKind {
    fn as_ref(&self) -> &str {
        (*self).as_str()
    }
}

/// Internal dispatch key that `fabro-agent` maps to a concrete agent profile.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AgentProfileKind {
    Anthropic,
    #[serde(rename = "openai")]
    #[strum(to_string = "openai")]
    OpenAi,
    Gemini,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_kind_round_trips_as_snake_case() {
        for kind in AdapterKind::VARIANTS {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let parsed: AdapterKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, *kind);
            assert_eq!(kind.as_str().parse::<AdapterKind>().unwrap(), *kind);
        }
    }

    #[test]
    fn bedrock_adapter_kind_roundtrips() {
        assert_eq!(AdapterKind::Bedrock.as_str(), "bedrock");
        assert_eq!(
            "bedrock".parse::<AdapterKind>().unwrap(),
            AdapterKind::Bedrock
        );
        assert!(AdapterKind::VARIANTS.contains(&AdapterKind::Bedrock));
    }

    #[test]
    fn agent_profile_kind_round_trips_as_settings_strings() {
        for (kind, expected) in [
            (AgentProfileKind::Anthropic, "anthropic"),
            (AgentProfileKind::OpenAi, "openai"),
            (AgentProfileKind::Gemini, "gemini"),
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let parsed: AgentProfileKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
            assert_eq!(expected.parse::<AgentProfileKind>().unwrap(), kind);
            assert_eq!(kind.to_string(), expected);
        }
    }
}
