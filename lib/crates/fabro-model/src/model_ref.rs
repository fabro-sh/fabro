use std::fmt;
use std::sync::Arc;

use crate::provider_id::ProviderId;
use crate::types::Model;

/// A reference to a model — either a fully resolved `Model` or a
/// provider + model-name pair that hasn't been looked up yet.
#[derive(Clone)]
pub enum ModelHandle {
    /// A model whose metadata has been resolved from the catalog.
    Resolved(Arc<Model>),
    /// An unresolved provider:model pair (e.g. from CLI input or config).
    ByName {
        provider: ProviderId,
        model:    String,
    },
}

impl ModelHandle {
    /// The model identifier string (e.g. `"claude-opus-4-6"`).
    #[must_use]
    pub fn model_id(&self) -> &str {
        match self {
            Self::Resolved(m) => &m.id,
            Self::ByName { model, .. } => model,
        }
    }

    /// The provider for this model.
    #[must_use]
    pub fn provider(&self) -> &ProviderId {
        match self {
            Self::Resolved(m) => &m.provider,
            Self::ByName { provider, .. } => provider,
        }
    }
}

impl fmt::Display for ModelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider(), self.model_id())
    }
}

impl fmt::Debug for ModelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolved(m) => write!(f, "ModelRef::Resolved({:?})", m.id),
            Self::ByName { provider, model } => f
                .debug_struct("ModelRef::ByName")
                .field("provider", provider)
                .field("model", model)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn by_name_display() {
        let r = ModelHandle::ByName {
            provider: ProviderId::from("anthropic"),
            model:    "claude-opus-4-6".to_string(),
        };
        assert_eq!(r.to_string(), "anthropic:claude-opus-4-6");
    }

    #[test]
    fn by_name_accessors() {
        let r = ModelHandle::ByName {
            provider: ProviderId::from("openai"),
            model:    "gpt-5.4".to_string(),
        };
        assert_eq!(r.model_id(), "gpt-5.4");
        assert_eq!(r.provider(), "openai");
    }

    #[test]
    fn resolved_display() {
        let info = Catalog::builtin().get("claude-opus-4-6").unwrap().clone();
        let r = ModelHandle::Resolved(Arc::new(info));
        assert_eq!(r.to_string(), "anthropic:claude-opus-4-6");
    }

    #[test]
    fn resolved_accessors() {
        let info = Catalog::builtin().get("gpt-5.4").unwrap().clone();
        let r = ModelHandle::Resolved(Arc::new(info));
        assert_eq!(r.model_id(), "gpt-5.4");
        assert_eq!(r.provider(), "openai");
    }

    #[test]
    fn debug_format() {
        let r = ModelHandle::ByName {
            provider: ProviderId::from("gemini"),
            model:    "gemini-3.1-pro-preview".to_string(),
        };
        let debug = format!("{r:?}");
        assert!(debug.contains("ByName"));
        assert!(debug.contains("gemini"));
    }
}
