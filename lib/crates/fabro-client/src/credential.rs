use std::fmt;

use crate::AuthEntry;

#[derive(Clone)]
pub enum Credential {
    DevToken(String),
    Worker(String),
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
            Self::DevToken(token) | Self::Worker(token) => token,
            Self::OAuth(entry) => &entry.access_token,
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DevToken(_) => f.write_str("Credential::DevToken(<redacted>)"),
            Self::Worker(_) => f.write_str("Credential::Worker(<redacted>)"),
            Self::OAuth(_) => f.write_str("Credential::OAuth(<redacted>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use static_assertions::assert_not_impl_any;

    use super::Credential;

    assert_not_impl_any!(Credential: std::fmt::Display);

    #[test]
    fn worker_bearer_token_returns_inner_token() {
        let credential = Credential::Worker("worker-token".to_string());
        assert_eq!(credential.bearer_token(), "worker-token");
    }

    #[test]
    fn worker_debug_redacts_token() {
        let credential = Credential::Worker("worker-token".to_string());
        assert_eq!(format!("{credential:?}"), "Credential::Worker(<redacted>)");
    }
}
