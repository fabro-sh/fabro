//! Petri's `SecretProvider` over Fabro's vault.
//!
//! A run's commands reach a secret as a `{"$secret": "NAME"}` reference
//! that Petri resolves at spawn, straight into the child's environment; the
//! value never enters a record. Resolving a secret registers it with the
//! run's masker, and every record and log line is masked before it is
//! appended, so a value that was resolved cannot appear in `petri_records`.
//! This provider is what makes the vault the place those names resolve
//! from, in the worker (over the vault snapshot the worker loads from the
//! server storage) and in the server process under its test override.
//!
//! Only `Token` entries resolve, as the legacy runner resolves
//! `{{ secrets.NAME }}` (`fabro_auth::vault_get_token`): an OAuth record
//! or a file-shaped secret is not a value a command's environment should
//! carry, so such a name is unknown here.
//!
//! [`SecretProvider::register`] is served: a human gate's sensitive answer
//! is registered under `answer:<question id>` before its reference is
//! delivered, and lives as long as the provider, which is the run.

use std::sync::Arc;

use fabro_types::SecretType;
use fabro_vault::Vault;
use petri_runtime::executor::{MapSecrets, Masker, Secret, SecretError, SecretProvider};

/// The vault's token entries, as Petri's secret provider for one run.
pub struct VaultSecrets {
    inner: MapSecrets,
}

impl VaultSecrets {
    /// A provider over the vault's `Token` entries as they are now: the
    /// worker holds a snapshot, so a later change to the vault is not seen
    /// by a running run, as with the legacy runner.
    #[must_use]
    pub fn from_vault(vault: &Vault) -> Self {
        let pairs = vault
            .entries()
            .iter()
            .filter(|(_, entry)| entry.secret_type == SecretType::Token)
            .map(|(name, entry)| (name.as_str(), entry.value.as_str()))
            .collect::<Vec<_>>();
        Self::from_pairs(&pairs)
    }

    /// A provider over the given names and values.
    #[must_use]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            inner: MapSecrets::from_pairs(pairs),
        }
    }
}

impl SecretProvider for VaultSecrets {
    fn resolve(&self, name: &str) -> Result<Secret, SecretError> {
        self.inner.resolve(name)
    }

    fn register(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.inner.register(name, value)
    }

    fn masker(&self) -> Masker {
        self.inner.masker()
    }
}

/// A shared provider, installed on a runtime that takes its provider by
/// value: the run's engine assembly holds the provider as a trait object
/// so a caller can hand in any implementation.
pub struct SharedSecrets(pub Arc<dyn SecretProvider>);

impl SecretProvider for SharedSecrets {
    fn resolve(&self, name: &str) -> Result<Secret, SecretError> {
        self.0.resolve(name)
    }

    fn register(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.0.register(name, value)
    }

    fn masker(&self) -> Masker {
        self.0.masker()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn vault() -> Vault {
        let mut vault = Vault::from_entries(HashMap::new());
        vault
            .set("TOKEN", "hunter2-hunter2", SecretType::Token, None)
            .expect("a detached vault takes an entry");
        vault
            .set(
                "OAUTH",
                r#"{"access_token":"oauth-secret-value"}"#,
                SecretType::Oauth,
                None,
            )
            .expect("a detached vault takes an entry");
        vault
    }

    #[test]
    fn a_token_entry_resolves_and_is_masked_afterwards() {
        let secrets = VaultSecrets::from_vault(&vault());
        let masker = secrets.masker();
        assert!(
            !masker.contains_secret("hunter2-hunter2"),
            "nothing resolved yet"
        );
        let secret = secrets.resolve("TOKEN").expect("the token resolves");
        assert_eq!(secret.expose(), "hunter2-hunter2");
        assert_eq!(masker.mask("got hunter2-hunter2"), "got ***");
    }

    #[test]
    fn a_non_token_entry_and_an_unknown_name_are_unknown() {
        let secrets = VaultSecrets::from_vault(&vault());
        assert!(matches!(
            secrets.resolve("OAUTH"),
            Err(SecretError::Unknown(name)) if name == "OAUTH"
        ));
        assert!(matches!(
            secrets.resolve("MISSING"),
            Err(SecretError::Unknown(name)) if name == "MISSING"
        ));
    }

    #[test]
    fn a_dynamic_secret_registers_once_and_masks_at_once() {
        let secrets = VaultSecrets::from_vault(&vault());
        secrets
            .register("answer:gate#3", "sensitive-answer")
            .expect("a new name registers");
        assert_eq!(
            secrets.masker().mask("said sensitive-answer"),
            "said ***",
            "registration feeds the masker before any resolution"
        );
        assert_eq!(
            secrets
                .resolve("answer:gate#3")
                .expect("registered")
                .expose(),
            "sensitive-answer"
        );
        assert!(matches!(
            secrets.register("TOKEN", "shadow"),
            Err(SecretError::Duplicate(_))
        ));
    }

    #[test]
    fn a_shared_provider_delegates() {
        let shared = SharedSecrets(Arc::new(VaultSecrets::from_vault(&vault())));
        assert_eq!(
            shared.resolve("TOKEN").expect("resolves").expose(),
            "hunter2-hunter2"
        );
        assert!(shared.masker().contains_secret("hunter2-hunter2"));
    }
}
