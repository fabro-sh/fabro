use std::fmt;

use crate::AuthEntry;

#[derive(Clone)]
pub enum Credential {
    DevToken(String),
    OAuth(AuthEntry),
}

pub trait CredentialFallback: Send + Sync {
    fn resolve(&self) -> Option<Credential>;
}

impl<F> CredentialFallback for F
where
    F: Fn() -> Option<Credential> + Send + Sync,
{
    fn resolve(&self) -> Option<Credential> {
        self()
    }
}

impl Credential {
    pub fn bearer_token(&self) -> &str {
        match self {
            Self::DevToken(token) => token,
            Self::OAuth(entry) => &entry.access_token,
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DevToken(_) => f.write_str("Credential::DevToken(<redacted>)"),
            Self::OAuth(_) => f.write_str("Credential::OAuth(<redacted>)"),
        }
    }
}
