use std::fmt;
use std::sync::Arc;

use crate::{AuthStore, Credential, CredentialFallback, ServerTarget};

#[derive(Clone)]
pub struct OAuthSession {
    pub target:     ServerTarget,
    pub auth_store: AuthStore,
    pub fallback:   Option<Arc<dyn CredentialFallback>>,
}

impl OAuthSession {
    #[must_use]
    pub fn new(target: ServerTarget, auth_store: AuthStore) -> Self {
        Self {
            target,
            auth_store,
            fallback: None,
        }
    }

    #[must_use]
    pub fn with_fallback(mut self, fallback: Arc<dyn CredentialFallback>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    #[must_use]
    pub fn resolve_fallback(&self) -> Option<Credential> {
        self.fallback
            .as_ref()
            .and_then(|fallback| fallback.resolve())
    }
}

impl fmt::Debug for OAuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthSession")
            .field("target", &self.target)
            .field("auth_store", &self.auth_store)
            .field(
                "fallback",
                &self.fallback.as_ref().map(|_| "<credential fallback>"),
            )
            .finish()
    }
}
