//! Adapter metadata vocabulary shared by the model catalog and LLM factories.
//!
//! Adapters are Rust-owned: each registered adapter key maps to a static
//! [`AdapterMetadata`] describing how the adapter dispatches agent profiles,
//! formats API key headers, and which native control values it supports.
//!
//! Provider/model catalog rows reference adapters by key. Both the catalog
//! (in `fabro-model`) and the LLM factory registry (in `fabro-llm`) must agree
//! on the same set of adapter keys; the parity is enforced by tests.

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::Speed;
use crate::reasoning::ReasoningEffort;

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
}

impl AdapterKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    #[must_use]
    pub fn metadata(self) -> &'static AdapterMetadata {
        match self {
            Self::Anthropic => &ANTHROPIC,
            Self::OpenAi => &OPENAI,
            Self::Gemini => &GEMINI,
            Self::OpenAiCompatible => &OPENAI_COMPATIBLE,
        }
    }
}

impl AsRef<str> for AdapterKind {
    fn as_ref(&self) -> &str {
        (*self).as_str()
    }
}

/// Internal dispatch key that `fabro-agent` maps to a concrete agent profile.
///
/// This is **not** a settings field. The agent profile is inferred from the
/// adapter, never set directly in TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentProfileKind {
    Anthropic,
    OpenAi,
    Gemini,
}

/// How an API key for the adapter is converted into an HTTP authentication
/// header.
///
/// Carries no secret values — the actual key is supplied at request time by
/// `fabro-auth::build_api_key_header(policy, key)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyHeaderPolicy {
    /// Standard `Authorization: Bearer <key>` header.
    Bearer,
    /// Custom header name carrying the raw key as its value, e.g. Anthropic's
    /// `x-api-key`.
    Custom { name: &'static str },
}

/// Native control values an adapter knows how to send through its provider
/// API.
#[derive(Debug, Clone, Copy)]
pub struct AdapterControlCapabilities {
    /// Reasoning-effort values this adapter can accept for models declaring
    /// `features.reasoning_effort = "levels"`. The adapter owns how those
    /// levels are encoded on the provider wire API.
    pub native_reasoning_effort: &'static [ReasoningEffort],
    /// Additional speeds (beyond `Speed::Standard`, which is implicit) the
    /// adapter supports. Models may declare `controls.speed` only as a
    /// subset of this list.
    pub additional_speeds:       &'static [Speed],
}

/// Static metadata for a single adapter implementation.
#[derive(Debug, Clone, Copy)]
pub struct AdapterMetadata {
    /// Typed stable adapter identity.
    pub kind:            AdapterKind,
    /// Stable adapter key referenced from `[llm.providers.<id>] adapter =
    /// "..."`.
    pub key:             &'static str,
    /// Default agent profile dispatched for providers that use this adapter.
    pub default_profile: AgentProfileKind,
    /// How API keys for this adapter are converted into auth headers.
    pub api_key_header:  ApiKeyHeaderPolicy,
    /// Native control values the adapter can transmit.
    pub controls:        AdapterControlCapabilities,
}

/// Every reasoning-effort variant. Re-exposed as a const slice so static
/// adapter metadata can reference it without re-listing variants.
const FULL_REASONING_EFFORTS: &[ReasoningEffort] = ReasoningEffort::VARIANTS;

const FAST_SPEEDS: &[Speed] = &[Speed::Fast];

/// Anthropic — `anthropic` adapter.
pub const ANTHROPIC: AdapterMetadata = AdapterMetadata {
    kind:            AdapterKind::Anthropic,
    key:             "anthropic",
    default_profile: AgentProfileKind::Anthropic,
    api_key_header:  ApiKeyHeaderPolicy::Custom { name: "x-api-key" },
    controls:        AdapterControlCapabilities {
        native_reasoning_effort: FULL_REASONING_EFFORTS,
        additional_speeds:       FAST_SPEEDS,
    },
};

/// OpenAI — `openai` adapter.
pub const OPENAI: AdapterMetadata = AdapterMetadata {
    kind:            AdapterKind::OpenAi,
    key:             "openai",
    default_profile: AgentProfileKind::OpenAi,
    api_key_header:  ApiKeyHeaderPolicy::Bearer,
    controls:        AdapterControlCapabilities {
        native_reasoning_effort: FULL_REASONING_EFFORTS,
        additional_speeds:       &[],
    },
};

/// Google Gemini — `gemini` adapter.
pub const GEMINI: AdapterMetadata = AdapterMetadata {
    kind:            AdapterKind::Gemini,
    key:             "gemini",
    default_profile: AgentProfileKind::Gemini,
    api_key_header:  ApiKeyHeaderPolicy::Custom {
        name: "x-goog-api-key",
    },
    controls:        AdapterControlCapabilities {
        native_reasoning_effort: FULL_REASONING_EFFORTS,
        additional_speeds:       &[],
    },
};

/// OpenAI-compatible — `openai_compatible` adapter, used by Kimi/Zai/etc.
/// Routes through the OpenAI agent profile but accepts arbitrary `base_url`
/// per provider settings.
pub const OPENAI_COMPATIBLE: AdapterMetadata = AdapterMetadata {
    kind:            AdapterKind::OpenAiCompatible,
    key:             "openai_compatible",
    default_profile: AgentProfileKind::OpenAi,
    api_key_header:  ApiKeyHeaderPolicy::Bearer,
    controls:        AdapterControlCapabilities {
        // `openai_compatible` providers vary widely; the catalog requires
        // models declaring `features.reasoning_effort = "levels"` to
        // enumerate exactly which effort values their endpoint accepts.
        native_reasoning_effort: FULL_REASONING_EFFORTS,
        additional_speeds:       &[],
    },
};

/// All built-in adapter metadata, in stable iteration order.
pub const ALL_ADAPTERS: &[AdapterMetadata] = &[ANTHROPIC, OPENAI, GEMINI, OPENAI_COMPATIBLE];

/// Look up adapter metadata by stable key.
#[must_use]
pub fn get(key: impl AsRef<str>) -> Option<&'static AdapterMetadata> {
    let key = key.as_ref();
    ALL_ADAPTERS.iter().find(|a| a.key == key)
}

/// Iterate every registered adapter key.
pub fn keys() -> impl Iterator<Item = &'static str> {
    ALL_ADAPTERS.iter().map(|a| a.key)
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
    fn lookup_by_known_key() {
        assert_eq!(get("anthropic").unwrap().key, "anthropic");
        assert_eq!(get("openai").unwrap().key, "openai");
        assert_eq!(get("gemini").unwrap().key, "gemini");
        assert_eq!(get("openai_compatible").unwrap().key, "openai_compatible");
    }

    #[test]
    fn lookup_unknown_key_returns_none() {
        assert!(get("does_not_exist").is_none());
    }

    #[test]
    fn keys_are_unique_and_match_all_adapters() {
        let keys: Vec<&'static str> = keys().collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate adapter key");
        assert_eq!(sorted.len(), ALL_ADAPTERS.len());
    }

    #[test]
    fn anthropic_uses_custom_x_api_key_header() {
        match ANTHROPIC.api_key_header {
            ApiKeyHeaderPolicy::Custom { name } => assert_eq!(name, "x-api-key"),
            ApiKeyHeaderPolicy::Bearer => panic!("expected custom header for anthropic"),
        }
    }

    #[test]
    fn openai_uses_bearer_header() {
        assert!(matches!(OPENAI.api_key_header, ApiKeyHeaderPolicy::Bearer));
    }

    #[test]
    fn anthropic_supports_fast_speed() {
        assert!(ANTHROPIC.controls.additional_speeds.contains(&Speed::Fast));
    }

    #[test]
    fn openai_compatible_uses_openai_profile() {
        assert_eq!(OPENAI_COMPATIBLE.default_profile, AgentProfileKind::OpenAi);
    }

    #[test]
    fn every_adapter_supports_full_native_reasoning_effort() {
        for adapter in ALL_ADAPTERS {
            assert_eq!(
                adapter.controls.native_reasoning_effort.len(),
                FULL_REASONING_EFFORTS.len(),
                "adapter {} should expose all reasoning-effort values",
                adapter.key,
            );
        }
    }
}
