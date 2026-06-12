# Mattermost Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Mattermost as a first-class notification and interview backend, feature-parallel to Slack, so a Fabro server can drive run lifecycle notifications and interactive human interview prompts through Mattermost.

**Architecture:** New `lib/crates/fabro-mattermost/` crate with 7 modules mirrors `lib/crates/fabro-slack/` module-for-module. `MattermostService` lives in `fabro-server/src/server.rs` as a concrete sibling to `SlackService` — no shared trait. Inbound interactions arrive via WebSocket (thread replies) and a new `POST /api/v1/webhooks/mattermost` HTTP endpoint (button actions).

**Tech Stack:** Rust; `tokio-tungstenite` for WebSocket; Mattermost REST API `/api/v4`; Axum for the webhook route.

---

## File Structure

**Create:**
- `lib/crates/fabro-mattermost/Cargo.toml`
- `lib/crates/fabro-mattermost/src/lib.rs` — config, credential types, resolution
- `lib/crates/fabro-mattermost/src/client.rs` — `MattermostClient`, `PostedMessage`, `MattermostApiError`
- `lib/crates/fabro-mattermost/src/threads.rs` — `ThreadRegistry`, `MattermostQuestionRef`
- `lib/crates/fabro-mattermost/src/webhook.rs` — `MattermostAnswerSubmission`, `parse_action`
- `lib/crates/fabro-mattermost/src/dispatch.rs` — `DispatchAction`, `WsEvent`, `dispatch()`
- `lib/crates/fabro-mattermost/src/attachments.rs` — message formatting
- `lib/crates/fabro-mattermost/src/connection.rs` — WebSocket loop, backoff reconnect
- `docs/public/integrations/mattermost.mdx`

**Modify:**
- `lib/crates/fabro-types/src/principal.rs` — add `Principal::Mattermost`
- `lib/crates/fabro-types/src/settings/server.rs` — add `MattermostIntegrationSettings`
- `lib/crates/fabro-types/src/settings/run.rs` — add `mattermost` fields to notification/interview settings
- `lib/crates/fabro-static/src/env_vars.rs` — add two secret constants
- `lib/crates/fabro-static/src/secret_registry.rs` — register in `OPTIONAL_VAULT_SECRETS`
- `lib/crates/fabro-server/Cargo.toml` — add `fabro-mattermost` dependency
- `lib/crates/fabro-server/src/server.rs` — `MattermostService`, `AppState` fields, wiring
- `lib/crates/fabro-server/src/server/handler/mod.rs` — webhook handler + route in `real_routes()`
- `docs/public/administration/server-configuration.mdx` — Mattermost section
- `.env.example` — add commented-out secrets

---

### Task 1: `Principal::Mattermost` in fabro-types

**Files:**
- Modify: `lib/crates/fabro-types/src/principal.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `lib/crates/fabro-types/src/principal.rs`:

```rust
#[test]
fn round_trips_mattermost_variant() {
    assert_round_trip(&Principal::Mattermost {
        team_id:   "T1".to_string(),
        user_id:   "U1".to_string(),
        user_name: Some("ada".to_string()),
    });
}

#[test]
fn mattermost_principal_serializes_correctly() {
    let principal = Principal::Mattermost {
        team_id:   "T1".to_string(),
        user_id:   "U1".to_string(),
        user_name: None,
    };
    let value = serde_json::to_value(&principal).unwrap();
    assert_eq!(value["kind"], "mattermost");
    assert_eq!(value["team_id"], "T1");
    assert_eq!(value["user_id"], "U1");
    assert!(value.get("user_name").is_none() || value["user_name"].is_null());
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-types -- round_trips_mattermost_variant
```

Expected: FAIL — `Principal::Mattermost` does not exist

- [ ] **Step 3: Add `Principal::Mattermost` variant**

In `lib/crates/fabro-types/src/principal.rs`, add the new variant immediately after `Principal::Slack { ... }`:

```rust
Mattermost {
    team_id:   String,
    user_id:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_name: Option<String>,
},
```

Update `kind()` — add arm after the `Slack` arm:

```rust
Self::Mattermost { .. } => "mattermost",
```

Update `display()` — add arms after the `Slack` arms:

```rust
Self::Mattermost {
    user_name: Some(user_name),
    ..
} => user_name.clone(),
Self::Mattermost {
    team_id, user_id, ..
} => format!("{team_id}:{user_id}"),
```

Update `emit_principal_http_log!` in `server.rs` — add arm (this will be caught by the compiler; for now just let the build fail on that file). Actually `principal.rs` has no compile error yet; server.rs will get a non-exhaustive match error in Task 12.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-types -- mattermost
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/principal.rs
git commit -m "feat(types): add Principal::Mattermost variant"
```

---

### Task 2: `MattermostIntegrationSettings` in server settings

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/server.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing tests module in `server.rs` (or create one if absent):

```rust
#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn mattermost_settings_default_enabled() {
        let settings = MattermostIntegrationSettings::default();
        assert!(settings.enabled);
        assert!(settings.url.is_none());
        assert!(settings.team.is_none());
        assert!(settings.default_channel.is_none());
    }

    #[test]
    fn server_integrations_has_mattermost() {
        let settings = ServerIntegrationsSettings::default();
        assert!(settings.mattermost.enabled);
    }

    #[test]
    fn mattermost_settings_round_trips_toml() {
        let toml = r#"
[mattermost]
enabled = true
url = "https://mm.example.com"
team = "myteam"
default_channel = "fabro-alerts"
"#;
        let integrations: ServerIntegrationsSettings = toml::from_str(toml).unwrap();
        assert!(integrations.mattermost.enabled);
        assert_eq!(
            integrations.mattermost.url.as_ref().unwrap().as_str(),
            "https://mm.example.com"
        );
        assert_eq!(
            integrations.mattermost.team.as_ref().unwrap().as_str(),
            "myteam"
        );
        assert_eq!(
            integrations.mattermost.default_channel.as_ref().unwrap().as_str(),
            "fabro-alerts"
        );
    }
}
```

Note: `InterpString::as_str()` is used here — verify the method exists; if not, use `.resolve(|_| None).unwrap().value.as_str()`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-types -- mattermost_settings
```

Expected: FAIL — `MattermostIntegrationSettings` does not exist

- [ ] **Step 3: Add `MattermostIntegrationSettings` and the field**

In `lib/crates/fabro-types/src/settings/server.rs`, add this struct after `SlackIntegrationSettings`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MattermostIntegrationSettings {
    pub enabled:         bool,
    pub url:             Option<InterpString>,
    pub team:            Option<InterpString>,
    pub default_channel: Option<InterpString>,
}

impl Default for MattermostIntegrationSettings {
    fn default() -> Self {
        Self {
            enabled:         true,
            url:             None,
            team:            None,
            default_channel: None,
        }
    }
}
```

Add the field to `ServerIntegrationsSettings`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIntegrationsSettings {
    pub github:      GithubIntegrationSettings,
    pub slack:       SlackIntegrationSettings,
    pub mattermost:  MattermostIntegrationSettings,
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-types -- mattermost_settings
```

Expected: PASS

Also verify the broader types test suite still passes:

```bash
cargo nextest run -p fabro-types
```

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/settings/server.rs
git commit -m "feat(types): add MattermostIntegrationSettings to server integrations"
```

---

### Task 3: Mattermost fields in run notification/interview settings

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/run.rs`

- [ ] **Step 1: Write the failing test**

Locate or create a tests module in `run.rs`. Add:

```rust
#[test]
fn notification_route_has_mattermost_field() {
    let toml_str = r#"
[on-start]
enabled = true
provider = "mattermost"
events = ["run.started"]
[on-start.mattermost]
channel = "fabro-alerts"
"#;
    let notifications: std::collections::HashMap<String, NotificationRouteSettings> =
        toml::from_str(toml_str).unwrap();
    let route = &notifications["on-start"];
    assert_eq!(route.provider.as_deref(), Some("mattermost"));
    assert_eq!(
        route
            .mattermost
            .as_ref()
            .unwrap()
            .channel
            .as_ref()
            .unwrap()
            .as_str(),
        "fabro-alerts"
    );
}

#[test]
fn interviews_settings_has_mattermost_field() {
    let toml_str = r#"
provider = "mattermost"
[mattermost]
channel = "fabro-reviews"
"#;
    let interviews: RunInterviewsSettings = toml::from_str(toml_str).unwrap();
    assert_eq!(interviews.provider.as_deref(), Some("mattermost"));
    assert_eq!(
        interviews
            .mattermost
            .as_ref()
            .unwrap()
            .channel
            .as_ref()
            .unwrap()
            .as_str(),
        "fabro-reviews"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-types -- notification_route_has_mattermost_field
```

Expected: FAIL

- [ ] **Step 3: Add `mattermost` fields to the settings structs**

In `lib/crates/fabro-types/src/settings/run.rs`, update `NotificationRouteSettings`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationRouteSettings {
    pub enabled:    bool,
    pub provider:   Option<String>,
    pub events:     Vec<String>,
    pub slack:      Option<NotificationProviderSettings>,
    pub mattermost: Option<NotificationProviderSettings>,
}
```

Update `RunInterviewsSettings`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInterviewsSettings {
    pub provider:   Option<String>,
    pub slack:      Option<InterviewProviderSettings>,
    pub mattermost: Option<InterviewProviderSettings>,
}
```

Update `substitute_variables` in the `RunNamespace` impl — add mattermost substitution alongside the existing slack substitution:

```rust
for route in self.notifications.values_mut() {
    substitute_option_string(&mut route.provider, &mut lookup)?;
    substitute_string_vec(&mut route.events, &mut lookup)?;
    if let Some(slack) = &mut route.slack {
        substitute_option(&mut slack.channel, &mut lookup)?;
    }
    if let Some(mattermost) = &mut route.mattermost {
        substitute_option(&mut mattermost.channel, &mut lookup)?;
    }
}
substitute_option_string(&mut self.interviews.provider, &mut lookup)?;
if let Some(slack) = &mut self.interviews.slack {
    substitute_option(&mut slack.channel, &mut lookup)?;
}
if let Some(mattermost) = &mut self.interviews.mattermost {
    substitute_option(&mut mattermost.channel, &mut lookup)?;
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-types
```

Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/settings/run.rs
git commit -m "feat(types): add mattermost fields to notification and interview settings"
```

---

### Task 4: Env var constants and secret registry entries

**Files:**
- Modify: `lib/crates/fabro-static/src/env_vars.rs`
- Modify: `lib/crates/fabro-static/src/secret_registry.rs`

- [ ] **Step 1: Write the failing tests**

In `env_vars.rs` tests module, the existing `env_var_constants_match_their_names` test covers new entries automatically once added. But add a targeted test so it fails visibly:

In `secret_registry.rs` tests module, locate `classifies_optional_vault_secrets` and add two entries to its array so the test fails until the registry entries exist.

Add to the test array in `classifies_optional_vault_secrets`:
```rust
EnvVars::FABRO_MATTERMOST_TOKEN,
EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET,
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p fabro-static
```

Expected: FAIL — constants don't exist yet

- [ ] **Step 3: Add constants to `env_vars.rs`**

In `lib/crates/fabro-static/src/env_vars.rs`, in the Fabro core section, add immediately after `FABRO_SLACK_BOT_TOKEN`:

```rust
pub const FABRO_MATTERMOST_TOKEN: &'static str = "FABRO_MATTERMOST_TOKEN";
pub const FABRO_MATTERMOST_WEBHOOK_SECRET: &'static str = "FABRO_MATTERMOST_WEBHOOK_SECRET";
```

Add both to the `env_var_constants_are_non_empty_and_single_tokens` values array in the tests:

```rust
EnvVars::FABRO_MATTERMOST_TOKEN,
EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET,
```

- [ ] **Step 4: Register in `secret_registry.rs`**

In `lib/crates/fabro-static/src/secret_registry.rs`, add to `OPTIONAL_VAULT_SECRETS` immediately after the Slack entries:

```rust
const OPTIONAL_VAULT_SECRETS: &[&str] = &[
    EnvVars::ANTHROPIC_API_KEY,
    EnvVars::BRAVE_SEARCH_API_KEY,
    EnvVars::FABRO_MATTERMOST_TOKEN,          // add
    EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET, // add
    EnvVars::FABRO_SLACK_APP_TOKEN,
    EnvVars::FABRO_SLACK_BOT_TOKEN,
    // ... rest unchanged
];
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-static
```

Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-static/src/env_vars.rs lib/crates/fabro-static/src/secret_registry.rs
git commit -m "feat(static): add FABRO_MATTERMOST_TOKEN and FABRO_MATTERMOST_WEBHOOK_SECRET"
```

---

### Task 5: Create `fabro-mattermost` crate scaffold with config

**Files:**
- Create: `lib/crates/fabro-mattermost/Cargo.toml`
- Create: `lib/crates/fabro-mattermost/src/lib.rs`

- [ ] **Step 1: Write the failing test (to be added to lib.rs)**

```rust
// in lib.rs tests module:
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_configured_when_both_secrets_present() {
        let resolution = resolve_credentials_status_with_lookup(|name| match name {
            "FABRO_MATTERMOST_TOKEN" => Some("my-token".to_string()),
            "FABRO_MATTERMOST_WEBHOOK_SECRET" => Some("my-secret".to_string()),
            _ => None,
        });
        assert!(matches!(
            resolution,
            MattermostCredentialResolution::Configured(_)
        ));
    }

    #[test]
    fn resolve_missing_when_token_absent() {
        let resolution = resolve_credentials_status_with_lookup(|name| match name {
            "FABRO_MATTERMOST_WEBHOOK_SECRET" => Some("my-secret".to_string()),
            _ => None,
        });
        match resolution {
            MattermostCredentialResolution::Missing { env_vars } => {
                assert!(env_vars.contains(&"FABRO_MATTERMOST_TOKEN"));
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn resolve_missing_when_webhook_secret_absent() {
        let resolution = resolve_credentials_status_with_lookup(|name| match name {
            "FABRO_MATTERMOST_TOKEN" => Some("my-token".to_string()),
            _ => None,
        });
        match resolution {
            MattermostCredentialResolution::Missing { env_vars } => {
                assert!(env_vars.contains(&"FABRO_MATTERMOST_WEBHOOK_SECRET"));
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn resolve_missing_lists_both_when_neither_present() {
        let resolution = resolve_credentials_status_with_lookup(|_| None);
        match resolution {
            MattermostCredentialResolution::Missing { env_vars } => {
                assert_eq!(env_vars.len(), 2);
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn empty_string_counts_as_missing() {
        let resolution = resolve_credentials_status_with_lookup(|_| Some(String::new()));
        assert!(matches!(
            resolution,
            MattermostCredentialResolution::Missing { .. }
        ));
    }
}
```

- [ ] **Step 2: Run test to verify it fails (crate doesn't exist yet)**

```bash
cargo nextest run -p fabro-mattermost 2>&1 | head -5
```

Expected: error — package not found

- [ ] **Step 3: Create `Cargo.toml`**

Create `lib/crates/fabro-mattermost/Cargo.toml`:

```toml
[package]
name = "fabro-mattermost"
edition.workspace = true
version.workspace = true
publish = false
license.workspace = true
description = "Mattermost integration for Fabro notifications and interviews"

[lib]
doctest = false

[lints]
workspace = true

[dependencies]
fabro-interview = { path = "../fabro-interview" }
fabro-types = { path = "../fabro-types" }
fabro-http.workspace = true
fabro-static.workspace = true
futures-util.workspace = true
serde.workspace = true
serde_json.workspace = true
strum.workspace = true
tokio.workspace = true
thiserror.workspace = true
tokio-tungstenite.workspace = true
tracing.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
toml.workspace = true
rustls = { version = "0.23", default-features = false, features = ["std", "ring"] }
```

- [ ] **Step 4: Create `lib.rs` with config and module declarations**

Create `lib/crates/fabro-mattermost/src/lib.rs`:

```rust
pub mod attachments;
pub mod client;
pub mod connection;
pub mod dispatch;
pub mod threads;
pub mod webhook;

use fabro_static::EnvVars;

#[derive(Debug, Clone)]
pub struct MattermostCredentials {
    pub token:          String,
    pub webhook_secret: String,
}

#[derive(Debug, Clone)]
pub enum MattermostCredentialResolution {
    Configured(MattermostCredentials),
    Missing { env_vars: Vec<&'static str> },
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

pub fn resolve_credentials_status_with_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> MattermostCredentialResolution {
    let token = non_empty(lookup(EnvVars::FABRO_MATTERMOST_TOKEN));
    let webhook_secret = non_empty(lookup(EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET));
    match (token, webhook_secret) {
        (Some(token), Some(webhook_secret)) => {
            MattermostCredentialResolution::Configured(MattermostCredentials {
                token,
                webhook_secret,
            })
        }
        (token, webhook_secret) => {
            let mut env_vars = Vec::new();
            if token.is_none() {
                env_vars.push(EnvVars::FABRO_MATTERMOST_TOKEN);
            }
            if webhook_secret.is_none() {
                env_vars.push(EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET);
            }
            MattermostCredentialResolution::Missing { env_vars }
        }
    }
}

// Stub modules — filled in subsequent tasks
```

Note: the `pub mod` declarations will fail to compile until the module files exist. Create empty stub files for each:

```bash
touch lib/crates/fabro-mattermost/src/attachments.rs \
      lib/crates/fabro-mattermost/src/client.rs \
      lib/crates/fabro-mattermost/src/connection.rs \
      lib/crates/fabro-mattermost/src/dispatch.rs \
      lib/crates/fabro-mattermost/src/threads.rs \
      lib/crates/fabro-mattermost/src/webhook.rs
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost
```

Expected: config tests pass (other modules empty — no tests to run yet)

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-mattermost/
git commit -m "feat(mattermost): scaffold fabro-mattermost crate with credential resolution"
```

---

### Task 6: `threads.rs` — `ThreadRegistry`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/threads.rs`

This is a structural copy of `fabro-slack/src/threads.rs`, keyed on Mattermost `post_id` instead of `ts`.

- [ ] **Step 1: Write the failing tests**

```rust
// threads.rs tests module:
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_resolve() {
        let registry = ThreadRegistry::new();
        registry.register("post-1", "run-1", "q-1");
        let result = registry.resolve("post-1").unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(result.qid, "q-1");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let registry = ThreadRegistry::new();
        assert!(registry.resolve("unknown").is_none());
    }

    #[test]
    fn remove_clears_mapping() {
        let registry = ThreadRegistry::new();
        registry.register("post-1", "run-1", "q-1");
        registry.remove("post-1");
        assert!(registry.resolve("post-1").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- threads
```

Expected: FAIL

- [ ] **Step 3: Implement `threads.rs`**

```rust
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MattermostQuestionRef {
    pub run_id: String,
    pub qid:    String,
}

#[derive(Default)]
pub struct ThreadRegistry {
    post_to_question: Mutex<HashMap<String, MattermostQuestionRef>>,
}

impl ThreadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, post_id: &str, run_id: &str, question_id: &str) {
        self.post_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .insert(post_id.to_string(), MattermostQuestionRef {
                run_id: run_id.to_string(),
                qid:    question_id.to_string(),
            });
    }

    pub fn resolve(&self, post_id: &str) -> Option<MattermostQuestionRef> {
        self.post_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .get(post_id)
            .cloned()
    }

    pub fn remove(&self, post_id: &str) {
        self.post_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .remove(post_id);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost -- threads
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/threads.rs
git commit -m "feat(mattermost): add ThreadRegistry keyed on post_id"
```

---

### Task 7: `client.rs` — `MattermostClient`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/client.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_post_response_extracts_post_and_channel_id() {
        let response = serde_json::json!({
            "id": "post-abc123",
            "channel_id": "channel-xyz"
        });
        let posted = parse_post_response(&response).unwrap();
        assert_eq!(posted.post_id, "post-abc123");
        assert_eq!(posted.channel_id, "channel-xyz");
    }

    #[test]
    fn parse_post_response_missing_id_is_error() {
        let response = serde_json::json!({ "channel_id": "channel-xyz" });
        assert!(parse_post_response(&response).is_err());
    }

    #[test]
    fn mattermost_api_error_display() {
        let err = MattermostApiError::Http("timeout".to_string());
        assert_eq!(err.to_string(), "Mattermost HTTP error: timeout");
        let err = MattermostApiError::Api("channel_not_found".to_string());
        assert_eq!(err.to_string(), "Mattermost API error: channel_not_found");
    }

    #[test]
    fn channel_cache_hit_on_second_call() {
        // This test verifies the cache key format and population, not the HTTP call.
        // Done by inspecting the cache directly after calling resolve_channel with a stub.
        let client = MattermostClient::new_with_http(
            "token".to_string(),
            "http://mm.example.com".to_string(),
            fabro_http::http_client().unwrap(),
        );
        // Pre-populate the cache to test the hit path
        client
            .channel_cache
            .lock()
            .unwrap()
            .insert("myteam/town-square".to_string(), "C123".to_string());
        let cached = client
            .channel_cache
            .lock()
            .unwrap()
            .get("myteam/town-square")
            .cloned();
        assert_eq!(cached, Some("C123".to_string()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- client
```

Expected: FAIL

- [ ] **Step 3: Implement `client.rs`**

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct PostedMessage {
    pub post_id:    String,
    pub channel_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MattermostApiError {
    #[error("Mattermost HTTP error: {0}")]
    Http(String),
    #[error("Mattermost API error: {0}")]
    Api(String),
}

#[derive(Clone)]
pub struct MattermostClient {
    pub(crate) token:         String,
    pub(crate) base_url:      String,
    http:                     fabro_http::HttpClient,
    pub(crate) channel_cache: Arc<Mutex<HashMap<String, String>>>,
}

impl MattermostClient {
    pub fn new(token: String, base_url: String) -> Self {
        Self::new_with_http(
            token,
            base_url,
            fabro_http::http_client().expect("Mattermost HTTP client should build"),
        )
    }

    pub fn new_with_http(
        token: String,
        base_url: String,
        http: fabro_http::HttpClient,
    ) -> Self {
        Self {
            token,
            base_url,
            http,
            channel_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn http(&self) -> &fabro_http::HttpClient {
        &self.http
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub async fn post_message(
        &self,
        channel_id: &str,
        message: &str,
        attachments: &[Value],
        root_id: Option<&str>,
    ) -> Result<PostedMessage, MattermostApiError> {
        let mut body = json!({
            "channel_id": channel_id,
            "message": message,
            "props": { "attachments": attachments }
        });
        if let Some(root_id) = root_id {
            body["root_id"] = json!(root_id);
        }
        let resp = self
            .http
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| MattermostApiError::Http(e.to_string()))?;

        let json: Value = resp
            .json()
            .await
            .map_err(|e| MattermostApiError::Http(e.to_string()))?;

        let posted = parse_post_response(&json)?;
        debug!(channel_id, post_id = %posted.post_id, "Posted Mattermost message");
        Ok(posted)
    }

    pub async fn update_post(
        &self,
        post_id: &str,
        message: &str,
        attachments: &[Value],
    ) -> Result<(), MattermostApiError> {
        let body = json!({
            "id": post_id,
            "message": message,
            "props": { "attachments": attachments }
        });
        let resp = self
            .http
            .put(format!("{}/api/v4/posts/{post_id}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| MattermostApiError::Http(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(MattermostApiError::Api(format!("{status}: {text}")));
        }
        Ok(())
    }

    pub async fn resolve_channel(
        &self,
        team: &str,
        channel_name: &str,
    ) -> Result<String, MattermostApiError> {
        let cache_key = format!("{team}/{channel_name}");
        if let Some(cached) = self
            .channel_cache
            .lock()
            .expect("channel cache lock poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(cached);
        }

        let resp = self
            .http
            .get(format!(
                "{}/api/v4/teams/name/{team}/channels/name/{channel_name}",
                self.base_url
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| MattermostApiError::Http(e.to_string()))?;

        let json: Value = resp
            .json()
            .await
            .map_err(|e| MattermostApiError::Http(e.to_string()))?;

        let channel_id = json["id"]
            .as_str()
            .ok_or_else(|| {
                MattermostApiError::Api("missing id in channel response".to_string())
            })?
            .to_string();

        self.channel_cache
            .lock()
            .expect("channel cache lock poisoned")
            .insert(cache_key, channel_id.clone());

        Ok(channel_id)
    }
}

pub fn parse_post_response(json: &Value) -> Result<PostedMessage, MattermostApiError> {
    let post_id = json["id"]
        .as_str()
        .ok_or_else(|| MattermostApiError::Api("missing id in post response".to_string()))?;
    let channel_id = json["channel_id"]
        .as_str()
        .ok_or_else(|| MattermostApiError::Api("missing channel_id in post response".to_string()))?;
    Ok(PostedMessage {
        post_id:    post_id.to_string(),
        channel_id: channel_id.to_string(),
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost -- client
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/client.rs
git commit -m "feat(mattermost): add MattermostClient with post/update/resolve_channel"
```

---

### Task 8: `webhook.rs` — button action parsing

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/webhook.rs`

The Mattermost server POSTs JSON to `integration.url` when a button is clicked. This module parses that payload and verifies the token query parameter.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use fabro_interview::AnswerValue;
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_yes_action() {
        let payload = json!({
            "channel_id": "C123",
            "team_id": "T123",
            "user_id": "U123",
            "user_name": "ada",
            "context": {
                "run_id": "run-1",
                "qid": "q-1",
                "kind": "yes"
            }
        });
        let submission = parse_action(&payload).unwrap();
        assert_eq!(submission.run_id, "run-1");
        assert_eq!(submission.qid, "q-1");
        assert_eq!(submission.answer.value, AnswerValue::Yes);
        assert!(matches!(
            submission.actor,
            fabro_types::Principal::Mattermost {
                ref team_id,
                ref user_id,
                ..
            } if team_id == "T123" && user_id == "U123"
        ));
    }

    #[test]
    fn parse_no_action() {
        let payload = json!({
            "team_id": "T1", "user_id": "U1", "user_name": "bob",
            "context": { "run_id": "r1", "qid": "q1", "kind": "no" }
        });
        let submission = parse_action(&payload).unwrap();
        assert_eq!(submission.answer.value, AnswerValue::No);
    }

    #[test]
    fn parse_selected_action() {
        let payload = json!({
            "team_id": "T1", "user_id": "U1", "user_name": null,
            "context": {
                "run_id": "r1", "qid": "q1",
                "kind": "selected", "key": "option-a"
            }
        });
        let submission = parse_action(&payload).unwrap();
        assert_eq!(
            submission.answer.value,
            AnswerValue::Selected("option-a".to_string())
        );
    }

    #[test]
    fn parse_action_returns_none_when_context_missing() {
        let payload = json!({ "team_id": "T1", "user_id": "U1" });
        assert!(parse_action(&payload).is_none());
    }

    #[test]
    fn parse_action_returns_none_for_unknown_kind() {
        let payload = json!({
            "team_id": "T1", "user_id": "U1",
            "context": { "run_id": "r1", "qid": "q1", "kind": "unknown" }
        });
        assert!(parse_action(&payload).is_none());
    }

    #[test]
    fn constant_time_eq_is_correct() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"wrong!"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- webhook
```

Expected: FAIL

- [ ] **Step 3: Implement `webhook.rs`**

```rust
use fabro_interview::{Answer, AnswerValue};
use fabro_types::Principal;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct MattermostAnswerSubmission {
    pub run_id: String,
    pub qid:    String,
    pub answer: Answer,
    pub actor:  Principal,
}

/// Parse an inbound Mattermost button action payload.
/// Returns `None` for unknown or incomplete payloads.
pub fn parse_action(payload: &Value) -> Option<MattermostAnswerSubmission> {
    let context = &payload["context"];
    let run_id = context["run_id"].as_str()?.to_string();
    let qid = context["qid"].as_str()?.to_string();
    let kind = context["kind"].as_str()?;

    let answer_value = match kind {
        "yes" => AnswerValue::Yes,
        "no" => AnswerValue::No,
        "selected" => {
            let key = context["key"].as_str()?.to_string();
            AnswerValue::Selected(key)
        }
        _ => return None,
    };

    let team_id = payload["team_id"].as_str().unwrap_or("unknown").to_string();
    let user_id = payload["user_id"].as_str().unwrap_or("unknown").to_string();
    let user_name = payload["user_name"].as_str().map(str::to_string);

    Some(MattermostAnswerSubmission {
        run_id,
        qid,
        answer: Answer {
            value: answer_value,
        },
        actor: Principal::Mattermost {
            team_id,
            user_id,
            user_name,
        },
    })
}

/// Constant-time byte slice comparison to prevent timing attacks on the webhook token.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
```

Note: `Answer` must be constructable as `Answer { value: AnswerValue::Yes }` — check `fabro_interview::Answer`. If `Answer` has a constructor like `Answer::yes()` use that instead:

```rust
let answer_value = match kind {
    "yes"  => return Some(make_submission(run_id, qid, Answer::yes(), actor(payload))),
    ...
};
```

Check the `fabro_interview::Answer` API:
- If `Answer` has `fn yes() -> Self`, `fn no() -> Self`, `fn selected(key: String) -> Self`, `fn text(s: String) -> Self` — use those.
- Otherwise construct `Answer { value: AnswerValue::Yes }` directly.

Based on the Slack code (`Answer::text(text)` seen in dispatch.rs), the constructor pattern exists. Use:

```rust
let answer = match kind {
    "yes"      => Answer::yes(),
    "no"       => Answer::no(),
    "selected" => {
        let key = context["key"].as_str()?.to_string();
        Answer::selected(key)
    }
    _ => return None,
};
```

If `Answer::yes()` / `Answer::selected()` don't exist, substitute with:
```rust
let answer = Answer { value: match kind { "yes" => AnswerValue::Yes, ... } };
```

- [ ] **Step 4: Verify `Answer` API and fix if needed**

```bash
cargo check -p fabro-mattermost 2>&1 | grep "no method named\|Answer"
```

Adjust constructor calls based on what compiles.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost -- webhook
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-mattermost/src/webhook.rs
git commit -m "feat(mattermost): add webhook action parsing and constant-time token comparison"
```

---

### Task 9: `dispatch.rs` — WebSocket event dispatch

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/dispatch.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use fabro_interview::AnswerValue;

    use super::*;
    use crate::threads::ThreadRegistry;

    fn registry() -> ThreadRegistry {
        ThreadRegistry::new()
    }

    #[test]
    fn hello_produces_connected() {
        let reg = registry();
        let event: WsEvent = serde_json::from_str(r#"{"event":"hello","data":{}}"#).unwrap();
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Connected));
    }

    #[test]
    fn goodbye_produces_reconnect() {
        let reg = registry();
        let event: WsEvent = serde_json::from_str(r#"{"event":"goodbye","data":{}}"#).unwrap();
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Reconnect));
    }

    #[test]
    fn posted_with_unregistered_root_id_is_ignored() {
        let reg = registry();
        let post_json = serde_json::json!({
            "id": "post-1",
            "root_id": "root-999",
            "message": "hello",
            "user_id": "U1"
        })
        .to_string();
        let event = WsEvent {
            event: "posted".to_string(),
            data: serde_json::json!({ "post": post_json, "team_id": "T1" }),
            broadcast: serde_json::json!({}),
        };
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn posted_with_registered_root_id_produces_submit_answer() {
        let reg = registry();
        reg.register("root-1", "run-10", "q-10");
        let post_json = serde_json::json!({
            "id": "post-2",
            "root_id": "root-1",
            "message": "my answer",
            "user_id": "U1",
            "type": ""
        })
        .to_string();
        let event = WsEvent {
            event: "posted".to_string(),
            data: serde_json::json!({ "post": post_json, "team_id": "T1" }),
            broadcast: serde_json::json!({}),
        };
        let action = dispatch(&event, &reg);
        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let s = *submission;
                assert_eq!(s.run_id, "run-10");
                assert_eq!(s.qid, "q-10");
                assert_eq!(s.answer.value, AnswerValue::Text("my answer".to_string()));
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[test]
    fn posted_with_no_root_id_is_ignored() {
        let reg = registry();
        let post_json = serde_json::json!({
            "id": "post-3",
            "root_id": "",
            "message": "top-level post",
            "user_id": "U1"
        })
        .to_string();
        let event = WsEvent {
            event: "posted".to_string(),
            data: serde_json::json!({ "post": post_json }),
            broadcast: serde_json::json!({}),
        };
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn posted_with_system_type_is_ignored() {
        let reg = registry();
        reg.register("root-1", "run-1", "q-1");
        let post_json = serde_json::json!({
            "id": "post-4",
            "root_id": "root-1",
            "message": "system msg",
            "user_id": "U1",
            "type": "system_join_channel"
        })
        .to_string();
        let event = WsEvent {
            event: "posted".to_string(),
            data: serde_json::json!({ "post": post_json }),
            broadcast: serde_json::json!({}),
        };
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn unknown_event_type_is_ignored() {
        let reg = registry();
        let event = WsEvent {
            event: "reaction_added".to_string(),
            data: serde_json::json!({}),
            broadcast: serde_json::json!({}),
        };
        let action = dispatch(&event, &reg);
        assert!(matches!(action, DispatchAction::Ignored));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- dispatch
```

Expected: FAIL

- [ ] **Step 3: Implement `dispatch.rs`**

```rust
use fabro_interview::Answer;
use fabro_types::Principal;
use serde::Deserialize;

use crate::threads::ThreadRegistry;
use crate::webhook::MattermostAnswerSubmission;

#[derive(Debug, Deserialize)]
pub struct WsEvent {
    pub event:     String,
    pub data:      serde_json::Value,
    #[serde(default)]
    pub broadcast: serde_json::Value,
}

#[derive(Debug)]
pub enum DispatchAction {
    Connected,
    SubmitAnswer(Box<MattermostAnswerSubmission>),
    Reconnect,
    Ignored,
}

pub fn dispatch(event: &WsEvent, thread_registry: &ThreadRegistry) -> DispatchAction {
    match event.event.as_str() {
        "hello" => DispatchAction::Connected,
        "goodbye" => DispatchAction::Reconnect,
        "posted" => dispatch_posted(event, thread_registry),
        _ => DispatchAction::Ignored,
    }
}

fn dispatch_posted(event: &WsEvent, thread_registry: &ThreadRegistry) -> DispatchAction {
    let post_json_str = match event.data["post"].as_str() {
        Some(s) => s,
        None => return DispatchAction::Ignored,
    };

    let post: serde_json::Value = match serde_json::from_str(post_json_str) {
        Ok(v) => v,
        Err(_) => return DispatchAction::Ignored,
    };

    // Ignore system messages (type is non-empty)
    if post["type"].as_str().is_some_and(|t| !t.is_empty()) {
        return DispatchAction::Ignored;
    }

    let root_id = match post["root_id"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => return DispatchAction::Ignored,
    };

    let Some(question_ref) = thread_registry.resolve(root_id) else {
        return DispatchAction::Ignored;
    };

    let message = match post["message"].as_str().filter(|s| !s.is_empty()) {
        Some(m) => m,
        None => return DispatchAction::Ignored,
    };

    let team_id = event.data["team_id"]
        .as_str()
        .or_else(|| event.broadcast["team_id"].as_str())
        .unwrap_or("unknown")
        .to_string();
    let user_id = post["user_id"].as_str().unwrap_or("unknown").to_string();

    DispatchAction::SubmitAnswer(Box::new(MattermostAnswerSubmission {
        run_id: question_ref.run_id,
        qid:    question_ref.qid,
        answer: Answer::text(message.to_string()),
        actor:  Principal::Mattermost {
            team_id,
            user_id,
            user_name: None,
        },
    }))
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost -- dispatch
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/dispatch.rs
git commit -m "feat(mattermost): add WS dispatch for posted/hello/goodbye events"
```

---

### Task 10: `attachments.rs` — message formatting

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/attachments.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use fabro_interview::Question;
    use fabro_types::{InterviewOption, QuestionType};

    use super::*;

    const WEBHOOK_URL: &str = "https://fabro.example/api/v1/webhooks/mattermost?token=secret";

    #[test]
    fn yes_no_produces_two_buttons() {
        let q = Question::new("Approve this PR?", QuestionType::YesNo);
        let attachments = question_to_attachments("run-1", "q-1", &q, None, WEBHOOK_URL);
        assert_eq!(attachments.len(), 1);
        let actions = attachments[0]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["name"], "Yes");
        assert_eq!(actions[1]["name"], "No");
        assert_eq!(
            actions[0]["integration"]["context"]["kind"],
            "yes"
        );
        assert_eq!(
            actions[1]["integration"]["context"]["kind"],
            "no"
        );
    }

    #[test]
    fn yes_no_buttons_carry_run_id_and_qid() {
        let q = Question::new("Approve?", QuestionType::YesNo);
        let attachments = question_to_attachments("run-7", "q-7", &q, None, WEBHOOK_URL);
        let actions = attachments[0]["actions"].as_array().unwrap();
        assert_eq!(actions[0]["integration"]["context"]["run_id"], "run-7");
        assert_eq!(actions[0]["integration"]["context"]["qid"], "q-7");
    }

    #[test]
    fn multiple_choice_produces_one_button_per_option() {
        let mut q = Question::new("Pick one:", QuestionType::MultipleChoice);
        q.options = vec![
            InterviewOption {
                key:         "rs".to_string(),
                label:       "Rust".to_string(),
                description: None,
                preview:     None,
            },
            InterviewOption {
                key:         "ts".to_string(),
                label:       "TypeScript".to_string(),
                description: None,
                preview:     None,
            },
        ];
        let attachments = question_to_attachments("run-1", "q-1", &q, None, WEBHOOK_URL);
        let actions = attachments[0]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["name"], "Rust");
        assert_eq!(actions[0]["integration"]["context"]["key"], "rs");
        assert_eq!(actions[1]["name"], "TypeScript");
    }

    #[test]
    fn freeform_has_no_buttons() {
        let q = Question::new("What repo URL?", QuestionType::Freeform);
        let attachments = question_to_attachments("run-1", "q-1", &q, None, WEBHOOK_URL);
        let actions = attachments[0]["actions"].as_array().unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn run_link_is_set_when_url_provided() {
        let q = Question::new("Approve?", QuestionType::YesNo);
        let attachments = question_to_attachments(
            "run-1",
            "q-1",
            &q,
            Some("https://fabro.example/runs/run-1"),
            WEBHOOK_URL,
        );
        assert_eq!(
            attachments[0]["title_link"],
            "https://fabro.example/runs/run-1"
        );
    }

    #[test]
    fn title_is_truncated_to_200_chars() {
        let q = Question::new("a".repeat(300), QuestionType::YesNo);
        let attachments = question_to_attachments("run-1", "q-1", &q, None, WEBHOOK_URL);
        assert!(attachments[0]["title"].as_str().unwrap().len() <= 200);
    }

    #[test]
    fn answered_attachments_show_question_and_answer() {
        let attachments = answered_attachments("Approve?", "Yes");
        assert_eq!(attachments.len(), 1);
        let text = attachments[0]["text"].as_str().unwrap();
        assert!(text.contains("Approve?"));
        assert!(text.contains("Yes"));
    }

    #[test]
    fn lifecycle_started_has_green_color_and_title() {
        let attachments = run_lifecycle_attachments(RunLifecycleKind::Started, &RunLifecycleDetails {
            run_id:         "run-1",
            run_url:        None,
            workflow_label: "deploy",
            result:         None,
            duration_ms:    None,
            pull_request:   None,
        });
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["color"], "#36a64f");
        let title = attachments[0]["title"].as_str().unwrap();
        assert!(title.contains("started") || title.contains("deploy"));
    }

    #[test]
    fn lifecycle_failed_has_red_color() {
        let attachments = run_lifecycle_attachments(RunLifecycleKind::Failed, &RunLifecycleDetails {
            run_id:         "run-1",
            run_url:        None,
            workflow_label: "deploy",
            result:         Some("command failed"),
            duration_ms:    None,
            pull_request:   None,
        });
        assert_eq!(attachments[0]["color"], "#cc0000");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- attachments
```

Expected: FAIL

- [ ] **Step 3: Implement `attachments.rs`**

```rust
use fabro_interview::Question;
use fabro_types::QuestionType;
use serde_json::{Value, json};
use strum::IntoStaticStr;

const MM_TITLE_LIMIT: usize = 200;
const MM_TEXT_LIMIT: usize = 8000;

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Button action for a Mattermost attachment.
fn mm_button(name: &str, webhook_url: &str, context: Value) -> Value {
    json!({
        "name": name,
        "type": "button",
        "integration": {
            "url": webhook_url,
            "context": context
        }
    })
}

/// Build `props.attachments` for an interview question.
pub fn question_to_attachments(
    run_id: &str,
    qid: &str,
    question: &Question,
    run_web_url: Option<&str>,
    webhook_url: &str,
) -> Vec<Value> {
    let title = truncate(&question.text, MM_TITLE_LIMIT);
    let actions = build_question_actions(run_id, qid, question, webhook_url);

    let mut attachment = json!({
        "color":   "#0072C6",
        "title":   title,
        "actions": actions
    });

    if let Some(url) = run_web_url {
        attachment["title_link"] = json!(url);
    }

    if let Some(context_display) = question.context_display.as_deref() {
        let trimmed = context_display.trim();
        if !trimmed.is_empty() {
            attachment["text"] = json!(truncate(trimmed, MM_TEXT_LIMIT));
        }
    }

    vec![attachment]
}

fn build_question_actions(
    run_id: &str,
    qid: &str,
    question: &Question,
    webhook_url: &str,
) -> Vec<Value> {
    match question.question_type {
        QuestionType::YesNo | QuestionType::Confirmation => vec![
            mm_button(
                "Yes",
                webhook_url,
                json!({"run_id": run_id, "qid": qid, "kind": "yes"}),
            ),
            mm_button(
                "No",
                webhook_url,
                json!({"run_id": run_id, "qid": qid, "kind": "no"}),
            ),
        ],
        QuestionType::MultipleChoice => question
            .options
            .iter()
            .map(|opt| {
                mm_button(
                    &opt.label,
                    webhook_url,
                    json!({"run_id": run_id, "qid": qid, "kind": "selected", "key": opt.key}),
                )
            })
            .collect(),
        QuestionType::Freeform | QuestionType::MultiSelect => vec![],
    }
}

/// Build `props.attachments` after an interview question is answered.
pub fn answered_attachments(question_text: &str, answer_text: &str) -> Vec<Value> {
    vec![json!({
        "color": "#36a64f",
        "text": format!("~~{}~~\n**Answer:** {}", question_text, answer_text)
    })]
}

/// Input for lifecycle notification attachments, parallel to Slack's `RunLifecycleBlocks`.
#[derive(Debug, Clone, Copy)]
pub struct RunLifecyclePullRequest<'a> {
    pub number: u64,
    pub title:  Option<&'a str>,
    pub url:    Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct RunLifecycleDetails<'a> {
    pub run_id:         &'a str,
    pub run_url:        Option<&'a str>,
    pub workflow_label: &'a str,
    pub result:         Option<&'a str>,
    pub duration_ms:    Option<u64>,
    pub pull_request:   Option<RunLifecyclePullRequest<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum RunLifecycleKind {
    #[strum(serialize = "Fabro run started")]
    Started,
    #[strum(serialize = "Fabro run completed")]
    Completed,
    #[strum(serialize = "Fabro run failed")]
    Failed,
}

/// Build `props.attachments` for a run lifecycle event.
pub fn run_lifecycle_attachments(
    kind: RunLifecycleKind,
    details: &RunLifecycleDetails<'_>,
) -> Vec<Value> {
    let color = match kind {
        RunLifecycleKind::Failed => "#cc0000",
        _ => "#36a64f",
    };
    let title: &'static str = kind.into();
    let text = build_lifecycle_text(kind, details);

    let mut attachment = json!({
        "color": color,
        "title": format!("{} — {}", title, details.workflow_label),
        "text":  text
    });

    if let Some(url) = details.run_url {
        attachment["title_link"] = json!(url);
    }

    vec![attachment]
}

fn build_lifecycle_text(kind: RunLifecycleKind, details: &RunLifecycleDetails<'_>) -> String {
    let mut parts: Vec<String> = vec![
        format!("**Workflow:** `{}`", details.workflow_label),
        format!("**Run ID:** `{}`", details.run_id),
    ];

    if let Some(result) = details.result.filter(|r| !r.trim().is_empty()) {
        let label = if matches!(kind, RunLifecycleKind::Failed) {
            "Failure"
        } else {
            "Result"
        };
        parts.push(format!("**{label}:** {result}"));
    }

    if let Some(ms) = details.duration_ms {
        parts.push(format!("**Duration:** {}", compact_duration(ms)));
    }

    if let Some(pr) = details.pull_request {
        let pr_text = match pr.url {
            Some(url) => format!("[#{}]({})", pr.number, url),
            None => format!("#{}", pr.number),
        };
        let pr_line = match pr.title.filter(|t| !t.trim().is_empty()) {
            Some(title) => format!("**Pull request:** {pr_text} — {title}"),
            None => format!("**Pull request:** {pr_text}"),
        };
        parts.push(pr_line);
    }

    parts.join("\n")
}

fn compact_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{}m {}s", minutes, seconds % 60);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{}h {}m", hours, minutes % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost -- attachments
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/attachments.rs
git commit -m "feat(mattermost): add attachment builders for questions and lifecycle events"
```

---

### Task 11: `connection.rs` — WebSocket loop

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/connection.rs`

Key differences from Slack's `connection.rs`:
- No `open_socket_url` API call. Derive WS URL from `client.base_url()` by replacing scheme.
- After connecting, send `{"seq":1,"action":"authentication_challenge","data":{"token":"..."}}`.
- No ack messages (Mattermost WS doesn't use an envelope+ack pattern).
- `process_message` returns `(ProcessOutcome, DispatchAction)` — no `Option<String>` ack.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::threads::ThreadRegistry;

    fn registry() -> ThreadRegistry {
        ThreadRegistry::new()
    }

    #[test]
    fn process_hello_produces_connected() {
        let text = r#"{"event":"hello","data":{}}"#;
        let (outcome, action) = process_message(text, &registry());
        assert_eq!(outcome, ProcessOutcome::Continue);
        assert!(matches!(action, crate::dispatch::DispatchAction::Connected));
    }

    #[test]
    fn process_goodbye_produces_reconnect() {
        let text = r#"{"event":"goodbye","data":{}}"#;
        let (outcome, action) = process_message(text, &registry());
        assert_eq!(outcome, ProcessOutcome::Reconnect);
        assert!(matches!(action, crate::dispatch::DispatchAction::Reconnect));
    }

    #[test]
    fn process_invalid_json_is_ignored() {
        let (outcome, action) = process_message("not json {{{", &registry());
        assert_eq!(outcome, ProcessOutcome::Continue);
        assert!(matches!(action, crate::dispatch::DispatchAction::Ignored));
    }

    #[test]
    fn wss_url_from_https() {
        assert_eq!(
            wss_url_from_base("https://mm.example.com"),
            "wss://mm.example.com/api/v4/websocket"
        );
    }

    #[test]
    fn wss_url_from_http() {
        assert_eq!(
            wss_url_from_base("http://localhost:8065"),
            "ws://localhost:8065/api/v4/websocket"
        );
    }

    #[tokio::test]
    async fn run_event_loop_submits_answers_via_callback() {
        use tokio::net::TcpListener;
        use tokio_tungstenite::tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Build a URL that the event loop can use directly (ws://)
        let url = format!("ws://{addr}");

        let registry = registry();
        registry.register("root-1", "run-1", "q-1");

        let submissions = Arc::new(Mutex::new(Vec::new()));
        let callback_subs = Arc::clone(&submissions);
        let on_submit: Arc<dyn Fn(crate::webhook::MattermostAnswerSubmission) + Send + Sync> =
            Arc::new(move |s| {
                callback_subs.lock().unwrap().push(s);
            });

        let post_json = serde_json::json!({
            "id": "post-2",
            "root_id": "root-1",
            "message": "my answer",
            "user_id": "U1",
            "type": ""
        })
        .to_string();

        let server = async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(
                r#"{"event":"hello","data":{},"broadcast":{}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                serde_json::json!({
                    "event": "posted",
                    "data": {"post": post_json, "team_id": "T1"},
                    "broadcast": {}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Close(None)).await.unwrap();
            // Drain remaining messages
            while let Some(Ok(_)) = futures_util::StreamExt::next(&mut ws).await {}
        };

        let _server = tokio::spawn(server);
        run_event_loop(&url, "token", &registry, &on_submit)
            .await
            .unwrap();

        let subs = submissions.lock().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].run_id, "run-1");
        assert_eq!(subs[0].qid, "q-1");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p fabro-mattermost -- connection
```

Expected: FAIL

- [ ] **Step 3: Implement `connection.rs`**

```rust
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::client::MattermostClient;
use crate::dispatch::{DispatchAction, WsEvent, dispatch};
use crate::threads::ThreadRegistry;
use crate::webhook::MattermostAnswerSubmission;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    Continue,
    Reconnect,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatusUpdate {
    Connecting,
    Connected,
    Error(String),
}

pub type ConnectionStatusSink = Arc<dyn Fn(ConnectionStatusUpdate) + Send + Sync>;

fn notify_status(sink: Option<&ConnectionStatusSink>, update: ConnectionStatusUpdate) {
    if let Some(sink) = sink {
        sink(update);
    }
}

pub(crate) fn wss_url_from_base(base_url: &str) -> String {
    let ws_base = base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{}/api/v4/websocket", ws_base.trim_end_matches('/'))
}

/// Process a single raw WebSocket text message: parse and dispatch.
pub fn process_message(
    text: &str,
    thread_registry: &ThreadRegistry,
) -> (ProcessOutcome, DispatchAction) {
    let event: WsEvent = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => {
            warn!("Failed to parse Mattermost WebSocket message");
            return (ProcessOutcome::Continue, DispatchAction::Ignored);
        }
    };

    let action = dispatch(&event, thread_registry);
    let outcome = match &action {
        DispatchAction::Reconnect => ProcessOutcome::Reconnect,
        _ => ProcessOutcome::Continue,
    };

    (outcome, action)
}

async fn run_event_loop_inner(
    wss_url: &str,
    token: &str,
    thread_registry: &ThreadRegistry,
    on_submit: &Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync>,
    status_sink: Option<&ConnectionStatusSink>,
) -> Result<(), ConnectionError> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(wss_url)
        .await
        .map_err(|e| ConnectionError::WebSocket(e.to_string()))?;

    let (mut write, mut read) = ws_stream.split();
    info!("Mattermost WebSocket connected");

    // Authenticate
    let auth_msg = serde_json::json!({
        "seq": 1,
        "action": "authentication_challenge",
        "data": {"token": token}
    });
    write
        .send(Message::Text(auth_msg.to_string().into()))
        .await
        .map_err(|e| ConnectionError::WebSocket(e.to_string()))?;
    debug!("Sent Mattermost authentication challenge");

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                error!("Mattermost WebSocket read error: {e}");
                return Err(ConnectionError::WebSocket(e.to_string()));
            }
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("Mattermost WebSocket closed by server");
                return Ok(());
            }
            Message::Ping(data) => {
                let _ = write.send(Message::Pong(data)).await;
                continue;
            }
            _ => continue,
        };

        let (outcome, action) = process_message(&text, thread_registry);

        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                debug!(
                    run_id = submission.run_id.as_str(),
                    qid = submission.qid.as_str(),
                    "Submitting answer from Mattermost thread reply"
                );
                on_submit(submission);
            }
            DispatchAction::Connected => {
                info!("Mattermost WebSocket authentication acknowledged");
                notify_status(status_sink, ConnectionStatusUpdate::Connected);
            }
            DispatchAction::Reconnect | DispatchAction::Ignored => {}
        }

        if outcome == ProcessOutcome::Reconnect {
            info!("Mattermost server requested disconnect; reconnecting");
            return Ok(());
        }
    }

    info!("Mattermost WebSocket stream ended");
    Ok(())
}

pub async fn run_event_loop(
    wss_url: &str,
    token: &str,
    thread_registry: &ThreadRegistry,
    on_submit: &Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync>,
) -> Result<(), ConnectionError> {
    run_event_loop_inner(wss_url, token, thread_registry, on_submit, None).await
}

async fn run_inner(
    client: &MattermostClient,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync>,
    status_sink: Option<ConnectionStatusSink>,
) {
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);
    let wss_url = wss_url_from_base(client.base_url());
    let token = client.token().to_string();

    loop {
        notify_status(status_sink.as_ref(), ConnectionStatusUpdate::Connecting);

        match run_event_loop_inner(
            &wss_url,
            &token,
            thread_registry,
            &on_submit,
            status_sink.as_ref(),
        )
        .await
        {
            Ok(()) => {
                info!("Mattermost event loop ended; reconnecting");
                backoff = std::time::Duration::from_secs(1);
            }
            Err(e) => {
                error!("Mattermost event loop error: {e}; reconnecting");
                notify_status(
                    status_sink.as_ref(),
                    ConnectionStatusUpdate::Error(e.to_string()),
                );
                sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

pub async fn run(
    client: &MattermostClient,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync>,
) {
    run_inner(client, thread_registry, on_submit, None).await;
}

pub async fn run_with_status(
    client: &MattermostClient,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync>,
    status_sink: ConnectionStatusSink,
) {
    run_inner(client, thread_registry, on_submit, Some(status_sink)).await;
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p fabro-mattermost
```

Expected: all PASS

- [ ] **Step 5: Confirm the full crate builds without errors**

```bash
cargo build -p fabro-mattermost
```

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-mattermost/src/connection.rs
git commit -m "feat(mattermost): add WebSocket connection loop with auth challenge and backoff"
```

---

### Task 12: Server wiring — `AppState`, `MattermostService`, startup

**Files:**
- Modify: `lib/crates/fabro-server/Cargo.toml`
- Modify: `lib/crates/fabro-server/src/server.rs`

This task adds:
1. The `fabro-mattermost` dependency
2. `MattermostConnectionRuntimeState` struct
3. `MattermostService` struct + methods
4. `mattermost_service: Option<Arc<MattermostService>>` + `mattermost_started: AtomicBool` to `AppState`
5. `start_optional_mattermost_service()` function
6. Call to `start_optional_mattermost_service()` in `build_router_with_options()`
7. Mattermost service construction in `new_app_state()`
8. `Principal::Mattermost` arm in `emit_principal_http_log!`

- [ ] **Step 1: Add dependency to `fabro-server/Cargo.toml`**

In `lib/crates/fabro-server/Cargo.toml`, find the `fabro-slack` entry and add immediately after it:

```toml
fabro-mattermost = { path = "../fabro-mattermost" }
```

- [ ] **Step 2: Run compile to see what's broken**

```bash
cargo build -p fabro-server 2>&1 | head -20
```

Expected: compile errors — `Principal::Mattermost` unmatched in `emit_principal_http_log!`

- [ ] **Step 3: Add `Principal::Mattermost` arm to `emit_principal_http_log!`**

In `server.rs`, find the macro at the logging section (~line 1931) and add a Mattermost arm after the Slack arm:

```rust
Principal::Mattermost {
    team_id, user_id, ..
} => emit_http_log!(
    $level,
    team_id = team_id.as_str(),
    user_id = user_id.as_str(),
),
```

- [ ] **Step 4: Add `MattermostConnectionRuntimeState` (after `SlackConnectionRuntimeState`)**

In `server.rs`, add immediately after the `SlackConnectionRuntimeState` definition:

```rust
#[derive(Debug, Clone)]
struct MattermostConnectionRuntimeState {
    status:            IntegrationConnectionState,
    last_connected_at: Option<DateTime<Utc>>,
    last_error:        Option<String>,
}

impl Default for MattermostConnectionRuntimeState {
    fn default() -> Self {
        Self {
            status:            IntegrationConnectionState::Connecting,
            last_connected_at: None,
            last_error:        None,
        }
    }
}
```

- [ ] **Step 5: Add import aliases for fabro-mattermost**

In `server.rs`, add imports after the existing `fabro_slack::*` imports. Find the block of `fabro_slack` imports and add:

```rust
use fabro_mattermost::{
    self as fabro_mattermost_crate,
    MattermostCredentialResolution,
    attachments as mm_attachments,
    client::{MattermostClient, PostedMessage as MattermostPostedMessage},
    connection as mm_connection,
    threads::ThreadRegistry as MattermostThreadRegistry,
    webhook::MattermostAnswerSubmission,
};
use fabro_mattermost::connection::ConnectionStatusUpdate as MattermostConnectionStatusUpdate;
```

- [ ] **Step 6: Add `MattermostService` struct and impl (after `SlackService` impl block)**

After the closing `}` of `impl SlackService`, add:

```rust
#[derive(Clone)]
struct MattermostService {
    client:          MattermostClient,
    team:            String,
    default_channel: Option<String>,
    webhook_secret:  String,
    server_url:      Option<String>,
    posted_messages: Arc<Mutex<HashMap<(RunId, String), MattermostPostedMessage>>>,
    thread_registry: Arc<MattermostThreadRegistry>,
    connection:      Arc<Mutex<MattermostConnectionRuntimeState>>,
}

impl MattermostService {
    fn new(
        token: String,
        base_url: String,
        team: String,
        default_channel: Option<String>,
        webhook_secret: String,
        server_url: Option<String>,
    ) -> Self {
        Self {
            client:          MattermostClient::new(token, base_url),
            team,
            default_channel,
            webhook_secret,
            server_url,
            posted_messages: Arc::new(Mutex::new(HashMap::new())),
            thread_registry: Arc::new(MattermostThreadRegistry::new()),
            connection:      Arc::new(Mutex::new(MattermostConnectionRuntimeState::default())),
        }
    }

    fn connection_status(&self) -> IntegrationConnectionStatus {
        let state = self
            .connection
            .lock()
            .expect("mattermost connection state lock poisoned")
            .clone();
        IntegrationConnectionStatus {
            kind:              IntegrationConnectionKind::WebSocket,
            status:            state.status,
            last_connected_at: state.last_connected_at,
            last_error:        state.last_error,
        }
    }

    fn status_sink(&self) -> mm_connection::ConnectionStatusSink {
        let connection = Arc::clone(&self.connection);
        Arc::new(move |update| {
            let mut state = connection
                .lock()
                .expect("mattermost connection state lock poisoned");
            match update {
                MattermostConnectionStatusUpdate::Connecting => {
                    state.status = IntegrationConnectionState::Connecting;
                    state.last_error = None;
                }
                MattermostConnectionStatusUpdate::Connected => {
                    state.status = IntegrationConnectionState::Connected;
                    state.last_connected_at = Some(Utc::now());
                    state.last_error = None;
                }
                MattermostConnectionStatusUpdate::Error(error) => {
                    state.status = IntegrationConnectionState::Error;
                    state.last_error = Some(sanitize_integration_error(&error));
                }
            }
        })
    }

    fn webhook_url(&self) -> Option<String> {
        let base = self.server_url.as_deref()?;
        Some(format!(
            "{}/api/v1/webhooks/mattermost?token={}",
            base.trim_end_matches('/'),
            self.webhook_secret
        ))
    }

    async fn handle_event(
        &self,
        state: &AppState,
        envelope: &EventEnvelope,
        run_web_url: Option<&str>,
    ) {
        let event = &envelope.event;
        match &event.body {
            EventBody::InterviewStarted(props) => {
                if props.question_id.is_empty() {
                    return;
                }
                let Some(default_channel) = self.default_channel.as_deref() else {
                    return;
                };
                let key = (event.run_id, props.question_id.clone());
                if self
                    .posted_messages
                    .lock()
                    .expect("mattermost posted messages lock poisoned")
                    .contains_key(&key)
                {
                    return;
                }
                let Some(webhook_url) = self.webhook_url() else {
                    return;
                };

                let question = runtime_question_from_interview_record(&InterviewQuestionRecord {
                    id:              props.question_id.clone(),
                    text:            props.question.clone(),
                    stage:           props.stage.clone(),
                    question_type:   props.question_type.parse().unwrap_or_default(),
                    options:         props.options.clone(),
                    allow_freeform:  props.allow_freeform,
                    timeout_seconds: props.timeout_seconds,
                    context_display: props.context_display.clone(),
                });

                let attachments = mm_attachments::question_to_attachments(
                    &event.run_id.to_string(),
                    &props.question_id,
                    &question,
                    run_web_url,
                    &webhook_url,
                );

                let channel_id = match self
                    .client
                    .resolve_channel(&self.team, default_channel)
                    .await
                {
                    Ok(id) => id,
                    Err(err) => {
                        warn!(
                            run_id = %event.run_id,
                            channel = default_channel,
                            error = %err,
                            "Failed to resolve Mattermost interview channel"
                        );
                        return;
                    }
                };

                if let Ok(posted) = self
                    .client
                    .post_message(&channel_id, "", &attachments, None)
                    .await
                {
                    if question.allow_freeform
                        || question.question_type == QuestionType::Freeform
                    {
                        self.thread_registry.register(
                            &posted.post_id,
                            &event.run_id.to_string(),
                            &props.question_id,
                        );
                    }
                    self.posted_messages
                        .lock()
                        .expect("mattermost posted messages lock poisoned")
                        .insert(key, posted);
                }
            }
            EventBody::InterviewCompleted(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    &props.answer,
                )
                .await;
            }
            EventBody::InterviewTimeout(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    "Timed out",
                )
                .await;
            }
            EventBody::InterviewInterrupted(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    "Interrupted",
                )
                .await;
            }
            EventBody::RunStarted(_)
            | EventBody::RunCompleted(_)
            | EventBody::RunFailed(_) => {
                self.handle_lifecycle_event(state, envelope, run_web_url)
                    .await;
            }
            _ => {}
        }
    }

    async fn handle_lifecycle_event(
        &self,
        state: &AppState,
        envelope: &EventEnvelope,
        run_web_url: Option<&str>,
    ) {
        let event = &envelope.event;
        let Some(details) = mm_lifecycle_details(event) else {
            return;
        };
        let event_name = event.body.event_name();
        let projection = match state.store.get_cached_run(&event.run_id).await {
            Ok(Some(cached)) => cached.projection,
            Ok(None) => {
                warn!(
                    run_id = %event.run_id,
                    event = event_name,
                    "Skipping Mattermost lifecycle notification: run projection missing"
                );
                return;
            }
            Err(err) => {
                warn!(
                    run_id = %event.run_id,
                    event = event_name,
                    error = %err,
                    "Skipping Mattermost lifecycle notification: run projection load failed"
                );
                return;
            }
        };

        let mut routes: Vec<_> = projection
            .spec
            .settings
            .run
            .notifications
            .iter()
            .filter(|(_, route)| {
                route.enabled
                    && route.provider.as_deref() == Some("mattermost")
                    && route.events.iter().any(|e| e == event_name)
            })
            .collect();
        if routes.is_empty() {
            return;
        }
        routes.sort_by_key(|(route_name, _)| *route_name);

        let prior = if matches!(
            details.kind,
            mm_attachments::RunLifecycleKind::Started
        ) {
            PriorSlackLifecycleEventDetails::default()
        } else {
            load_prior_slack_lifecycle_event_details(state, event.run_id, envelope.seq).await
        };

        let workflow_label = slack_lifecycle_workflow_label(
            projection.as_ref(),
            details
                .started_event_name
                .as_deref()
                .or(prior.started_event_name.as_deref()),
            event_name,
        );
        let pull_request = prior.pull_request.or_else(|| {
            projection
                .pull_request
                .as_ref()
                .map(slack_lifecycle_pull_request_from_link)
        });
        let run_id = event.run_id.to_string();
        let run_url = run_web_url.or(projection.web_url.as_deref());
        let pr_details = pull_request.as_ref().map(|pr| {
            mm_attachments::RunLifecyclePullRequest {
                number: pr.number,
                title:  pr.title.as_deref(),
                url:    pr.url.as_deref(),
            }
        });
        let attachments = mm_attachments::run_lifecycle_attachments(
            details.kind,
            &mm_attachments::RunLifecycleDetails {
                run_id: &run_id,
                run_url,
                workflow_label: &workflow_label,
                result:         details.result.as_deref(),
                duration_ms:    details.duration_ms,
                pull_request:   pr_details,
            },
        );

        let attachments = &attachments;
        let posts = routes.into_iter().filter_map(|(route_name, route)| {
            let channel_name = route
                .mattermost
                .as_ref()
                .and_then(|mm| mm.channel.as_ref())?
                .resolve(|name| (state.env_lookup)(name))
                .ok()
                .map(|r| r.value)?;
            if channel_name.trim().is_empty() {
                warn!(
                    run_id = %event.run_id,
                    notification_route = route_name.as_str(),
                    "Skipping Mattermost lifecycle notification route with empty channel"
                );
                return None;
            }
            Some(async move {
                let channel_id = match self.client.resolve_channel(&self.team, &channel_name).await {
                    Ok(id) => id,
                    Err(err) => {
                        warn!(
                            run_id = %event.run_id,
                            notification_route = route_name.as_str(),
                            error = %err,
                            "Failed to resolve Mattermost notification channel"
                        );
                        return;
                    }
                };
                if let Err(err) = self.client.post_message(&channel_id, "", attachments, None).await {
                    warn!(
                        run_id = %event.run_id,
                        notification_route = route_name.as_str(),
                        error = %err,
                        "Failed to post Mattermost lifecycle notification"
                    );
                }
            })
        });
        join_all(posts).await;
    }

    async fn finish_interview(
        &self,
        run_id: RunId,
        qid: &str,
        question_text: &str,
        answer_text: &str,
    ) {
        let key = (run_id, qid.to_string());
        let posted = self
            .posted_messages
            .lock()
            .expect("mattermost posted messages lock poisoned")
            .remove(&key);
        let Some(posted) = posted else {
            return;
        };
        self.thread_registry.remove(&posted.post_id);
        let attachments = mm_attachments::answered_attachments(question_text, answer_text);
        let _ = self
            .client
            .update_post(&posted.post_id, "", &attachments)
            .await;
    }

    async fn submit_answer(&self, state: Arc<AppState>, submission: MattermostAnswerSubmission) {
        let Ok(run_id) = RunId::from_str(&submission.run_id) else {
            return;
        };
        let Ok(pending) =
            load_pending_interview(state.as_ref(), run_id, &submission.qid).await
        else {
            return;
        };
        let answer_submission = AnswerSubmission::new(submission.answer, submission.actor);
        let _ = submit_pending_interview_answer(state.as_ref(), &pending, answer_submission).await;
    }
}
```

Note: `mm_lifecycle_details` function should mirror `slack_lifecycle_details` but return `mm_attachments::RunLifecycleKind` instead. Add it near the Slack version:

```rust
struct MmLifecycleDetails {
    kind:               mm_attachments::RunLifecycleKind,
    started_event_name: Option<String>,
    result:             Option<String>,
    duration_ms:        Option<u64>,
}

fn mm_lifecycle_details(event: &RunEvent) -> Option<MmLifecycleDetails> {
    match &event.body {
        EventBody::RunStarted(props) => Some(MmLifecycleDetails {
            kind:               mm_attachments::RunLifecycleKind::Started,
            started_event_name: Some(props.name.clone()),
            result:             None,
            duration_ms:        None,
        }),
        EventBody::RunCompleted(props) => Some(MmLifecycleDetails {
            kind:               mm_attachments::RunLifecycleKind::Completed,
            started_event_name: None,
            result:             Some(slack_lifecycle_completed_result(
                &props.status,
                props.reason,
            )),
            duration_ms:        Some(props.timing.wall_time_ms),
        }),
        EventBody::RunFailed(props) => Some(MmLifecycleDetails {
            kind:               mm_attachments::RunLifecycleKind::Failed,
            started_event_name: None,
            result:             Some(slack_lifecycle_failed_result(&props.failure)),
            duration_ms:        Some(props.timing.wall_time_ms),
        }),
        _ => None,
    }
}
```

- [ ] **Step 7: Add `mattermost_service` and `mattermost_started` to `AppState`**

In `AppState` struct, add immediately after `slack_started`:

```rust
mattermost_service: Option<Arc<MattermostService>>,
mattermost_started: AtomicBool,
```

- [ ] **Step 8: Add `start_optional_mattermost_service()` function (after `start_optional_slack_service`)**

```rust
fn start_optional_mattermost_service(state: &Arc<AppState>) {
    let Some(service) = state.mattermost_service.clone() else {
        return;
    };
    if state.mattermost_started.swap(true, Ordering::SeqCst) {
        return;
    }

    let event_state = Arc::clone(state);
    let event_service = Arc::clone(&service);
    tokio::spawn(async move {
        let mut rx = event_state.global_event_tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    let run_web_url = event_state.run_web_url(&envelope.event.run_id);
                    event_service
                        .handle_event(event_state.as_ref(), &envelope, run_web_url.as_deref())
                        .await;
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    let socket_state = Arc::clone(state);
    tokio::spawn(async move {
        let submit_service = Arc::clone(&service);
        let on_submit: Arc<dyn Fn(MattermostAnswerSubmission) + Send + Sync> =
            Arc::new(move |submission| {
                let state = Arc::clone(&socket_state);
                let service = Arc::clone(&submit_service);
                tokio::spawn(async move {
                    service.submit_answer(state, submission).await;
                });
            });
        mm_connection::run_with_status(
            &service.client,
            &service.thread_registry,
            on_submit,
            service.status_sink(),
        )
        .await;
    });
}
```

- [ ] **Step 9: Call `start_optional_mattermost_service` in `build_router_with_options`**

In `build_router_with_options`, immediately after `start_optional_slack_service(&state);`, add:

```rust
start_optional_mattermost_service(&state);
```

- [ ] **Step 10: Add Mattermost service construction in `new_app_state`**

In the `new_app_state` function, after the `slack_service` block, add:

```rust
let mattermost_service = {
    let mm_settings = &current_server_settings.server.integrations.mattermost;
    if mm_settings.enabled {
        let url = mm_settings
            .url
            .as_ref()
            .map(|v| v.resolve(process_env_var).map(|r| r.value).map_err(anyhow::Error::from))
            .transpose()?;
        let team = mm_settings
            .team
            .as_ref()
            .map(|v| v.resolve(process_env_var).map(|r| r.value).map_err(anyhow::Error::from))
            .transpose()?;
        let default_channel = mm_settings
            .default_channel
            .as_ref()
            .map(|v| v.resolve(process_env_var).map(|r| r.value).map_err(anyhow::Error::from))
            .transpose()?;

        let vault_guard = vault.try_read().ok();
        match fabro_mattermost_crate::resolve_credentials_status_with_lookup(|name| {
            vault_guard
                .as_ref()
                .and_then(|vault| vault.get(name).map(str::to_string))
        }) {
            MattermostCredentialResolution::Configured(credentials) => {
                let server_url = state
                    .canonical_origin()   // Not available here — compute from settings
                    .ok();
                // Derive server_url from server.web.url at construction time
                let server_url = mm_settings
                    .url
                    .as_ref()
                    .map(|_| ())  // placeholder — see note below
                    .and(None::<String>);
                // NOTE: server_url for webhook integration.url is resolved from
                // server.api.url or server.web.url at runtime in handle_event
                // using state.canonical_origin(). Store None here; compute per-event.
                let _ = server_url;

                if let (Some(url), Some(team)) = (url, team) {
                    info!(
                        default_channel_configured = default_channel.is_some(),
                        "Mattermost integration enabled"
                    );
                    Some(Arc::new(MattermostService::new(
                        credentials.token,
                        url,
                        team,
                        default_channel,
                        credentials.webhook_secret,
                        None, // server_url resolved at runtime via state.canonical_origin()
                    )))
                } else {
                    info!(
                        missing_env_vars = "url or team not configured",
                        "Mattermost integration disabled; url and team are required"
                    );
                    None
                }
            }
            MattermostCredentialResolution::Missing { env_vars } => {
                info!(
                    missing_env_vars = %env_vars.join(","),
                    "Mattermost integration disabled; missing credentials"
                );
                None
            }
        }
    } else {
        info!("Mattermost integration disabled by server configuration");
        None
    }
};
```

**Note on `server_url` for webhook integration URL:** The `MattermostService::webhook_url()` method returns `None` when `server_url` is `None`. To properly set the server URL, update `MattermostService::handle_event` to derive the webhook URL from `state.canonical_origin()` at call time instead of from a stored field. Change `webhook_url()` to accept `&AppState`:

```rust
fn webhook_url(&self, state: &AppState) -> Option<String> {
    let base = state.canonical_origin().ok()?;
    Some(format!(
        "{}/api/v1/webhooks/mattermost?token={}",
        base.trim_end_matches('/'),
        self.webhook_secret
    ))
}
```

And update the `handle_event` call site accordingly:
```rust
let Some(webhook_url) = self.webhook_url(state) else { return; };
```

Remove the `server_url` field from `MattermostService` and `MattermostService::new()`.

- [ ] **Step 11: Add `mattermost_service` and `mattermost_started` to the `AppState` constructor**

In `Ok(Arc::new(AppState { ... }))` near the end of `new_app_state`, add after the slack lines:

```rust
mattermost_service,
mattermost_started: AtomicBool::new(false),
```

- [ ] **Step 12: Run the build to verify no compile errors**

```bash
cargo build -p fabro-server
```

Fix any compile errors surfaced (type mismatches, missing imports, etc).

- [ ] **Step 13: Run server tests**

```bash
cargo nextest run -p fabro-server
```

Expected: existing tests pass; new paths compile but have no dedicated tests yet (covered in Task 13)

- [ ] **Step 14: Commit**

```bash
git add lib/crates/fabro-server/Cargo.toml lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): add MattermostService and AppState wiring"
```

---

### Task 13: Webhook handler and route

**Files:**
- Modify: `lib/crates/fabro-server/src/server/handler/mod.rs`
- Modify: `lib/crates/fabro-server/src/server.rs` (add handler function + route wiring)

- [ ] **Step 1: Write the failing test**

In `lib/crates/fabro-server/src/server/tests.rs` (or the test module in `server.rs`), add:

```rust
#[tokio::test]
async fn mattermost_webhook_returns_401_with_wrong_token() {
    // Build a test app state with a MattermostService configured with a known webhook secret.
    // POST to /api/v1/webhooks/mattermost?token=wrong — expect 401.
    // (Requires test-support AppState; skip if test helpers not available for this path)
    // For now, test the token verification function directly:
    use fabro_mattermost::webhook::constant_time_eq;
    assert!(!constant_time_eq(b"right-secret", b"wrong-secret"));
    assert!(constant_time_eq(b"right-secret", b"right-secret"));
}
```

The full integration test (correct token → 200, wrong token → 401) is covered in the manual integration test plan in the spec.

- [ ] **Step 2: Add the `mattermost_webhook` handler in `server.rs`**

Add after `github_webhook_routes`:

```rust
#[derive(serde::Deserialize)]
struct MattermostWebhookParams {
    token: Option<String>,
}

async fn mattermost_webhook(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MattermostWebhookParams>,
    Json(body): Json<serde_json::Value>,
) -> StatusCode {
    let Some(service) = state.mattermost_service.as_ref() else {
        return StatusCode::NOT_FOUND;
    };

    let provided_token = params.token.as_deref().unwrap_or("").as_bytes();
    let expected_token = service.webhook_secret.as_bytes();

    if !fabro_mattermost::webhook::constant_time_eq(provided_token, expected_token) {
        return StatusCode::UNAUTHORIZED;
    }

    let Some(submission) = fabro_mattermost::webhook::parse_action(&body) else {
        return StatusCode::OK;
    };

    let service = Arc::clone(service);
    tokio::spawn(async move {
        service.submit_answer(state, submission).await;
    });

    StatusCode::OK
}
```

Note: `MattermostService::webhook_secret` must be `pub(super)` or accessible here. The service struct is in the same module (`server.rs`), so private field access works.

- [ ] **Step 3: Add the route to `build_router_with_options`**

In `build_router_with_options`, add the Mattermost webhook route to `real_router` **after** the principal_layer nest but **before** `with_state(state)`:

```rust
let mut real_router = Router::new().nest(
    "/api/v1",
    api_common
        .merge(handler::real_routes())
        .layer(principal_layer),
);
// Mattermost webhook is outside the auth layer; secured by token query param
if state.mattermost_service.is_some() {
    real_router = real_router
        .route("/api/v1/webhooks/mattermost", post(mattermost_webhook));
}
```

This requires `post` to be imported. Find `use axum::routing::{get, post, ...}` and ensure `post` is already imported (it is, given the github webhook pattern).

- [ ] **Step 4: Build and run tests**

```bash
cargo build -p fabro-server && cargo nextest run -p fabro-server
```

Expected: builds clean, existing tests pass

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-server/src/server.rs lib/crates/fabro-server/src/server/handler/mod.rs
git commit -m "feat(server): add POST /api/v1/webhooks/mattermost route with token verification"
```

---

### Task 14: Final verification and docs

**Files:**
- Create: `docs/public/integrations/mattermost.mdx`
- Modify: `docs/public/administration/server-configuration.mdx`
- Modify: `.env.example`

- [ ] **Step 1: Run the full workspace build and test suite**

```bash
cargo build --workspace && cargo nextest run --workspace
```

Expected: all pass. Fix any remaining compile errors before proceeding.

- [ ] **Step 2: Create `docs/public/integrations/mattermost.mdx`**

Model it on `docs/public/integrations/slack.mdx`. Key sections:
- Overview: what this integration does
- Prerequisites: Mattermost server, bot token, webhook secret
- Setup: `fabro secret set FABRO_MATTERMOST_TOKEN <token>` and `fabro secret set FABRO_MATTERMOST_WEBHOOK_SECRET <random>`
- Server config: the `[server.integrations.mattermost]` TOML block
- Run notifications config: `[run.notifications.on-start]` example
- Run interviews config: `[run.interviews]` example
- Verifying: the startup log line and manual test steps from the spec

- [ ] **Step 3: Update `.env.example`**

Add after the Slack entries:

```bash
# Mattermost integration (optional)
# FABRO_MATTERMOST_TOKEN=<bot-or-personal-access-token>
# FABRO_MATTERMOST_WEBHOOK_SECRET=<random-secret-for-button-actions>
```

- [ ] **Step 4: Update `docs/public/administration/server-configuration.mdx`**

Add a `[server.integrations.mattermost]` section to the integrations reference table, and add `FABRO_MATTERMOST_TOKEN` and `FABRO_MATTERMOST_WEBHOOK_SECRET` to the secrets table.

- [ ] **Step 5: Final commit**

```bash
git add docs/ .env.example
git commit -m "docs: add Mattermost integration documentation and env.example entries"
```

---

## Summary of Commits

1. `feat(types): add Principal::Mattermost variant`
2. `feat(types): add MattermostIntegrationSettings to server integrations`
3. `feat(types): add mattermost fields to notification and interview settings`
4. `feat(static): add FABRO_MATTERMOST_TOKEN and FABRO_MATTERMOST_WEBHOOK_SECRET`
5. `feat(mattermost): scaffold fabro-mattermost crate with credential resolution`
6. `feat(mattermost): add ThreadRegistry keyed on post_id`
7. `feat(mattermost): add MattermostClient with post/update/resolve_channel`
8. `feat(mattermost): add webhook action parsing and constant-time token comparison`
9. `feat(mattermost): add WS dispatch for posted/hello/goodbye events`
10. `feat(mattermost): add attachment builders for questions and lifecycle events`
11. `feat(mattermost): add WebSocket connection loop with auth challenge and backoff`
12. `feat(server): add MattermostService and AppState wiring`
13. `feat(server): add POST /api/v1/webhooks/mattermost route with token verification`
14. `docs: add Mattermost integration documentation and env.example entries`

## Manual Integration Test

After all tasks are complete, run:

```bash
docker run --name mattermost-preview -p 8065:8065 mattermost/mattermost-preview
```

1. Create a bot account, generate a personal access token → `fabro secret set FABRO_MATTERMOST_TOKEN <token>`
2. Generate a random secret → `fabro secret set FABRO_MATTERMOST_WEBHOOK_SECRET <random>`
3. Configure `[server.integrations.mattermost]` pointing at `http://localhost:8065`, team `ad-1`, channel `town-square`
4. Start Fabro server; confirm `"Mattermost integration enabled"` in logs and WebSocket connects
5. Trigger a run; confirm lifecycle notification appears in `town-square`
6. Trigger a run with a yes/no interview; confirm question with buttons; click Yes; confirm post updates
7. Trigger a run with a freeform interview; reply in thread; confirm answer recorded
8. Confirm Slack integration is unaffected throughout
