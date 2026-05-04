use crate::billing::Speed;
use crate::reasoning_effort::ReasoningEffort;

/// Identifies the kind of agent profile an adapter's models use.
///
/// This is an internal dispatch key, not a settings field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProfileKind {
    Anthropic,
    OpenAi,
    Gemini,
}

/// How an API key is sent with requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyHeaderPolicy {
    Bearer,
    Custom { name: &'static str },
}

/// Control capabilities declared by an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterControlCapabilities {
    pub native_reasoning_effort: &'static [ReasoningEffort],
    pub additional_speeds:       &'static [Speed],
}

/// Static metadata for a provider adapter.
///
/// This is Rust-owned code, not settings data. It describes behavioral
/// contracts that adapters implement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterMetadata {
    pub key:             &'static str,
    pub default_profile: AgentProfileKind,
    pub api_key_header:  ApiKeyHeaderPolicy,
    pub controls:        AdapterControlCapabilities,
}

/// Built-in adapter metadata registry.
///
/// New adapters require Rust code; new providers using existing adapters
/// only require settings data.
pub fn builtin_adapter_metadata() -> &'static [AdapterMetadata] {
    static METADATA: &[AdapterMetadata] = &[
        AdapterMetadata {
            key:             "anthropic",
            default_profile: AgentProfileKind::Anthropic,
            api_key_header:  ApiKeyHeaderPolicy::Custom { name: "x-api-key" },
            controls:        AdapterControlCapabilities {
                native_reasoning_effort: &[
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ],
                additional_speeds:       &[Speed::Fast],
            },
        },
        AdapterMetadata {
            key:             "openai",
            default_profile: AgentProfileKind::OpenAi,
            api_key_header:  ApiKeyHeaderPolicy::Bearer,
            controls:        AdapterControlCapabilities {
                native_reasoning_effort: &[
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ],
                additional_speeds:       &[],
            },
        },
        AdapterMetadata {
            key:             "gemini",
            default_profile: AgentProfileKind::Gemini,
            api_key_header:  ApiKeyHeaderPolicy::Bearer,
            controls:        AdapterControlCapabilities {
                native_reasoning_effort: &[
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ],
                additional_speeds:       &[],
            },
        },
        AdapterMetadata {
            key:             "openai_compatible",
            default_profile: AgentProfileKind::OpenAi,
            api_key_header:  ApiKeyHeaderPolicy::Bearer,
            controls:        AdapterControlCapabilities {
                native_reasoning_effort: &[],
                additional_speeds:       &[],
            },
        },
    ];
    METADATA
}

/// Look up adapter metadata by key.
#[must_use]
pub fn adapter_metadata(key: &str) -> Option<&'static AdapterMetadata> {
    builtin_adapter_metadata().iter().find(|m| m.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_metadata_has_four_adapters() {
        assert_eq!(builtin_adapter_metadata().len(), 4);
    }

    #[test]
    fn lookup_by_key() {
        let anthropic = adapter_metadata("anthropic").unwrap();
        assert_eq!(anthropic.default_profile, AgentProfileKind::Anthropic);
        assert_eq!(anthropic.api_key_header, ApiKeyHeaderPolicy::Custom {
            name: "x-api-key",
        });
    }

    #[test]
    fn lookup_unknown_key() {
        assert!(adapter_metadata("unknown").is_none());
    }

    #[test]
    fn openai_compatible_has_empty_controls() {
        let compat = adapter_metadata("openai_compatible").unwrap();
        assert!(compat.controls.native_reasoning_effort.is_empty());
        assert!(compat.controls.additional_speeds.is_empty());
    }
}
