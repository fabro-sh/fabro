# Vault and Credential Source Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Execution status:** Completed on 2026-05-18 on local `main` as requested. No worktree was created and no commits were made during implementation. The optional live provider auth spot-check was not run.

**Goal:** Replace the conflated `credential:`/`env:` credential reference model with explicit `vault:`/`env:` prefixes, and store API-key secrets as raw tokens (no JSON envelope) while keeping OAuth credentials as a typed JSON schema in the vault.

**Architecture:** The vault keeps three secret schemas: `token` (opaque token value), `oauth` (JSON `OAuthCredential`), and `file` (path-shaped). Provider catalog credential refs are explicit about source: `env:NAME` reads the process env only, `vault:NAME` reads the vault only. The credential resolver branches on the vault entry's schema to build the right `ApiCredential` (token → API-key header; oauth → refreshable bearer token); OpenAI/Codex-specific headers and base URLs live in resolver/catalog policy, not in the vault schema. The old `AuthCredential` enum, `credential_id_for`, `parse_credential_secret`, `Vault::snapshot()`, and the `credential:` prefix all go away.

**Tech Stack:** Rust workspace (`cargo nextest`), `insta` snapshots, OpenAPI + `progenitor` for `fabro-api`, Bun + `openapi-generator` for the TS client.

**Scope notes:**
- Greenfield app, no backwards compat. Rip-and-replace, do not parallel-implement.
- No higher-level alias prefix; `env:` and `vault:` are the only two.
- Keep `SecretType::File`. Possible future schemas such as `Cookie`, `PemCertificate`, and `SshKey` are out of scope.
- The order in `credentials = [...]` is the lookup order. Conventionally `env:NAME` precedes `vault:NAME`.
- Schema mismatch on a `vault:` reference (e.g. asking for an API key, finding a `file` entry) is a hard error surfaced from the resolver.
- Do not implicitly project vault token secrets into spawned process env. If workflow command env needs vault-backed values later, add an explicit mapping feature instead of a bulk export API.

---

### Task 1: Create the feature branch and baseline

**Files:**
- (none)

- [ ] **Step 1: Confirm clean working state on `fix/provider-auth-headers`**

Run: `git status`
Expected: existing modified files are all unrelated to vault/credential refactor (changelog, docs, provider catalog `.toml` files for the live-provider-cache fix, etc.). If any of those are partial work for *this* refactor, stop and resolve first.

- [ ] **Step 2: Create the working branch**

Run: `git switch -c refactor/vault-credential-schema`
Expected: switches off the parent branch with the existing modifications still in the working tree (cleanest base).

- [ ] **Step 3: Sanity-check the baseline build**

Run: `cargo build --workspace`
Expected: clean build (or only warnings).

- [ ] **Step 4: Sanity-check the baseline tests**

Run: `cargo nextest run --workspace`
Expected: all tests green. Note any pre-existing failures so you can distinguish them later.

---

### Task 2: Rename `SecretType` variants in `fabro-types`

**Files:**
- Modify: `lib/crates/fabro-types/src/secret.rs`

This is the keystone rename. All compile errors that follow will be tracked down in later tasks; this task is just the type definition change.

- [ ] **Step 1: Replace the `SecretType` enum**

Edit `lib/crates/fabro-types/src/secret.rs` to:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SecretType {
    /// Opaque API-key/PAT-style token value.
    #[default]
    Token,
    /// JSON-encoded `OAuthCredential`. Refreshable; never projected into env.
    Oauth,
    /// Path-shaped secret materialized to the filesystem.
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub name:        String,
    #[serde(rename = "type")]
    pub secret_type: SecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}
```

Note: `Oauth` (not `OAuth`) so `serialize_all = "snake_case"` produces `oauth`. Confirm with `cargo expand` or a unit test if unsure.

- [ ] **Step 2: Add a serialization round-trip test**

Append to `lib/crates/fabro-types/src/secret.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_type_serializes_to_snake_case() {
        assert_eq!(serde_json::to_string(&SecretType::Token).unwrap(), "\"token\"");
        assert_eq!(serde_json::to_string(&SecretType::Oauth).unwrap(), "\"oauth\"");
        assert_eq!(serde_json::to_string(&SecretType::File).unwrap(), "\"file\"");
    }

    #[test]
    fn secret_type_default_is_token() {
        assert_eq!(SecretType::default(), SecretType::Token);
    }
}
```

- [ ] **Step 3: Run the new tests in isolation**

Run: `cargo nextest run -p fabro-types`
Expected: passes (this crate has no callers of the old variants).

- [ ] **Step 4: Commit**

```bash
git add lib/crates/fabro-types/src/secret.rs
git commit -m "$(cat <<'EOF'
refactor(types): rename SecretType variants to schemas

Environment→Token, Credential→Oauth. Schemas now describe the
shape of the stored secret rather than how it is consumed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

The workspace will not compile after this commit. Subsequent tasks bring it back to green.

---

### Task 3: Update `fabro-vault` for the new variants and remove bulk export

**Files:**
- Modify: `lib/crates/fabro-vault/src/lib.rs`

- [ ] **Step 1: Update `validate_name`**

Find `validate_name` (around line 177) and replace the match arms:

```rust
pub fn validate_name(name: &str, secret_type: SecretType) -> Result<(), Error> {
    match secret_type {
        SecretType::Token | SecretType::Oauth => Self::validate_env_name(name),
        SecretType::File => Self::validate_file_name(name),
    }
}
```

- [ ] **Step 2: Delete `snapshot()` and `credential_entries()`**

Remove the `snapshot()` method entirely. It is unused in production and would be a foot gun because it bulk-exports vault secrets as env-shaped key/value pairs.

Remove `credential_entries()` as well. The new resolver uses explicit `vault:NAME` lookups and schema checks; it should not scan all credential-like entries.

Update `file_secrets` to use the new variant: `SecretType::File` (no change to the variant itself, just confirm it still compiles).

- [ ] **Step 3: Update vault tests in `lib/crates/fabro-vault/src/lib.rs`**

In the inline `mod tests`, replace all `SecretType::Environment` → `SecretType::Token` and `SecretType::Credential` → `SecretType::Oauth`. Delete tests for `snapshot()` and `credential_entries()`. Keep or add focused tests that prove `file_secrets()` still returns only `SecretType::File` entries and that token/oauth entries can still be read by name through `get_entry()`.

- [ ] **Step 4: Run vault tests**

Run: `cargo nextest run -p fabro-vault`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-vault/
git commit -m "$(cat <<'EOF'
refactor(vault): rename helpers for new SecretType schemas

Token and Oauth keep env-var-shaped names; File keeps path-shaped names.
Remove snapshot() and credential_entries() so the vault no longer exposes
a bulk secret export/scanning API.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Replace `AuthCredential`/`AuthDetails` with a single `OAuthCredential`

**Files:**
- Modify: `lib/crates/fabro-auth/src/credential.rs` (major rewrite)
- Modify: `lib/crates/fabro-auth/src/lib.rs` (re-export changes)

This is the big type-shape change. We delete the api-key envelope entirely and keep only the OAuth structure.

- [ ] **Step 1: Rewrite `lib/crates/fabro-auth/src/credential.rs`**

Replace the whole file with:

```rust
use chrono::{DateTime, Duration, Utc};
use fabro_redact::redact_string;
use serde::{Deserialize, Serialize};

/// JSON shape stored in the vault when `secret_type == Oauth`.
///
/// There is no `provider` field: provider context comes from the catalog and
/// the auth strategy at resolve time. The current OpenAI Codex device-flow
/// login stores this shape, but the schema itself is generic OAuth. `account_id`
/// is a provider account identifier; today it is populated from OpenAI/ChatGPT
/// claims when available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthCredential {
    pub tokens:     OAuthTokens,
    pub config:     OAuthConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl OAuthCredential {
    #[must_use]
    pub fn needs_refresh(&self) -> bool {
        self.tokens.expires_at <= Utc::now() + Duration::minutes(5)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token:  String,
    pub refresh_token: Option<String>,
    pub expires_at:    DateTime<Utc>,
}

pub(crate) fn expires_at_from_now(expires_in: Option<u64>) -> DateTime<Utc> {
    let seconds = i64::try_from(expires_in.unwrap_or(3600)).unwrap_or(i64::MAX);
    Utc::now() + Duration::seconds(seconds)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthConfig {
    pub auth_url:     String,
    pub token_url:    String,
    pub client_id:    String,
    pub scopes:       Vec<String>,
    pub redirect_uri: Option<String>,
    pub use_pkce:     bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ApiKeyHeader {
    Bearer(String),
    Custom { name: String, value: String },
}

fn redact_for_debug(value: &str) -> String {
    let redacted = redact_string(value);
    if redacted == value && !value.is_empty() {
        "REDACTED".to_string()
    } else {
        redacted
    }
}

impl std::fmt::Debug for ApiKeyHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(value) => f
                .debug_tuple("Bearer")
                .field(&redact_for_debug(value))
                .finish(),
            Self::Custom { name, value } => f
                .debug_struct("Custom")
                .field("name", name)
                .field("value", &redact_for_debug(value))
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(expires_at: DateTime<Utc>) -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token:  "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at,
            },
            config:     OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
                use_pkce:     true,
            },
            account_id: Some("acct_123".to_string()),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let credential = fixture(Utc::now() + Duration::hours(1));
        let json = serde_json::to_string(&credential).unwrap();
        let parsed: OAuthCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, credential);
    }

    #[test]
    fn needs_refresh_uses_five_minute_buffer() {
        assert!(fixture(Utc::now() + Duration::minutes(4)).needs_refresh());
        assert!(!fixture(Utc::now() + Duration::minutes(6)).needs_refresh());
    }

    #[test]
    fn api_key_header_debug_redacts_secret_values() {
        let header = ApiKeyHeader::Bearer("sk-test".to_string());
        let debug = format!("{header:?}");
        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("REDACTED"));
    }
}
```

- [ ] **Step 2: Update `lib/crates/fabro-auth/src/lib.rs` re-exports**

Replace the `pub use credential::{...}` block with:

```rust
pub use credential::{
    ApiKeyHeader, OAuthCredential, OAuthConfig, OAuthTokens,
};
```

Drop the `AuthCredential`, `AuthDetails`, `credential_id_for`, and `parse_credential_secret` re-exports — they no longer exist.

- [ ] **Step 3: Verify the crate doesn't compile yet — that's expected**

Run: `cargo build -p fabro-auth 2>&1 | head -40`
Expected: many errors in `resolve.rs`, `vault_ext.rs`, `vault_source.rs`, `strategies/*.rs`, `refresh.rs`. Each one will be fixed in subsequent tasks.

- [ ] **Step 4: Commit the partial state**

```bash
git add lib/crates/fabro-auth/src/credential.rs lib/crates/fabro-auth/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(auth): drop AuthCredential envelope, keep OAuthCredential only

API-key secrets are no longer wrapped; they live in the vault as plain
token values. The OAuth credential keeps its struct form but loses the
`provider` field; provider-specific behavior stays in the resolver/catalog.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Rewrite `vault_ext` for schema-typed lookups

**Files:**
- Modify: `lib/crates/fabro-auth/src/vault_ext.rs` (full rewrite)
- Modify: `lib/crates/fabro-auth/src/lib.rs` (update re-exports)

- [ ] **Step 1: Rewrite `lib/crates/fabro-auth/src/vault_ext.rs`**

Replace the whole file with:

```rust
use fabro_types::SecretMetadata;
use fabro_vault::{Error as VaultError, SecretType, Vault};

use crate::credential::OAuthCredential;

/// Errors raised when a `vault:NAME` lookup finds an entry whose schema does
/// not match what the caller expected.
#[derive(Debug, thiserror::Error)]
pub enum VaultLookupError {
    #[error("vault entry '{name}' has schema {actual:?}, expected {expected:?}")]
    SchemaMismatch {
        name:     String,
        expected: SecretType,
        actual:   SecretType,
    },
    #[error("vault entry '{name}' is not valid {expected:?} JSON: {source}")]
    DecodeFailed {
        name:     String,
        expected: SecretType,
        #[source]
        source:   serde_json::Error,
    },
}

/// Reads the raw token value of a `Token`-typed secret.
///
/// Returns `Ok(None)` if no entry exists. Returns `Err(SchemaMismatch)` if an
/// entry exists with a different schema.
pub fn vault_get_token(vault: &Vault, name: &str) -> Result<Option<String>, VaultLookupError> {
    let Some(entry) = vault.get_entry(name) else {
        return Ok(None);
    };
    if entry.secret_type != SecretType::Token {
        return Err(VaultLookupError::SchemaMismatch {
            name:     name.to_string(),
            expected: SecretType::Token,
            actual:   entry.secret_type,
        });
    }
    Ok(Some(entry.value.clone()))
}

/// Reads and decodes a `Oauth`-typed secret.
pub fn vault_get_oauth(
    vault: &Vault,
    name: &str,
) -> Result<Option<OAuthCredential>, VaultLookupError> {
    let Some(entry) = vault.get_entry(name) else {
        return Ok(None);
    };
    if entry.secret_type != SecretType::Oauth {
        return Err(VaultLookupError::SchemaMismatch {
            name:     name.to_string(),
            expected: SecretType::Oauth,
            actual:   entry.secret_type,
        });
    }
    serde_json::from_str(&entry.value)
        .map(Some)
        .map_err(|source| VaultLookupError::DecodeFailed {
            name: name.to_string(),
            expected: SecretType::Oauth,
            source,
        })
}

pub fn vault_set_token(
    vault: &mut Vault,
    name: &str,
    value: &str,
) -> Result<SecretMetadata, VaultError> {
    vault.set(name, value, SecretType::Token, None)
}

pub fn vault_set_oauth(
    vault: &mut Vault,
    name: &str,
    credential: &OAuthCredential,
) -> Result<SecretMetadata, VaultError> {
    let json = serde_json::to_string(credential)?;
    vault.set(name, &json, SecretType::Oauth, None)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;
    use crate::credential::{OAuthConfig, OAuthTokens};

    fn temp_vault() -> Vault {
        let dir = tempfile::tempdir().unwrap();
        Vault::open(dir.path()).unwrap()
    }

    fn fixture() -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token: "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at: Utc::now() + Duration::hours(1),
            },
            config:     OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: None,
                use_pkce:     true,
            },
            account_id: None,
        }
    }

    #[test]
    fn vault_get_token_returns_none_when_absent() {
        let vault = temp_vault();
        assert!(vault_get_token(&vault, "ANTHROPIC_API_KEY").unwrap().is_none());
    }

    #[test]
    fn vault_get_token_returns_value_when_present() {
        let mut vault = temp_vault();
        vault_set_token(&mut vault, "ANTHROPIC_API_KEY", "sk-test").unwrap();
        assert_eq!(
            vault_get_token(&vault, "ANTHROPIC_API_KEY").unwrap().as_deref(),
            Some("sk-test"),
        );
    }

    #[test]
    fn vault_get_token_errors_on_oauth_entry() {
        let mut vault = temp_vault();
        vault_set_oauth(&mut vault, "OPENAI_CODEX", &fixture()).unwrap();
        let err = vault_get_token(&vault, "OPENAI_CODEX").unwrap_err();
        assert!(matches!(err, VaultLookupError::SchemaMismatch { .. }));
    }

    #[test]
    fn vault_get_oauth_round_trips() {
        let mut vault = temp_vault();
        let credential = fixture();
        vault_set_oauth(&mut vault, "OPENAI_CODEX", &credential).unwrap();
        assert_eq!(
            vault_get_oauth(&vault, "OPENAI_CODEX").unwrap().unwrap(),
            credential,
        );
    }
}
```

- [ ] **Step 2: Update `lib/crates/fabro-auth/src/lib.rs` re-exports**

Replace the `pub use vault_ext::{...}` line with:

```rust
pub use vault_ext::{
    VaultLookupError, vault_get_oauth, vault_get_token, vault_set_oauth,
    vault_set_token,
};
```

Drop the old `vault_credentials_for_provider`, `vault_get_credential`, `vault_set_credential` exports — they are gone.

- [ ] **Step 3: Commit**

```bash
git add lib/crates/fabro-auth/src/vault_ext.rs lib/crates/fabro-auth/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(auth): split vault credential helpers by schema

vault_get_token and vault_get_oauth replace the polymorphic
vault_get_credential, with explicit SchemaMismatch errors when an
entry's stored type does not match what the caller asked for.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

The crate still won't compile — `resolve.rs`, `vault_source.rs`, `strategies/*.rs`, `refresh.rs`, and `strategy.rs` still reference the old types. That's the next task.

---

### Task 6: Refactor `AuthStrategy` and the two strategy implementations

**Files:**
- Modify: `lib/crates/fabro-auth/src/strategy.rs`
- Modify: `lib/crates/fabro-auth/src/strategies/api_key.rs`
- Modify: `lib/crates/fabro-auth/src/strategies/codex_device.rs`
- Modify: `lib/crates/fabro-auth/src/refresh.rs`
- Modify: `lib/crates/fabro-auth/src/lib.rs` (export `LoginResult`)

- [ ] **Step 1: Introduce `LoginResult` in `strategy.rs`**

Replace `lib/crates/fabro-auth/src/strategy.rs` with:

```rust
use async_trait::async_trait;
use fabro_model::ProviderId;

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::credential::{OAuthCredential, OAuthConfig};

/// What a successful login produces. The login flow inspects this to decide
/// which vault schema to persist into.
#[derive(Debug, Clone)]
pub enum LoginResult {
    /// Plain API-key token (will be stored as `SecretType::Token`).
    ApiKey {
        provider: ProviderId,
        key:      String,
    },
    /// OAuth credential (will be stored as `SecretType::Oauth`).
    OAuth {
        provider:   ProviderId,
        credential: OAuthCredential,
    },
}

#[async_trait]
pub trait AuthStrategy: Send {
    async fn init(&mut self) -> anyhow::Result<AuthContextRequest>;
    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<LoginResult>;
}

#[allow(dead_code)]
fn _config_marker(_: &OAuthConfig) {}
```

(The `_config_marker` line is just to silence the unused-import lint if `OAuthConfig` re-export remains needed; remove it once the build settles if clippy is happy.)

- [ ] **Step 2: Update `strategies/api_key.rs` `complete()` to return `LoginResult::ApiKey`**

In `lib/crates/fabro-auth/src/strategies/api_key.rs`:

- Remove `use crate::credential::{AuthCredential, AuthDetails};`
- Add `use crate::strategy::{AuthStrategy, LoginResult};`
- Change the `complete` return type to `anyhow::Result<LoginResult>` and the body to:

```rust
async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<LoginResult> {
    match response {
        AuthContextResponse::ApiKey { key } => Ok(LoginResult::ApiKey {
            provider: self.provider_id.clone(),
            key,
        }),
        AuthContextResponse::DeviceCodeConfirmed => {
            Err(anyhow::anyhow!("expected API key response"))
        }
    }
}
```

- Also fix the `CredentialRef::Credential(_) => None,` arm: it becomes `CredentialRef::Vault(_) => None,` once Task 8 lands. Leave it for now — this file will compile-error until then.

- [ ] **Step 3: Update `strategies/codex_device.rs` to return `LoginResult::OAuth`**

In `lib/crates/fabro-auth/src/strategies/codex_device.rs`:

- Replace any `AuthCredential { provider, details: AuthDetails::CodexOAuth { tokens, config, account_id } }` construction with `LoginResult::OAuth { provider, credential: OAuthCredential { tokens, config, account_id } }`.
- Update the `complete` return type to `anyhow::Result<LoginResult>`.
- Remove imports of `AuthCredential`/`AuthDetails`, add `OAuthCredential` and `LoginResult`.

(Read the file before editing — the device-code flow has a refresh-on-completion path that may also need adjustment.)

- [ ] **Step 4: Update `refresh.rs` to operate on `OAuthCredential`**

In `lib/crates/fabro-auth/src/refresh.rs`, change the public signature from `refresh(credential: &AuthCredential, ...) -> Result<AuthCredential, _>` to:

```rust
pub async fn refresh(
    credential: &OAuthCredential,
    http: &reqwest::Client,
) -> Result<OAuthCredential, RefreshError>
```

Drop the `match &credential.details` — there is only the OAuth case now. The body that built a new `AuthCredential` should build a `OAuthCredential` directly. Keep `expires_at_from_now` import.

- [ ] **Step 5: Re-export `LoginResult` from `lib.rs`**

In `lib/crates/fabro-auth/src/lib.rs`, add `pub use strategy::{AuthStrategy, LoginResult};` (replace the existing `pub use strategy::AuthStrategy;` line).

- [ ] **Step 6: Don't commit yet**

The crate still doesn't compile — `resolve.rs` and `vault_source.rs` haven't been updated. We commit after the next task.

---

### Task 7: Refactor `resolve.rs` and `vault_source.rs` (and `env_source.rs`)

**Files:**
- Modify: `lib/crates/fabro-auth/src/resolve.rs` (significant changes)
- Modify: `lib/crates/fabro-auth/src/vault_source.rs`
- Modify: `lib/crates/fabro-auth/src/env_source.rs`

This is the resolver core: `vault:NAME` reads the vault by schema; `env:NAME` reads only the process env.

- [ ] **Step 1: Update `env_source.rs` — `env:` is now process-env-only**

In `lib/crates/fabro-auth/src/env_source.rs`:

- The existing logic that does `let CredentialRef::Env(name) = credential_ref else { return None; }` and then `self.lookup(name)` is already correct — it reads only from the env lookup, not the vault. No behavior change needed here.
- Update `CredentialRef::Credential(_)` arms in any iteration to `CredentialRef::Vault(_)` (the rename lands in Task 8; leave a `// FIXME(vault-rename)` for now if needed and revisit).
- Remove the doc string that says "facade for provider API-key process-env" if it's misleading; replace with "Resolves `env:NAME` references from the process environment only."

- [ ] **Step 2: Rewrite `credential_from_ref` and related helpers in `resolve.rs`**

In `lib/crates/fabro-auth/src/resolve.rs`:

- Drop `lookup_env_or_vault`. Inline the two cases:
  - `CredentialRef::Env(name)` → `(self.env_lookup)(name)` only.
  - `CredentialRef::Vault(name)` → inspect `vault.get_entry(name)`, branch by `entry.secret_type`, and error on schema mismatch.
- The resolver no longer returns `Option<AuthCredential>`; it returns `Result<Option<ResolvedSecret>, ResolveError>` where:

```rust
pub(crate) enum ResolvedSecret {
    ApiKey(String),
    OAuth(OAuthCredential),
}
```

- For each `CredentialRef` in `credentials = [...]`, try in order:
  - `Env(name)` → if `Some(value)`, return `ResolvedSecret::ApiKey(value)`; else continue.
  - `Vault(name)` → look up the entry. If absent, continue. If present, branch on `secret_type`:
    - `Token` → `ResolvedSecret::ApiKey(value)`
    - `Oauth` → decode and return `ResolvedSecret::OAuth(credential)`
    - `File` → return `ResolveError::SchemaMismatch` (a `vault:` ref can never resolve to a file secret).
- Update the OAuth refresh path that currently round-trips an `AuthCredential`: read `vault_get_oauth`, call `refresh::refresh(&credential, http)`, write back via `vault_set_oauth`. The vault name comes from the `CredentialRef::Vault(name)` that resolved to the OAuth secret in the first place; thread that name through.
- Replace every `AuthDetails::ApiKey { key }` / `AuthDetails::CodexOAuth { .. }` match with the new `ResolvedSecret` shape.

- [ ] **Step 3: Update the `ApiCredential`-build paths**

The functions that build an `ApiCredential` from an old `AuthCredential` (look near `build_api_key_header`, line ~335 and ~407) should now take `ResolvedSecret` instead. The branching is the same: api-key → `build_api_key_header(policy, key)`; oauth → bearer token. The OpenAI Codex-mode auto-configuration block (sets `base_url`, `codex_mode`, `originator`, `ChatGPT-Account-Id`) should fire only when the provider is OpenAI and the resolved secret is `ResolvedSecret::OAuth`.

- [ ] **Step 4: Update `vault_source.rs`**

In `lib/crates/fabro-auth/src/vault_source.rs`:

- Update the test helpers: `api_key_credential` becomes a function that returns a `String` (or you inline it). `expired_openai_credential` returns `OAuthCredential`.
- Replace `vault.set(name, &serde_json::to_string(&api_key_credential).unwrap(), SecretType::Credential, None)` with `vault_set_token(&mut vault, name, &api_key)`.
- Replace `vault.set(name, &serde_json::to_string(&oauth_credential).unwrap(), SecretType::Credential, None)` with `vault_set_oauth(&mut vault, name, &oauth_credential)`.
- Update assertions to check `ResolvedSecret` variants.

- [ ] **Step 5: Update `resolve.rs` tests**

`lib/crates/fabro-auth/src/resolve.rs` has ~600 lines of tests using `AuthCredential`/`vault_set_credential`. Rewrite each test to use `vault_set_token` / `vault_set_oauth` and the new `CredentialRef::Vault(_)` variant. Notable tests to keep working:

- `with_env_lookup_overrides_vault_settings` — semantics change: env now wins because `env:` is listed first in the test's catalog. Verify the test asserts the right precedence rule.
- The refresh-on-resolve test (around line 1062) that uses `vault_get_credential` — switch to `vault_get_oauth`.

Delete tests that exercised the env-falls-back-to-vault behavior or the `credential_id_for` rule — they're testing removed code.

- [ ] **Step 6: Run the auth crate tests**

Run: `cargo nextest run -p fabro-auth`
Expected: green. If a test you deleted was load-bearing for a particular bug, add a replacement that exercises the new behavior (schema mismatch error, etc.).

- [ ] **Step 7: Commit**

```bash
git add lib/crates/fabro-auth/
git commit -m "$(cat <<'EOF'
refactor(auth): resolve credentials via explicit env vs vault sources

env:NAME reads only the process environment; vault:NAME reads only the
vault and branches on the entry's schema (Token → ApiKey, Oauth →
OAuth bearer). File entries cannot satisfy a credential ref. Removes
the old env-or-vault fallback that conflated the two sources.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Rename `CredentialRef::Credential` → `CredentialRef::Vault`, change prefix

**Files:**
- Modify: `lib/crates/fabro-model/src/catalog.rs`

- [ ] **Step 1: Update the enum and its parse impl**

In `lib/crates/fabro-model/src/catalog.rs` around line 154, rewrite:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CredentialRef {
    Vault(String),
    Env(String),
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vault(name) => write!(f, "vault:{name}"),
            Self::Env(name) => write!(f, "env:{name}"),
        }
    }
}

impl FromStr for CredentialRef {
    type Err = CredentialRefParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(name) = value.strip_prefix("vault:") {
            if name.is_empty() {
                return Err(CredentialRefParseError::EmptyVault);
            }
            return Ok(Self::Vault(name.to_string()));
        }
        if let Some(name) = value.strip_prefix("env:") {
            if name.is_empty() {
                return Err(CredentialRefParseError::EmptyEnv);
            }
            return Ok(Self::Env(name.to_string()));
        }
        Err(CredentialRefParseError::Invalid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CredentialRefParseError {
    #[error("credential reference must be `vault:<name>` or `env:<NAME>`")]
    Invalid,
    #[error("credential reference is missing a name after `vault:`")]
    EmptyVault,
    #[error("credential reference is missing a name after `env:`")]
    EmptyEnv,
}
```

- [ ] **Step 2: Update inline tests in `catalog.rs`**

Search the file for `"credential:"`, `CredentialRef::Credential(`, and `EmptyCredential` — update each to the new names. The inline test fixtures around line 2795 (`credentials = ["credential:bearer", "env:BEARER_API_KEY"]`) become `["env:BEARER_API_KEY", "vault:BEARER_API_KEY"]`. Likewise around 2839.

- [ ] **Step 3: Find and update any other `CredentialRef::Credential` matches**

Run: `rg -n 'CredentialRef::Credential\b' lib/`
Expected output sites (each must be updated to `Vault`): `strategies/api_key.rs`, `resolve.rs`, `cli/commands/install.rs`, `workflow/handler/llm/launch_env.rs`, `config/layers/llm.rs`.

For each: rename the variant and (if the surrounding logic was reading from process env as a fallback for a `Credential` ref) align it with the new resolver semantics.

- [ ] **Step 4: Run the workspace build**

Run: `cargo build --workspace`
Expected: green. If TOML parsing tests fail, that's because the provider catalogs still use `credential:` — that's the next task.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
refactor(model): rename CredentialRef::Credential to Vault

Prefix changes from `credential:` to `vault:` to match the resolver's
new semantics (vault-only, never falls back to env).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Update all provider catalog TOMLs

**Files:**
- Modify: `lib/crates/fabro-model/src/catalog/providers/anthropic.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/gemini.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/inception.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/kimi.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/litellm.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/minimax.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/openai.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/venice.toml`
- Modify: `lib/crates/fabro-model/src/catalog/providers/zai.toml`
- (Ollama has no auth — confirm `lib/crates/fabro-model/src/catalog/providers/ollama.toml` and skip if unchanged.)

For each provider, the rule is:
- Reorder to put `env:` first (so process env wins).
- Use the same name on both sides (matches user convention).
- Drop the magic `credential:openai_codex` — replace with `vault:OPENAI_CODEX` and rely on the resolver branching on entry schema.

- [ ] **Step 1: Update Anthropic**

In `anthropic.toml`, change:

```toml
credentials = ["credential:anthropic", "env:ANTHROPIC_API_KEY"]
```

to:

```toml
credentials = ["env:ANTHROPIC_API_KEY", "vault:ANTHROPIC_API_KEY"]
```

- [ ] **Step 2: Update Gemini**

```toml
credentials = ["env:GEMINI_API_KEY", "env:GOOGLE_API_KEY", "vault:GEMINI_API_KEY"]
```

- [ ] **Step 3: Update OpenAI**

```toml
credentials = ["env:OPENAI_API_KEY", "vault:OPENAI_API_KEY", "vault:OPENAI_CODEX"]
```

- [ ] **Step 4: Update Kimi, Venice, Inception, LiteLLM, MiniMax, Zai, Ollama**

Apply the same `["env:NAME", "vault:NAME"]` pattern to each remaining provider TOML.

- [ ] **Step 5: Run catalog tests**

Run: `cargo nextest run -p fabro-model`
Expected: green. If a snapshot fails because the TOML serialized representation changed, run `cargo insta pending-snapshots`, verify the diff is the expected rename, then `cargo insta accept --snapshot <path>`.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-model/src/catalog/providers/
git commit -m "$(cat <<'EOF'
refactor(catalog): update provider credentials to vault: prefix

env:NAME now precedes vault:NAME so process env wins. Names are
aligned across both sources. Drops the magic openai_codex id in
favor of vault:OPENAI_CODEX with schema-driven dispatch.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Update OpenAPI schema and regenerate `fabro-api` types

**Files:**
- Modify: `docs/public/api-reference/fabro-api.yaml` (SecretType enum)
- Affected (auto-regenerated): `lib/crates/fabro-api/`
- Modify: `lib/crates/fabro-api/tests/secret_type_round_trip.rs`
- Modify: `lib/crates/fabro-api/tests/secret_metadata_round_trip.rs`

- [ ] **Step 1: Update the OpenAPI SecretType enum**

In `docs/public/api-reference/fabro-api.yaml` around line 10387, change:

```yaml
SecretType:
  description: The way a secret is consumed by the sandbox.
  type: string
  enum:
    - environment
    - file
    - credential
```

to:

```yaml
SecretType:
  description: Schema of a stored secret.
  type: string
  enum:
    - token
    - oauth
    - file
```

Also update the `SecretMetadata.name` example if it still hints at the old terminology.

- [ ] **Step 2: Rebuild `fabro-api`**

Run: `cargo build -p fabro-api`
Expected: progenitor regenerates the Rust enum. The build will fail if any consumer references the old variants — those get fixed in later tasks.

- [ ] **Step 3: Update the API round-trip tests**

In `lib/crates/fabro-api/tests/secret_type_round_trip.rs`, update the test cases to cover `Token`, `Oauth`, `File`. Same for `secret_metadata_round_trip.rs` if it references variants by name.

- [ ] **Step 4: Run the api crate tests**

Run: `cargo nextest run -p fabro-api`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add docs/public/api-reference/fabro-api.yaml lib/crates/fabro-api/
git commit -m "$(cat <<'EOF'
refactor(api): update SecretType enum to schema-shaped variants

token | oauth | file. Regenerates fabro-api types via
progenitor. Tests updated for the new wire vocabulary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: Update the server secrets handler and install flow

**Files:**
- Modify: `lib/crates/fabro-server/src/server/handler/secrets.rs`
- Modify: `lib/crates/fabro-server/src/install.rs`
- Modify: `lib/crates/fabro-server/src/diagnostics.rs`
- Modify: `lib/crates/fabro-server/src/run_manifest.rs`
- Modify: `lib/crates/fabro-server/src/server.rs` (re-exports)
- Modify: `lib/crates/fabro-server/src/server/tests.rs`
- Modify: `lib/crates/fabro-server/src/test_support.rs`
- Modify: `lib/crates/fabro-server/tests/it/api/install.rs`

- [ ] **Step 1: Update `secrets.rs` create handler**

In `lib/crates/fabro-server/src/server/handler/secrets.rs`:

- Drop the import and usage of `parse_credential_secret` — it no longer exists.
- Replace the `if secret_type == SecretType::Credential { ... parse_credential_secret ... }` block with the equivalent for `SecretType::Oauth`: validate that `value` deserializes to `OAuthCredential` and reject with `bad_request` on failure.
- Update `SecretType::Environment` → `SecretType::Token` in the Daytona validation block.

Example replacement for the validation block:

```rust
if secret_type == SecretType::Oauth {
    if let Err(err) = serde_json::from_str::<OAuthCredential>(&value) {
        return ApiError::bad_request(format!("invalid oauth credential JSON: {err}")).into_response();
    }
}
if secret_type == SecretType::Token && name == EnvVars::DAYTONA_API_KEY {
    // ... existing daytona check, unchanged body ...
}
```

Add an import for `fabro_auth::OAuthCredential` if not already present (you may need to thread it through the `server.rs` re-export hub at line 92).

- [ ] **Step 2: Update `install.rs` provider-secret persistence**

In `lib/crates/fabro-server/src/install.rs` around line 1537–1559, replace the `AuthCredential { ... details: AuthDetails::ApiKey { key } }` construction with:

```rust
for provider in llm.providers {
    let name = provider_secret_name(&provider.provider);
    vault_secrets.push(VaultSecretWrite {
        name,
        value:       provider.api_key,
        secret_type: VaultSecretType::Token,
        description: None,
    });
}
```

Where `provider_secret_name` returns e.g. `"ANTHROPIC_API_KEY"` for Anthropic — match the names used in the provider TOMLs from Task 9. The simplest implementation: a `match` on `ProviderId` that returns the conventional env-var name, or pull it from the catalog's first `CredentialRef::Vault` entry (preferred, single source of truth).

- [ ] **Step 3: Update `install.rs` Daytona/GitHub blocks**

Change `VaultSecretType::Environment` → `VaultSecretType::Token` for the Daytona and GitHub token writes (around lines 1533 and 1578).

- [ ] **Step 4: Update `diagnostics.rs` and `run_manifest.rs`**

In `lib/crates/fabro-server/src/diagnostics.rs` around line 747 and `lib/crates/fabro-server/src/run_manifest.rs` around line 2342, change `SecretType::Credential` references. Read the surrounding context to decide whether the right replacement is `Token` (it's exposing an api-key value) or `Oauth` (it's the OAuth record). Most likely `Token` for diagnostics, `Oauth` if it specifically references an OAuth secret.

- [ ] **Step 5: Update `server.rs` re-export hub (around line 92)**

Drop the `parse_credential_secret` from the use list. Update any `SecretType` variant references that flow through.

- [ ] **Step 6: Update `server/tests.rs` and `tests/it/api/install.rs`**

Rename all `SecretType::Environment` → `SecretType::Token` and `SecretType::Credential` → `SecretType::Oauth`. Where tests previously inserted a JSON `AuthCredential` blob under `SecretType::Credential`, replace with `vault_set_token`/`vault_set_oauth` helpers.

The test at `server/tests.rs:6158` (`SecretType::Credential` for `GITHUB_TOKEN`) is suspect — `GITHUB_TOKEN` is a token secret, not OAuth. It should be `Token`.

- [ ] **Step 7: Build and test**

Run: `cargo nextest run -p fabro-server`
Expected: green. Accept any insta snapshots whose diffs are the schema rename (verify first with `cargo insta pending-snapshots`).

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-server/
git commit -m "$(cat <<'EOF'
refactor(server): use schema-typed SecretType everywhere

CreateSecret handler validates oauth JSON shape on write. Install
flow writes API keys as Token secrets (not the old JSON envelope).
Daytona and GitHub tokens move to SecretType::Token.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: Update CLI args, secret commands, and provider login

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs`
- Modify: `lib/crates/fabro-cli/src/commands/secret/set.rs`
- Modify: `lib/crates/fabro-cli/src/commands/provider/login.rs`
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/runner.rs`
- Modify: `lib/crates/fabro-cli/src/shared/provider_auth.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/doctor.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/install.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/run.rs`
- Modify: `lib/crates/fabro-cli/tests/it/workflow/acp.rs`
- Modify: `lib/crates/fabro-cli/tests/it/workflow/hooks.rs`

- [ ] **Step 1: Update `SecretTypeArg` in `args.rs`**

Around line 656:

```rust
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum SecretTypeArg {
    Token,
    File,
}
```

Drop `Environment` (replaced by `Token`). Do **not** add `Oauth` here — oauth secrets are written by `fabro provider login`, never by `fabro secret set`.

Update the default in `SecretSetArgs.r#type`:

```rust
#[arg(long, value_enum, default_value = "token")]
pub(crate) r#type: SecretTypeArg,
```

- [ ] **Step 2: Update `commands/secret/set.rs`**

```rust
fn api_secret_type(secret_type: SecretTypeArg) -> types::SecretType {
    match secret_type {
        SecretTypeArg::Token => types::SecretType::Token,
        SecretTypeArg::File => types::SecretType::File,
    }
}
```

- [ ] **Step 3: Rewrite `commands/provider/login.rs`**

Replace the whole file with:

```rust
use anyhow::Result;
use fabro_api::types;
use fabro_auth::LoginResult;
use fabro_util::terminal::Styles;

use crate::args::ProviderLoginArgs;
use crate::command_context::CommandContext;
use crate::shared::provider_auth;

pub(super) async fn login_command(
    args: ProviderLoginArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    base_ctx.require_no_json_override()?;
    let printer = base_ctx.printer();
    let s = Styles::detect_stderr();
    let ctx = base_ctx.with_target(&args.target)?;
    let server = ctx.server().await?;
    let result = if args.api_key_stdin {
        provider_auth::authenticate_provider_with_api_key_source_and_catalog(
            args.provider,
            provider_auth::ApiKeySource::Stdin,
            &s,
            printer,
            ctx.catalog()?,
        )
        .await?
    } else {
        provider_auth::authenticate_provider_with_catalog(
            args.provider,
            &s,
            printer,
            ctx.catalog()?,
        )
        .await?
    };

    let (name, value, type_) = match result {
        LoginResult::ApiKey { provider, key } => {
            let name = api_key_secret_name(&provider, ctx.catalog()?);
            (name, key, types::SecretType::Token)
        }
        LoginResult::OAuth { credential, .. } => {
            ("OPENAI_CODEX".to_string(), serde_json::to_string(&credential)?, types::SecretType::Oauth)
        }
    };

    server
        .create_secret(types::CreateSecretRequest {
            name: name.clone(),
            value,
            type_,
            description: None,
        })
        .await?;
    fabro_util::printerr!(printer, "  {} Saved {}", s.green.apply_to("✔"), name);
    Ok(())
}

/// Returns the first `vault:NAME` from the provider's catalog `credentials`
/// list, falling back to the provider id uppercased + `_API_KEY`.
fn api_key_secret_name(provider: &fabro_model::ProviderId, catalog: &fabro_model::Catalog) -> String {
    use fabro_model::CredentialRef;

    catalog
        .provider(provider)
        .and_then(|p| p.auth.as_ref())
        .and_then(|auth| {
            auth.credentials.iter().find_map(|r| match r {
                CredentialRef::Vault(name) => Some(name.clone()),
                CredentialRef::Env(_) => None,
            })
        })
        .unwrap_or_else(|| format!("{}_API_KEY", provider.to_string().to_uppercase()))
}
```

(Adapt the imports and `catalog.provider(...).auth` access path to match the actual API of the catalog accessor. The point is: the secret name comes from the catalog, not a magic constant.)

- [ ] **Step 4: Update `shared/provider_auth.rs`**

Search for return types `AuthCredential` and change them to `LoginResult`. The function bodies that built `AuthCredential::ApiKey` should construct `LoginResult::ApiKey`; same for the OAuth path.

- [ ] **Step 5: Update `commands/install.rs`**

Around line 1180 and 2958, change `SecretType` references and `CredentialRef::Credential(_)` matches per the new vocabulary. Test fixtures that write JSON `AuthCredential` blobs become `Token` secrets.

- [ ] **Step 6: Update CLI tests**

For each of `tests/it/cmd/doctor.rs`, `cmd/install.rs`, `cmd/run.rs`, `workflow/acp.rs`, `workflow/hooks.rs`:

- Replace `SecretType::Environment` → `SecretType::Token`, `SecretType::Credential` → `SecretType::Oauth`.
- Replace JSON `AuthCredential` writes with `vault_set_token`/`vault_set_oauth`.
- Accept insta snapshot diffs that are the rename only (verify first).

- [ ] **Step 7: Build and test**

Run: `cargo build --workspace && cargo nextest run -p fabro-cli`
Expected: green. Use `cargo insta pending-snapshots` and accept renames.

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-cli/
git commit -m "$(cat <<'EOF'
refactor(cli): use LoginResult and schema-typed secrets

fabro secret set --type now accepts token|file (default token).
fabro provider login picks the vault name from the catalog and writes
either a Token or Oauth secret based on the login result.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: Update remaining workflow/config references

**Files:**
- Modify: `lib/crates/fabro-config/src/layers/llm.rs`
- Modify: `lib/crates/fabro-config/src/layers/combine.rs`
- Modify: `lib/crates/fabro-config/src/layers/mod.rs`
- Modify: `lib/crates/fabro-config/src/lib.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/api.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/launch_env.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs`
- Modify: `lib/crates/fabro-workflow/tests/it/integration.rs`
- Modify: `lib/crates/fabro-install/src/lib.rs`

- [ ] **Step 1: Sweep `CredentialRef::Credential` → `CredentialRef::Vault`**

Run: `rg -n 'CredentialRef::Credential\b'` — should be empty if Task 8 was thorough. Any straggler: rename and recheck its surrounding logic for env-fallback assumptions.

- [ ] **Step 2: Sweep `SecretType::Environment` and `SecretType::Credential`**

Run: `rg -n 'SecretType::Environment|SecretType::Credential\b'` — should be empty. Rename any straggler to `Token`/`Oauth`.

- [ ] **Step 3: Update `config/layers/llm.rs` tests**

The tests at `config/layers/llm.rs:217`, `223`, `277`, `500`, `501`, `756`, `764` use `CredentialRef::Credential(...)` and `CredentialRef::Env(...)` literals. Rename the `Credential` ones to `Vault` and update the string under test (`"credential:openai_codex"` → `"vault:OPENAI_CODEX"`, etc.) to match the new prefix.

- [ ] **Step 4: Update workflow llm/launch_env.rs**

Look at line 84 — the `let CredentialRef::Env(name) = credential_ref else { return None; }` pattern. This is filtering for env vars when constructing the spawned-process env. Keep this site `env:`-only. `vault:` token secrets must not be implicitly projected into spawned process env; if a workflow needs vault-backed environment values later, add an explicit mapping feature rather than reintroducing bulk vault export.

- [ ] **Step 5: Build and test the affected crates**

Run: `cargo nextest run -p fabro-config -p fabro-workflow -p fabro-install`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
refactor: update workflow/config/install for vault: prefix and schemas

Final consumer updates: CredentialRef::Vault rename propagates,
SecretType variant references aligned with the new vocabulary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: Regenerate TypeScript API client

**Files:**
- Auto-regenerated: `lib/packages/fabro-api-client/`

- [ ] **Step 1: Regenerate**

Run: `cd lib/packages/fabro-api-client && bun run generate`
Expected: TS types now expose `'token' | 'oauth' | 'file'` for `SecretType`.

- [ ] **Step 2: Build the package**

Run: `cd lib/packages/fabro-api-client && bun run typecheck && bun run build`
Expected: green.

- [ ] **Step 3: Build the web app**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: green. (The web UI doesn't reference `SecretType` by literal — verified during plan discovery — so no source changes should be needed.)

- [ ] **Step 4: Commit**

```bash
git add lib/packages/fabro-api-client/
git commit -m "$(cat <<'EOF'
chore(api-client): regenerate TS client for new SecretType variants

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 15: Update documentation

**Files:**
- Modify: `docs/public/reference/sdk.mdx` (if it documents the SecretType enum)
- Modify: `docs/public/reference/user-configuration.mdx`
- Modify: `docs/public/core-concepts/models.mdx`
- Modify: `docs/public/changelog/2026-05-13.mdx` (or create a new dated entry for today)
- Modify: `docs/internal/server-secrets-strategy.md` (if it describes the old schema)

- [ ] **Step 1: Update docs that show TOML examples**

Search docs for `credential:` literally and replace with `vault:`. Search for descriptions of `SecretType::Environment` / `SecretType::Credential` and update.

Run: `rg -n 'credential:|SecretType::Environment|SecretType::Credential' docs/`
Expected after edits: only references inside the changelog entry describing the rename itself.

- [ ] **Step 2: Add a changelog entry**

Create `docs/public/changelog/2026-05-18.mdx` (or append to an existing 2026-05-18 entry if one exists from earlier work today):

```mdx
---
title: "Vault and credential source cleanup"
date: 2026-05-18
---

The provider credential reference grammar is now explicit:
`env:NAME` reads only the process environment, `vault:NAME` reads only
the vault. Vault secrets carry a schema (`token`, `oauth`, or
`file`) describing what they hold; the resolver branches on that schema
to build the right credential. API keys are stored as plain tokens,
not JSON envelopes.
```

- [ ] **Step 3: Update `docs/internal/server-secrets-strategy.md` if needed**

Read it. If it describes the old `Environment`/`Credential` schema, update to reflect the new `Token`/`Oauth`/`File` vocabulary. If it doesn't touch that, leave it alone.

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "$(cat <<'EOF'
docs: update for vault: prefix and schema-typed SecretType

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: Final verification

**Files:**
- (none — verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: clean.

- [ ] **Step 2: Full workspace tests**

Run: `cargo nextest run --workspace`
Expected: all green. If any insta snapshots are still pending, check them with `cargo insta pending-snapshots` and accept the rename diffs only.

- [ ] **Step 3: Format and lint**

Run: `cargo +nightly-2026-04-14 fmt --all`
Run: `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`
Expected: both green.

- [ ] **Step 4: Search for residue**

Run: `rg -n '"credential:"|AuthCredential\b|AuthDetails\b|credential_id_for|parse_credential_secret|vault_get_credential|vault_set_credential|vault_get_string|vault_set_string|vault_credentials_for_provider|credential_entries\b|SecretType::Environment\b|SecretType::Credential\b|SecretType::String\b|SecretType::CodexOauth\b|CredentialRef::Credential\b|codex_oauth' lib/ apps/ docs/`
Expected: zero hits in source code. Any hits in markdown changelogs describing the migration itself are fine.

Run: `rg -n 'pub fn snapshot|\.snapshot\(\)' lib/crates/fabro-vault/`
Expected: zero hits. `Vault::snapshot()` should be gone.

- [ ] **Step 5: TypeScript build**

Run: `cd apps/fabro-web && bun run typecheck && bun run build`
Run: `cd lib/packages/fabro-api-client && bun run typecheck`
Expected: green.

- [ ] **Step 6: Spot-check live provider auth (optional but recommended)**

If you have credentials in `.env`:

Run: `set -a && source .env && set +a && cargo nextest run -p fabro-llm --profile e2e --run-ignored only --test-threads 1`
Expected: live provider tests pass. This exercises the env: path with real keys.

- [ ] **Step 7: Final commit if formatting changed anything**

```bash
git status
# if dirty:
git add -u
git commit -m "$(cat <<'EOF'
style: cargo fmt after vault/credential cleanup

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Resolved Decisions and Remaining Notes

1. **`OPENAI_CODEX` secret name.** Use `OPENAI_CODEX` as the canonical vault entry name for the OpenAI Codex OAuth credential. Task 9 uses it in the OpenAI provider TOML; Task 12 hardcodes the same in `provider/login.rs`. The validator in `vault.set` enforces env-var-shaped names, so uppercase is intentional.

2. **`vault:` references to `File` secrets.** Treat this as a hard error (Task 7 step 2), not a silent miss. A schema mismatch means the catalog/vault configuration is wrong.

3. **No bulk vault env projection.** `Vault::snapshot()` is removed. `Token` means opaque secret material, not "project this to env." If workflow commands later need vault-backed environment variables, add an explicit mapping feature.

4. **`provider_auth::authenticate_provider_with_catalog` return type.** Task 12 step 4 assumes it returns the OAuth/api-key login result. If it currently returns a fully-built `AuthCredential` *with* a `provider` field that downstream code relies on, thread `provider` through `LoginResult` (already done in the enum definition) and adjust call sites accordingly.
