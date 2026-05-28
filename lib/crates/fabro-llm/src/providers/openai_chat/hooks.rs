//! Extension hooks that let provider-specific adapters (e.g. OpenRouter)
//! inject behavior into the shared OpenAI Chat Completions pipeline without
//! duplicating the wire layer.

use crate::types::{Request, Response};

/// Optional pre-send and post-receive hooks for the chat pipeline.
///
/// Each field is an opt-in `fn` pointer. The default-constructed value
/// ([`Self::NONE`]) is behaviorally identical to no hooks at all, which is
/// what `OpenAiCompatibleAdapter` uses.
#[derive(Clone, Copy)]
pub(crate) struct ChatHooks {
    /// Mutate the final JSON body just before sending. Used by OpenRouter
    /// to translate typed `Request.reasoning_effort` into OR's
    /// `{reasoning: {effort: ...}}` shape (and any other request-shape
    /// translations).
    pub(crate) mutate_request:  Option<fn(&mut serde_json::Value, &Request)>,
    /// Read provider-specific fields out of the raw response body and
    /// attach them to the unified [`Response`]. Used by OpenRouter to
    /// extract authoritative `usage.cost`.
    pub(crate) enrich_response: Option<fn(&mut Response, &serde_json::Value)>,
}

impl ChatHooks {
    /// No-op hooks. Behaviorally identical to no hooks.
    pub(crate) const NONE: Self = Self {
        mutate_request:  None,
        enrich_response: None,
    };
}

impl Default for ChatHooks {
    fn default() -> Self {
        Self::NONE
    }
}
