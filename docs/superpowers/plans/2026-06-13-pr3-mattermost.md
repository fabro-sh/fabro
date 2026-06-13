# PR 3: Mattermost Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver Mattermost as a full `ChatService` provider — lifecycle notifications, interview questions with button actions, and thread-reply answers — matching Slack feature parity.

**Architecture:** New `fabro-mattermost` crate with 8 modules mirroring `fabro-slack`; wired into `chat_services: Vec<Arc<dyn ChatService>>` from PR 2. Credential and config changes touch `fabro-static`, `fabro-types`, `fabro-config`, and `fabro-api`. Mattermost uses a WebSocket inbound loop for thread replies and HTTP webhook callbacks for button actions. Token lives in `?token=...` query string (already threaded through `handle_webhook` in PR 2).

**Tech Stack:** Rust, `tokio-tungstenite`, `axum`, `fabro-chat`, `insta` snapshots, `fabro-test` HTTP helpers

**Spec:** `docs/superpowers/specs/2026-06-13-chat-service-trait-and-providers.md` — §Phase 2: Mattermost, §fabro-types changes, §fabro-config changes, §fabro-static changes, §Testing Strategy

**Branches:** Stacks on PR 2. Branch from PR 2's branch or from `main` after PR 2 merges.

---

## Before you start

```
cargo nextest run --workspace
```

All tests must pass before you make any changes.

---

## File Structure

**Modify:**
- `lib/crates/fabro-static/src/env_vars.rs` — add `FABRO_MATTERMOST_TOKEN`, `FABRO_MATTERMOST_WEBHOOK_SECRET`
- `lib/crates/fabro-types/src/settings/server.rs` — add `MattermostIntegrationSettings`, update `ServerIntegrationsSettings`
- `lib/crates/fabro-types/src/principal.rs` — add `Principal::Mattermost` variant
- `lib/crates/fabro-types/src/settings/run.rs` — add `mattermost` to notification/interview settings
- `lib/crates/fabro-config/src/layers/server.rs` — add `MattermostIntegrationLayer`, update `ServerIntegrationsLayer`
- `lib/crates/fabro-config/src/resolve/server.rs` — add `resolve_mattermost`
- `lib/crates/fabro-config/src/resolve/server.rs` — wire `mattermost` field
- `docs/public/api-reference/fabro-api.yaml` — add `mattermost` to `Principal` oneOf
- `lib/crates/fabro-server/src/server.rs` — wire `MattermostService`, update `http_log_middleware`

**Create:**
- `lib/crates/fabro-mattermost/Cargo.toml`
- `lib/crates/fabro-mattermost/src/lib.rs` — crate root + credential resolution
- `lib/crates/fabro-mattermost/src/client.rs` — REST client
- `lib/crates/fabro-mattermost/src/attachments.rs` — attachment JSON builders
- `lib/crates/fabro-mattermost/src/connection.rs` — WebSocket loop
- `lib/crates/fabro-mattermost/src/dispatch.rs` — classify WS events
- `lib/crates/fabro-mattermost/src/threads.rs` — thread registry
- `lib/crates/fabro-mattermost/src/webhook.rs` — parse and verify button actions
- `lib/crates/fabro-mattermost/src/service.rs` — `MattermostService` + `impl ChatService`

---

### Task 1: Add `MattermostIntegrationSettings` to `fabro-types`

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/server.rs`

- [ ] **Step 1: Add `MattermostIntegrationSettings` struct**

Add after `SlackIntegrationSettings`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MattermostIntegrationSettings {
    pub enabled:         bool,             // default: true
    pub url:             Option<InterpString>,
    pub team:            Option<InterpString>,
    pub default_channel: Option<InterpString>,
}
```

- [ ] **Step 2: Add `TeamsIntegrationSettings` struct**

Add after `MattermostIntegrationSettings`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamsIntegrationSettings {
    pub enabled: bool,   // default: true
}
```

- [ ] **Step 3: Update `ServerIntegrationsSettings`**

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIntegrationsSettings {
    pub github:     GithubIntegrationSettings,
    pub slack:      SlackIntegrationSettings,
    pub mattermost: MattermostIntegrationSettings,
    pub teams:      TeamsIntegrationSettings,
}
```

- [ ] **Step 4: Build `fabro-types`**

```
cargo build -p fabro-types
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/settings/server.rs
git commit -m "feat(types): add MattermostIntegrationSettings and TeamsIntegrationSettings"
```

---

### Task 2: Add `Principal::Mattermost` to `fabro-types`

**Files:**
- Modify: `lib/crates/fabro-types/src/principal.rs`

Read the file first to understand the current enum layout and which methods need updating.

- [ ] **Step 1: Add `Mattermost` and `Teams` variants**

After `Principal::Slack { .. }`, add:

```rust
Mattermost {
    team_id:   String,
    user_id:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_name: Option<String>,
},
Teams {
    tenant_id: String,
    user_id:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_name: Option<String>,
},
```

- [ ] **Step 2: Update `kind()` method**

Find the `kind()` impl on `Principal` and add:

```rust
Principal::Mattermost { .. } => "mattermost",
Principal::Teams { .. }      => "teams",
```

- [ ] **Step 3: Update `display()` or `fmt::Display` impl**

Find how `Principal::Slack` is displayed and add parallel arms for Mattermost and Teams
(use `user_name` if present, fall back to `user_id`).

- [ ] **Step 4: Build and run tests**

```
cargo nextest run -p fabro-types
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/principal.rs
git commit -m "feat(types): add Principal::Mattermost and Principal::Teams"
```

---

### Task 3: Add Mattermost/Teams to run settings in `fabro-types`

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/run.rs`

Read the file to find `NotificationRouteSettings` and `RunInterviewsSettings` and the existing `slack` fields.

- [ ] **Step 1: Add `MattermostNotificationRouteSettings` and `TeamsNotificationRouteSettings`**

Mirror the pattern of the existing Slack settings struct. Add after the Slack counterparts:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MattermostNotificationRouteSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamsNotificationRouteSettings {
    // placeholder for future fields
}
```

- [ ] **Step 2: Add fields to `NotificationRouteSettings`**

```rust
pub mattermost: Option<MattermostNotificationRouteSettings>,
pub teams:      Option<TeamsNotificationRouteSettings>,
```

- [ ] **Step 3: Add fields to `RunInterviewsSettings`**

```rust
pub mattermost: Option<MattermostInterviewsSettings>,
pub teams:      Option<TeamsInterviewsSettings>,
```

with corresponding empty structs mirroring the Slack pattern.

- [ ] **Step 4: Build and run tests**

```
cargo nextest run -p fabro-types
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-types/src/settings/run.rs
git commit -m "feat(types): add mattermost/teams fields to run notification and interview settings"
```

---

### Task 4: Add env var constants to `fabro-static`

**Files:**
- Modify: `lib/crates/fabro-static/src/env_vars.rs`

- [ ] **Step 1: Add three constants to `EnvVars`**

Find the `FABRO_SLACK_APP_TOKEN` / `FABRO_SLACK_BOT_TOKEN` declarations and add nearby
(alphabetical order):

```rust
pub const FABRO_MATTERMOST_TOKEN:          &'static str = "FABRO_MATTERMOST_TOKEN";
pub const FABRO_MATTERMOST_WEBHOOK_SECRET: &'static str = "FABRO_MATTERMOST_WEBHOOK_SECRET";
pub const FABRO_TEAMS_WEBHOOK_SECRET:      &'static str = "FABRO_TEAMS_WEBHOOK_SECRET";
```

- [ ] **Step 2: Add them to `OPTIONAL_VAULT_SECRETS`**

Find the array where `FABRO_SLACK_APP_TOKEN` and `FABRO_SLACK_BOT_TOKEN` appear and add:

```rust
EnvVars::FABRO_MATTERMOST_TOKEN,
EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET,
EnvVars::FABRO_TEAMS_WEBHOOK_SECRET,
```

- [ ] **Step 3: Build and run tests**

```
cargo nextest run -p fabro-static
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add lib/crates/fabro-static/src/env_vars.rs
git commit -m "feat(static): add FABRO_MATTERMOST_TOKEN, FABRO_MATTERMOST_WEBHOOK_SECRET, FABRO_TEAMS_WEBHOOK_SECRET"
```

---

### Task 5: Add Mattermost/Teams to `fabro-config` layer + resolver

**Files:**
- Modify: `lib/crates/fabro-config/src/layers/server.rs`
- Modify: `lib/crates/fabro-config/src/resolve/server.rs`
- Modify: `lib/crates/fabro-config/src/layers/mod.rs` (if `SlackIntegrationLayer` is re-exported there)

- [ ] **Step 1: Add layer structs in `layers/server.rs`**

After `SlackIntegrationLayer`:

```rust
/// `[server.integrations.mattermost]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct MattermostIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:         Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url:             Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team:            Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_channel: Option<InterpString>,
}

/// `[server.integrations.teams]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct TeamsIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
```

- [ ] **Step 2: Add fields to `ServerIntegrationsLayer`**

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub mattermost: Option<MattermostIntegrationLayer>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub teams:      Option<TeamsIntegrationLayer>,
```

- [ ] **Step 3: Re-export new types in `layers/mod.rs`**

Add `MattermostIntegrationLayer` and `TeamsIntegrationLayer` to the `pub use` list in `mod.rs`
if `SlackIntegrationLayer` is listed there.

- [ ] **Step 4: Add resolver functions in `resolve/server.rs`**

Add after the Slack resolver section:

```rust
fn resolve_mattermost(layer: Option<&MattermostIntegrationLayer>) -> MattermostIntegrationSettings {
    match layer {
        None => MattermostIntegrationSettings {
            enabled:         false,
            url:             None,
            team:            None,
            default_channel: None,
        },
        Some(mm) => MattermostIntegrationSettings {
            enabled:         mm.enabled.unwrap_or(true),
            url:             mm.url.clone(),
            team:            mm.team.clone(),
            default_channel: mm.default_channel.clone(),
        },
    }
}

fn resolve_teams(layer: Option<&TeamsIntegrationLayer>) -> TeamsIntegrationSettings {
    TeamsIntegrationSettings {
        enabled: layer.and_then(|t| t.enabled).unwrap_or(true),
    }
}
```

You will also need to add imports for `MattermostIntegrationSettings` and `TeamsIntegrationSettings`
at the top of `resolve/server.rs`.

- [ ] **Step 5: Wire new resolvers into `resolve_integrations`**

Find `resolve_integrations` and update the struct literal:

```rust
ServerIntegrationsSettings {
    github:     resolve_github(...),
    slack:      resolve_slack(...),
    mattermost: resolve_mattermost(layer.and_then(|i| i.mattermost.as_ref())),
    teams:      resolve_teams(layer.and_then(|i| i.teams.as_ref())),
}
```

- [ ] **Step 6: Build and run tests**

```
cargo nextest run -p fabro-config
```

Expected: PASS. If there are snapshot tests for `resolve_server`, update them:
```
cargo insta pending-snapshots
cargo insta accept
```
Verify the diff shows only the expected new `mattermost` and `teams` fields with correct defaults.

- [ ] **Step 7: Commit**

```bash
git add lib/crates/fabro-config/src/layers/server.rs \
        lib/crates/fabro-config/src/layers/mod.rs \
        lib/crates/fabro-config/src/resolve/server.rs
git commit -m "feat(config): add mattermost and teams integration config layer and resolver"
```

---

### Task 6: Add `mattermost` to `Principal` in OpenAPI spec + regenerate clients

**Files:**
- Modify: `docs/public/api-reference/fabro-api.yaml`

- [ ] **Step 1: Add `mattermost` and `teams` to the `Principal` discriminator**

Find the `Principal` oneOf block in `fabro-api.yaml`. It should have a `slack` entry. Add parallel entries:

```yaml
- title: MattermostPrincipal
  type: object
  required: [kind, team_id, user_id]
  properties:
    kind:
      type: string
      enum: [mattermost]
    team_id:
      type: string
    user_id:
      type: string
    user_name:
      type: string
      nullable: true
  additionalProperties: false

- title: TeamsPrincipal
  type: object
  required: [kind, tenant_id, user_id]
  properties:
    kind:
      type: string
      enum: [teams]
    tenant_id:
      type: string
    user_id:
      type: string
    user_name:
      type: string
      nullable: true
  additionalProperties: false
```

Also add `mattermost` and `teams` to the `discriminator.mapping` if one is present.

- [ ] **Step 2: Regenerate Rust types**

```
cargo build -p fabro-api
```

Expected: PASS. The progenitor-generated types will include `Mattermost` and `Teams` variants.

- [ ] **Step 3: Add `with_replacement` entries in `fabro-api/build.rs` if needed**

Open `lib/crates/fabro-api/build.rs`. If there is a `with_replacement` call for `Principal`,
ensure it still maps correctly. The `Principal::Mattermost` and `Principal::Teams` variants in
`fabro-types` are now the canonical types; verify they match the generated schema shape.

- [ ] **Step 4: Regenerate TypeScript client**

```
cd lib/packages/fabro-api-client && bun run generate
```

Expected: PASS.

- [ ] **Step 5: Run conformance tests**

```
cargo nextest run -p fabro-server -- openapi_conformance
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add docs/public/api-reference/fabro-api.yaml \
        lib/crates/fabro-api/build.rs \
        lib/packages/fabro-api-client/src/
git commit -m "feat(api): add Principal::Mattermost and Principal::Teams to OpenAPI spec"
```

---

### Task 7: Create `fabro-mattermost` crate skeleton

**Files:**
- Create: `lib/crates/fabro-mattermost/Cargo.toml`
- Create: `lib/crates/fabro-mattermost/src/lib.rs`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name    = "fabro-mattermost"
version.workspace = true
edition.workspace = true

[dependencies]
async-trait.workspace = true
bytes.workspace = true
chrono.workspace = true
fabro-chat    = { path = "../fabro-chat" }
fabro-http.workspace = true
fabro-interview = { path = "../fabro-interview" }
fabro-static  = { path = "../fabro-static" }
fabro-types   = { path = "../fabro-types" }
futures.workspace = true
http = "1"
parking_lot.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tokio-tungstenite.workspace = true
tracing.workspace = true

[dev-dependencies]
fabro-chat  = { path = "../fabro-chat", features = ["test-support"] }
fabro-test  = { path = "../fabro-test" }
insta.workspace = true
tokio = { workspace = true, features = ["test-util"] }
```

> **Note:** Check if `tokio-tungstenite` is already in the workspace `Cargo.toml` — look at how `fabro-slack` references it. If not, add it to the workspace manifest first.

- [ ] **Step 2: Create `src/lib.rs` with module declarations and credential types**

```rust
pub mod attachments;
pub mod client;
pub mod connection;
pub mod dispatch;
pub mod service;
pub mod threads;
pub mod webhook;

pub use service::MattermostService;

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

pub fn resolve_credentials_status_with_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> MattermostCredentialResolution {
    let token   = non_empty(lookup(EnvVars::FABRO_MATTERMOST_TOKEN));
    let secret  = non_empty(lookup(EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET));
    match (token, secret) {
        (Some(token), Some(webhook_secret)) => {
            MattermostCredentialResolution::Configured(MattermostCredentials {
                token,
                webhook_secret,
            })
        }
        (token, secret) => {
            let mut env_vars = Vec::new();
            if token.is_none() { env_vars.push(EnvVars::FABRO_MATTERMOST_TOKEN); }
            if secret.is_none() { env_vars.push(EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET); }
            MattermostCredentialResolution::Missing { env_vars }
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_when_both_absent() {
        let result = resolve_credentials_status_with_lookup(|_| None);
        assert!(matches!(result, MattermostCredentialResolution::Missing { .. }));
    }

    #[test]
    fn missing_when_only_token_present() {
        let result = resolve_credentials_status_with_lookup(|name| {
            if name == EnvVars::FABRO_MATTERMOST_TOKEN { Some("tok".into()) } else { None }
        });
        match result {
            MattermostCredentialResolution::Missing { env_vars } => {
                assert!(env_vars.contains(&EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET));
                assert!(!env_vars.contains(&EnvVars::FABRO_MATTERMOST_TOKEN));
            }
            _ => panic!("expected Missing"),
        }
    }

    #[test]
    fn configured_when_both_present() {
        let result = resolve_credentials_status_with_lookup(|name| match name {
            n if n == EnvVars::FABRO_MATTERMOST_TOKEN          => Some("tok".into()),
            n if n == EnvVars::FABRO_MATTERMOST_WEBHOOK_SECRET => Some("sec".into()),
            _ => None,
        });
        assert!(matches!(result, MattermostCredentialResolution::Configured(_)));
    }

    #[test]
    fn empty_string_treated_as_missing() {
        let result = resolve_credentials_status_with_lookup(|_| Some(String::new()));
        assert!(matches!(result, MattermostCredentialResolution::Missing { .. }));
    }
}
```

- [ ] **Step 3: Add placeholder module files so the crate compiles**

Create stub files for each module:

```
touch lib/crates/fabro-mattermost/src/attachments.rs
touch lib/crates/fabro-mattermost/src/client.rs
touch lib/crates/fabro-mattermost/src/connection.rs
touch lib/crates/fabro-mattermost/src/dispatch.rs
touch lib/crates/fabro-mattermost/src/service.rs
touch lib/crates/fabro-mattermost/src/threads.rs
touch lib/crates/fabro-mattermost/src/webhook.rs
```

In each, add an empty placeholder:

```rust
// placeholder
```

- [ ] **Step 4: Run crate tests**

```
cargo nextest run -p fabro-mattermost
```

Expected: PASS (credential resolution tests pass; other modules are empty).

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/
git commit -m "feat(mattermost): create crate skeleton with credential resolution"
```

---

### Task 8: Implement `threads.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/threads.rs`

Structural copy of `fabro-slack/src/threads.rs`. Read that file first for the exact implementation.

- [ ] **Step 1: Write a test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let reg = ThreadRegistry::new();
        reg.register("post_abc", "run-1", "q1");
        assert_eq!(reg.lookup("post_abc"), Some(("run-1".into(), "q1".into())));
    }

    #[test]
    fn remove_returns_none() {
        let reg = ThreadRegistry::new();
        reg.register("post_abc", "run-1", "q1");
        reg.remove("post_abc");
        assert_eq!(reg.lookup("post_abc"), None);
    }

    #[test]
    fn lookup_missing_returns_none() {
        let reg = ThreadRegistry::new();
        assert_eq!(reg.lookup("no_such_post"), None);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- threads
```

Expected: FAIL.

- [ ] **Step 3: Implement `ThreadRegistry`**

Copy `fabro-slack/src/threads.rs` into `fabro-mattermost/src/threads.rs`, renaming references
from Slack-specific terminology to be provider-neutral (the type name is the same,
`ThreadRegistry`, but the key is `post_id` instead of `ts`).

```rust
use std::collections::HashMap;
use std::sync::Mutex;

pub struct ThreadRegistry {
    inner: Mutex<HashMap<String, (String, String)>>,  // post_id → (run_id, qid)
}

impl ThreadRegistry {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub fn register(&self, post_id: &str, run_id: &str, qid: &str) {
        self.inner
            .lock()
            .expect("thread registry lock poisoned")
            .insert(post_id.to_string(), (run_id.to_string(), qid.to_string()));
    }

    pub fn lookup(&self, post_id: &str) -> Option<(String, String)> {
        self.inner
            .lock()
            .expect("thread registry lock poisoned")
            .get(post_id)
            .cloned()
    }

    pub fn remove(&self, post_id: &str) {
        self.inner
            .lock()
            .expect("thread registry lock poisoned")
            .remove(post_id);
    }
}
```

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- threads
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/threads.rs
git commit -m "feat(mattermost): implement ThreadRegistry"
```

---

### Task 9: Implement `dispatch.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/dispatch.rs`

- [ ] **Step 1: Write dispatch tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_event_is_connected() {
        let msg = r#"{"event":"hello"}"#;
        let action = dispatch_raw(msg);
        assert!(matches!(action, DispatchAction::Connected));
    }

    #[test]
    fn goodbye_event_is_reconnect() {
        let msg = r#"{"event":"goodbye"}"#;
        assert!(matches!(dispatch_raw(msg), DispatchAction::Reconnect));
    }

    #[test]
    fn unknown_event_is_ignored() {
        let msg = r#"{"event":"status_change"}"#;
        assert!(matches!(dispatch_raw(msg), DispatchAction::Ignored));
    }

    #[test]
    fn malformed_json_is_ignored() {
        assert!(matches!(dispatch_raw("{not json}"), DispatchAction::Ignored));
    }

    #[test]
    fn posted_with_unregistered_root_is_ignored() {
        let registry = crate::threads::ThreadRegistry::new();
        let msg = r#"{"event":"posted","data":{"post":"{\"id\":\"p1\",\"type\":\"\",\"root_id\":\"r1\",\"user_id\":\"u1\",\"message\":\"hello\"}"}}"#;
        assert!(matches!(dispatch(&registry, msg), DispatchAction::Ignored));
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- dispatch
```

Expected: FAIL.

- [ ] **Step 3: Implement `dispatch.rs`**

```rust
use fabro_chat::AnswerSubmission;
use fabro_interview::Answer;
use fabro_types::Principal;
use serde::Deserialize;

use crate::threads::ThreadRegistry;

pub enum DispatchAction {
    Connected,
    SubmitAnswer(Box<AnswerSubmission>),
    Reconnect,
    Ignored,
}

#[derive(Deserialize)]
struct WsEnvelope {
    event: Option<String>,
    data:  Option<WsData>,
}

#[derive(Deserialize)]
struct WsData {
    post:    Option<String>,  // JSON-encoded post object
    team_id: Option<String>,
}

#[derive(Deserialize)]
struct WsPost {
    id:      String,
    #[serde(rename = "type")]
    kind:    String,
    root_id: Option<String>,
    user_id: String,
    #[serde(default)]
    message: String,
}

pub fn dispatch(registry: &ThreadRegistry, raw: &str) -> DispatchAction {
    dispatch_raw_with_registry(Some(registry), raw)
}

pub fn dispatch_raw(raw: &str) -> DispatchAction {
    dispatch_raw_with_registry(None, raw)
}

fn dispatch_raw_with_registry(registry: Option<&ThreadRegistry>, raw: &str) -> DispatchAction {
    let Ok(envelope) = serde_json::from_str::<WsEnvelope>(raw) else {
        return DispatchAction::Ignored;
    };
    match envelope.event.as_deref() {
        Some("hello")   => DispatchAction::Connected,
        Some("goodbye") => DispatchAction::Reconnect,
        Some("posted")  => {
            let Some(data) = &envelope.data else { return DispatchAction::Ignored };
            let Some(post_json) = &data.post else { return DispatchAction::Ignored };
            let Ok(post) = serde_json::from_str::<WsPost>(post_json) else {
                return DispatchAction::Ignored;
            };
            // ignore system messages
            if !post.kind.is_empty() { return DispatchAction::Ignored; }
            let root_id = post.root_id.as_deref().unwrap_or("");
            let registry = registry?;  // None → Ignored (test path without registry)
            let (run_id, qid) = registry.lookup(root_id)?;
            let actor = Principal::Mattermost {
                team_id:   data.team_id.clone().unwrap_or_default(),
                user_id:   post.user_id.clone(),
                user_name: None,
            };
            let answer = Answer::Freeform(post.message.clone());
            Some(DispatchAction::SubmitAnswer(Box::new(AnswerSubmission {
                run_id,
                qid,
                answer,
                actor,
            })))
        }
        _ => DispatchAction::Ignored,
    }
}
```

> **Note:** The `?` on `Option<&ThreadRegistry>` returns `None` from the function body, which
> maps to `DispatchAction::Ignored` via the `Some(DispatchAction::SubmitAnswer(...))` return
> path — you'll need to restructure the `Some("posted")` arm to avoid this. Simplest fix:
> check `registry.is_none()` early and return `Ignored`.

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- dispatch
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/dispatch.rs
git commit -m "feat(mattermost): implement WS event dispatch"
```

---

### Task 10: Implement `webhook.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/webhook.rs`

- [ ] **Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";

    fn yes_payload() -> serde_json::Value {
        serde_json::json!({
            "channel_id": "c1",
            "team_id":    "t1",
            "user_id":    "u1",
            "user_name":  "alice",
            "context": { "run_id": "r1", "qid": "q1", "kind": "yes" }
        })
    }

    fn no_payload() -> serde_json::Value {
        serde_json::json!({
            "channel_id": "c1",
            "team_id":    "t1",
            "user_id":    "u1",
            "user_name":  "alice",
            "context": { "run_id": "r1", "qid": "q1", "kind": "no" }
        })
    }

    fn selected_payload() -> serde_json::Value {
        serde_json::json!({
            "channel_id": "c1",
            "team_id":    "t1",
            "user_id":    "u1",
            "user_name":  "alice",
            "context": { "run_id": "r1", "qid": "q1", "kind": "selected", "key": "opt_a" }
        })
    }

    #[test]
    fn verify_correct_token() {
        assert!(verify_token(SECRET, SECRET));
    }

    #[test]
    fn verify_wrong_token_fails() {
        assert!(!verify_token(SECRET, "wrong"));
    }

    #[test]
    fn verify_different_length_fails() {
        assert!(!verify_token(SECRET, "x"));
    }

    #[test]
    fn parse_yes() {
        let sub = parse_action(&yes_payload()).unwrap();
        use fabro_interview::Answer;
        assert_eq!(sub.run_id, "r1");
        assert_eq!(sub.qid, "q1");
        assert!(matches!(sub.answer, Answer::Yes));
    }

    #[test]
    fn parse_no() {
        let sub = parse_action(&no_payload()).unwrap();
        use fabro_interview::Answer;
        assert!(matches!(sub.answer, Answer::No));
    }

    #[test]
    fn parse_selected() {
        let sub = parse_action(&selected_payload()).unwrap();
        use fabro_interview::Answer;
        match sub.answer {
            Answer::Selected { key } => assert_eq!(key, "opt_a"),
            _ => panic!("expected Selected"),
        }
    }

    #[test]
    fn parse_unknown_kind_returns_none() {
        let mut payload = yes_payload();
        payload["context"]["kind"] = serde_json::json!("unknown");
        assert!(parse_action(&payload).is_none());
    }

    #[test]
    fn parse_missing_context_returns_none() {
        let payload = serde_json::json!({
            "channel_id": "c1",
            "team_id":    "t1",
            "user_id":    "u1"
        });
        assert!(parse_action(&payload).is_none());
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- webhook
```

Expected: FAIL.

- [ ] **Step 3: Implement `webhook.rs`**

```rust
use fabro_chat::AnswerSubmission;
use fabro_interview::Answer;
use fabro_types::Principal;
use serde::Deserialize;

#[derive(Deserialize)]
struct ActionPayload {
    team_id:   Option<String>,
    user_id:   Option<String>,
    user_name: Option<String>,
    context:   Option<ActionContext>,
}

#[derive(Deserialize)]
struct ActionContext {
    run_id: Option<String>,
    qid:    Option<String>,
    kind:   Option<String>,
    key:    Option<String>,
}

pub fn parse_action(payload: &serde_json::Value) -> Option<AnswerSubmission> {
    let p: ActionPayload = serde_json::from_value(payload.clone()).ok()?;
    let ctx = p.context.as_ref()?;
    let run_id = ctx.run_id.clone()?;
    let qid    = ctx.qid.clone()?;
    let answer = match ctx.kind.as_deref()? {
        "yes"      => Answer::Yes,
        "no"       => Answer::No,
        "selected" => Answer::Selected { key: ctx.key.clone().unwrap_or_default() },
        _          => return None,
    };
    let actor = Principal::Mattermost {
        team_id:   p.team_id.unwrap_or_default(),
        user_id:   p.user_id.unwrap_or_default(),
        user_name: p.user_name,
    };
    Some(AnswerSubmission { run_id, qid, answer, actor })
}

/// Constant-time byte comparison. Returns `false` for different lengths.
pub fn verify_token(expected: &str, provided: &str) -> bool {
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn handle_webhook(
    query_string: &str,
    body: &bytes::Bytes,
    webhook_secret: &str,
) -> fabro_chat::WebhookOutcome {
    let token = extract_query_param(query_string, "token").unwrap_or_default();
    if !verify_token(webhook_secret, &token) {
        return fabro_chat::WebhookOutcome { status: 401, body: None };
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
        return fabro_chat::WebhookOutcome { status: 400, body: None };
    };
    (payload, fabro_chat::WebhookOutcome { status: 200, body: None })
}

fn extract_query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let val = parts.next().unwrap_or("");
        if key == name {
            return Some(val.to_string());
        }
    }
    None
}
```

> **Note:** `handle_webhook` currently returns a tuple `(payload, outcome)` so the caller can
> act on the payload. Adjust the return type to what `service.rs` needs — probably just return
> `Option<(AnswerSubmission, WebhookOutcome)>` or separate the token check from the parse step.
> Design this so `service.rs` can call `on_submit` after `handle_webhook` returns.

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- webhook
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/webhook.rs
git commit -m "feat(mattermost): implement webhook token verification and action parsing"
```

---

### Task 11: Implement `attachments.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/attachments.rs`

All assertions use `insta` inline JSON snapshots.

- [ ] **Step 1: Write snapshot tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fabro_interview::{Question, QuestionType};

    fn yes_no_question() -> Question {
        Question {
            id:              "q1".into(),
            text:            "Deploy to production?".into(),
            question_type:   QuestionType::YesNo,
            options:         vec![],
            allow_freeform:  false,
            timeout_seconds: None,
            context_display: None,
        }
    }

    fn freeform_question() -> Question {
        Question {
            id:              "q2".into(),
            text:            "Describe the change".into(),
            question_type:   QuestionType::Freeform,
            options:         vec![],
            allow_freeform:  true,
            timeout_seconds: None,
            context_display: Some("See PR #123".into()),
        }
    }

    #[test]
    fn yes_no_question_snapshot() {
        let attachments = question_to_attachments(
            "run-abc",
            "q1",
            &yes_no_question(),
            Some("https://app.fabro.sh/runs/run-abc"),
            "https://fabro.sh/api/v1/webhooks/mattermost?token=sec",
        );
        insta::assert_json_snapshot!(attachments);
    }

    #[test]
    fn freeform_question_snapshot() {
        let attachments = question_to_attachments(
            "run-abc",
            "q2",
            &freeform_question(),
            None,
            "https://fabro.sh/api/v1/webhooks/mattermost?token=sec",
        );
        insta::assert_json_snapshot!(attachments);
    }

    #[test]
    fn lifecycle_started_snapshot() {
        use crate::RunLifecycleKind;
        let attachments = run_lifecycle_attachments(
            RunLifecycleKind::Started,
            &RunLifecycleDetails {
                run_id:         "run-abc",
                run_url:        Some("https://app.fabro.sh/runs/run-abc"),
                workflow_label: "deploy-prod",
                result:         None,
                duration_ms:    None,
                pull_request:   None,
            },
        );
        insta::assert_json_snapshot!(attachments);
    }

    #[test]
    fn answered_attachments_snapshot() {
        let attachments = answered_attachments("Deploy to production?", "yes");
        insta::assert_json_snapshot!(attachments);
    }

    #[test]
    fn title_truncated_at_200_chars() {
        let long_text: String = "x".repeat(250);
        let q = Question {
            text: long_text.clone(),
            ..yes_no_question()
        };
        let attachments = question_to_attachments("r", "q", &q, None, "http://h/w");
        let title = attachments[0]["title"].as_str().unwrap();
        assert_eq!(title.len(), 200);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- attachments
```

Expected: FAIL.

- [ ] **Step 3: Implement `attachments.rs`**

```rust
use fabro_interview::{Answer, Question, QuestionType};
use serde_json::{json, Value};

pub enum RunLifecycleKind { Started, Completed, Failed }

pub struct RunLifecycleDetails<'a> {
    pub run_id:         &'a str,
    pub run_url:        Option<&'a str>,
    pub workflow_label: &'a str,
    pub result:         Option<&'a str>,
    pub duration_ms:    Option<u64>,
    pub pull_request:   Option<PullRequestDetails<'a>>,
}

pub struct PullRequestDetails<'a> {
    pub number: u64,
    pub title:  Option<&'a str>,
    pub url:    Option<&'a str>,
}

const MAX_TITLE_CHARS: usize = 200;
const MAX_TEXT_CHARS: usize  = 8000;
const COLOR_BLUE:  &str = "#0072C6";
const COLOR_GREEN: &str = "#36a64f";
const COLOR_RED:   &str = "#cc0000";

pub fn question_to_attachments(
    run_id:      &str,
    qid:         &str,
    question:    &Question,
    run_web_url: Option<&str>,
    webhook_url: &str,
) -> Vec<Value> {
    let title: String = question.text.chars().take(MAX_TITLE_CHARS).collect();
    let text: Option<String> = question
        .context_display
        .as_deref()
        .map(|s| s.chars().take(MAX_TEXT_CHARS).collect());

    let actions: Vec<Value> = match question.question_type {
        QuestionType::YesNo => {
            vec![
                button("Yes", run_id, qid, "yes", None, webhook_url),
                button("No",  run_id, qid, "no",  None, webhook_url),
            ]
        }
        QuestionType::MultipleChoice => {
            question
                .options
                .iter()
                .map(|opt| button(&opt.label, run_id, qid, "selected", Some(&opt.key), webhook_url))
                .collect()
        }
        QuestionType::Freeform | QuestionType::MultiSelect | _ => vec![],
    };

    let mut attachment = json!({
        "color": COLOR_BLUE,
        "title": title,
    });
    if let Some(t) = text { attachment["text"] = json!(t); }
    if let Some(url) = run_web_url { attachment["title_link"] = json!(url); }
    if !actions.is_empty() { attachment["actions"] = json!(actions); }
    vec![attachment]
}

fn button(
    name:        &str,
    run_id:      &str,
    qid:         &str,
    kind:        &str,
    key:         Option<&str>,
    webhook_url: &str,
) -> Value {
    let mut context = json!({ "run_id": run_id, "qid": qid, "kind": kind });
    if let Some(k) = key { context["key"] = json!(k); }
    json!({
        "name": name,
        "integration": { "url": webhook_url, "context": context }
    })
}

pub fn run_lifecycle_attachments(kind: RunLifecycleKind, details: &RunLifecycleDetails<'_>) -> Vec<Value> {
    let color = match kind {
        RunLifecycleKind::Failed    => COLOR_RED,
        RunLifecycleKind::Started | RunLifecycleKind::Completed => COLOR_GREEN,
    };
    let event_label = match kind {
        RunLifecycleKind::Started   => "Run started",
        RunLifecycleKind::Completed => "Run completed",
        RunLifecycleKind::Failed    => "Run failed",
    };
    let title = format!("{} — {}", event_label, details.workflow_label);
    let mut lines = vec![format!("Run `{}`", details.run_id)];
    if let Some(url)      = details.run_url     { lines.push(format!("[View run]({})", url)); }
    if let Some(result)   = details.result      { lines.push(format!("Result: {}", result)); }
    if let Some(ms)       = details.duration_ms { lines.push(format!("Duration: {}s", ms / 1000)); }
    if let Some(pr)       = &details.pull_request {
        let pr_text = match (pr.title, pr.url) {
            (Some(t), Some(u)) => format!("PR [#{} {}]({})", pr.number, t, u),
            (None,    Some(u)) => format!("PR [#{}]({})", pr.number, u),
            _                  => format!("PR #{}", pr.number),
        };
        lines.push(pr_text);
    }
    vec![json!({
        "color": color,
        "title": title,
        "text":  lines.join("\n"),
    })]
}

pub fn answered_attachments(question_text: &str, answer_text: &str) -> Vec<Value> {
    vec![json!({
        "color": COLOR_BLUE,
        "title": question_text.chars().take(MAX_TITLE_CHARS).collect::<String>(),
        "text":  answer_text,
    })]
}
```

- [ ] **Step 4: Run tests and accept snapshots**

```
cargo nextest run -p fabro-mattermost -- attachments
cargo insta pending-snapshots
```

Review each pending snapshot — it should show the JSON attachment structures. Accept:

```
cargo insta accept
```

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/attachments.rs \
        lib/crates/fabro-mattermost/src/snapshots/
git commit -m "feat(mattermost): implement attachment builders with insta snapshots"
```

---

### Task 12: Implement `client.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/client.rs`

- [ ] **Step 1: Write unit tests for response parsing and channel cache**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_post_response() {
        let json = serde_json::json!({ "id": "post123", "channel_id": "chan456" });
        let msg: PostedMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.post_id, "post123");
        assert_eq!(msg.channel_id, "chan456");
    }

    #[test]
    fn wss_url_from_https() {
        let client = MattermostClient::for_testing("tok", "https://mm.example.com");
        assert_eq!(
            client.wss_url(),
            "wss://mm.example.com/api/v4/websocket"
        );
    }

    #[test]
    fn wss_url_from_http() {
        let client = MattermostClient::for_testing("tok", "http://localhost:8065");
        assert_eq!(
            client.wss_url(),
            "ws://localhost:8065/api/v4/websocket"
        );
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- client
```

Expected: FAIL.

- [ ] **Step 3: Implement `client.rs`**

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

pub struct MattermostClient {
    token:         String,
    base_url:      String,
    http:          fabro_http::HttpClient,
    channel_cache: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostedMessage {
    #[serde(rename = "id")]
    pub post_id:    String,
    pub channel_id: String,
}

#[derive(Serialize)]
struct PostBody<'a> {
    channel_id: &'a str,
    message:    &'a str,
    props:      PostProps<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_id:    Option<&'a str>,
}

#[derive(Serialize)]
struct PostProps<'a> {
    attachments: &'a [serde_json::Value],
}

impl MattermostClient {
    pub fn new(token: String, base_url: String) -> Self {
        Self {
            token,
            base_url: base_url.trim_end_matches('/').to_string(),
            http: fabro_http::http_client().expect("Mattermost HTTP client should build"),
            channel_cache: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_testing(token: &str, base_url: &str) -> Self {
        Self::new(token.to_string(), base_url.to_string())
    }

    pub fn wss_url(&self) -> String {
        let base = self.base_url.as_str();
        if base.starts_with("https://") {
            format!("wss://{}/api/v4/websocket", &base[8..])
        } else if base.starts_with("http://") {
            format!("ws://{}/api/v4/websocket", &base[7..])
        } else {
            format!("wss://{}/api/v4/websocket", base)
        }
    }

    pub async fn post_message(
        &self,
        channel_id: &str,
        message: &str,
        attachments: &[serde_json::Value],
        root_id: Option<&str>,
    ) -> anyhow::Result<PostedMessage> {
        let body = PostBody { channel_id, message, props: PostProps { attachments }, root_id };
        let response = self
            .http
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Mattermost post_message failed: {} — {}", status, text);
        }
        Ok(response.json::<PostedMessage>().await?)
    }

    pub async fn update_post(
        &self,
        post_id: &str,
        message: &str,
        attachments: &[serde_json::Value],
    ) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct UpdateBody<'a> {
            id:      &'a str,
            message: &'a str,
            props:   PostProps<'a>,
        }
        let body = UpdateBody { id: post_id, message, props: PostProps { attachments } };
        let response = self
            .http
            .put(format!("{}/api/v4/posts/{}", self.base_url, post_id))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Mattermost update_post failed: {} — {}", status, text);
        }
        Ok(())
    }

    pub async fn resolve_channel(&self, team: &str, channel_name: &str) -> anyhow::Result<String> {
        let cache_key = format!("{}/{}", team, channel_name);
        {
            let cache = self.channel_cache.lock().expect("channel cache lock poisoned");
            if let Some(id) = cache.get(&cache_key) {
                return Ok(id.clone());
            }
        }
        #[derive(Deserialize)]
        struct ChannelResponse { id: String }
        let response = self
            .http
            .get(format!(
                "{}/api/v4/teams/name/{}/channels/name/{}",
                self.base_url, team, channel_name
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("resolve_channel failed: {}", status);
        }
        let channel: ChannelResponse = response.json().await?;
        self.channel_cache
            .lock()
            .expect("channel cache lock poisoned")
            .insert(cache_key, channel.id.clone());
        Ok(channel.id)
    }
}
```

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- client
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/client.rs
git commit -m "feat(mattermost): implement REST client"
```

---

### Task 13: Implement `connection.rs`

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/connection.rs`

Structural parallel to `fabro-slack/src/connection.rs`. Read that file first for the backoff
and reconnect pattern.

- [ ] **Step 1: Write unit tests for `process_message` and `wss_url_from_base`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::ThreadRegistry;
    use crate::dispatch::DispatchAction;

    #[test]
    fn hello_updates_status_connected() {
        let status_calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let calls_clone = status_calls.clone();
        let sink: ConnectionStatusSink = std::sync::Arc::new(move |update| {
            calls_clone.lock().unwrap().push(update);
        });
        let registry = ThreadRegistry::new();
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        process_message(r#"{"event":"hello"}"#, &registry, &sink);
        let calls = status_calls.lock().unwrap();
        assert!(matches!(calls.last(), Some(ConnectionStatusUpdate::Connected)));
    }

    #[test]
    fn auth_message_format() {
        let msg = auth_message("my-token");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["action"], "authentication_challenge");
        assert_eq!(v["data"]["token"], "my-token");
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- connection
```

Expected: FAIL.

- [ ] **Step 3: Implement `connection.rs`**

```rust
use std::sync::Arc;
use std::time::Duration;

use fabro_chat::AnswerSubmission;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::client::MattermostClient;
use crate::dispatch::{self, DispatchAction};
use crate::threads::ThreadRegistry;

pub type ConnectionStatusSink = Arc<dyn Fn(ConnectionStatusUpdate) + Send + Sync>;

#[derive(Clone)]
pub enum ConnectionStatusUpdate {
    Connecting,
    Connected,
    Error(String),
}

const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS:     u64 = 30_000;

pub fn auth_message(token: &str) -> String {
    serde_json::json!({
        "seq":    1,
        "action": "authentication_challenge",
        "data":   { "token": token }
    })
    .to_string()
}

pub fn process_message(
    raw: &str,
    registry: &ThreadRegistry,
    status_sink: &ConnectionStatusSink,
) -> Option<AnswerSubmission> {
    match dispatch::dispatch(registry, raw) {
        DispatchAction::Connected => {
            status_sink(ConnectionStatusUpdate::Connected);
            None
        }
        DispatchAction::SubmitAnswer(sub) => Some(*sub),
        DispatchAction::Reconnect => None,   // caller handles
        DispatchAction::Ignored   => None,
    }
}

pub async fn run_with_status(
    client: &MattermostClient,
    token: &str,
    registry: &Arc<ThreadRegistry>,
    on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>,
    status_sink: ConnectionStatusSink,
) {
    let mut backoff_ms = INITIAL_BACKOFF_MS;
    let wss_url = client.wss_url();

    loop {
        status_sink(ConnectionStatusUpdate::Connecting);
        match connect_async(&wss_url).await {
            Err(err) => {
                warn!(error = %err, "Mattermost WebSocket connect failed; retrying");
                status_sink(ConnectionStatusUpdate::Error(err.to_string()));
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                continue;
            }
            Ok((mut ws, _)) => {
                use futures::{SinkExt, StreamExt};
                backoff_ms = INITIAL_BACKOFF_MS;
                if let Err(err) = ws.send(Message::Text(auth_message(token))).await {
                    warn!(error = %err, "Mattermost auth message failed");
                    continue;
                }
                while let Some(msg) = ws.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let DispatchAction::Reconnect = dispatch::dispatch(registry, &text) {
                                info!("Mattermost goodbye received; reconnecting");
                                break;
                            }
                            if let Some(submission) = process_message(&text, registry, &status_sink) {
                                on_submit(submission);
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            let _ = ws.send(Message::Pong(payload)).await;
                        }
                        Ok(Message::Close(_)) => break,
                        Err(err) => {
                            warn!(error = %err, "Mattermost WebSocket error");
                            status_sink(ConnectionStatusUpdate::Error(err.to_string()));
                            break;
                        }
                        _ => {}
                    }
                }
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- connection
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-mattermost/src/connection.rs
git commit -m "feat(mattermost): implement WebSocket inbound loop with reconnect"
```

---

### Task 14: Implement `service.rs` (`MattermostService` + `impl ChatService`)

**Files:**
- Modify: `lib/crates/fabro-mattermost/src/service.rs`

- [ ] **Step 1: Write the contract test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fabro_chat::test_support::assert_chat_service_contract;

    fn test_service() -> MattermostService {
        MattermostService::new(
            "http://localhost:8065".into(),
            "test-token".into(),
            "test-team".into(),
            None,
            "webhook-secret".into(),
        )
    }

    #[tokio::test]
    async fn chat_service_contract_holds() {
        assert_chat_service_contract(&test_service()).await;
    }

    #[test]
    fn kind_is_mattermost() {
        use fabro_chat::ChatService;
        assert_eq!(test_service().kind(), fabro_chat::ChatProviderKind::Mattermost);
    }

    #[test]
    fn webhook_wrong_token_is_401() {
        use fabro_chat::ChatService;
        let svc = test_service();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(svc.handle_webhook(
            "",
            "token=wrong",
            &http::HeaderMap::new(),
            &bytes::Bytes::new(),
        ));
        assert_eq!(outcome.status, 401);
    }

    #[test]
    fn webhook_correct_token_empty_body_is_400() {
        use fabro_chat::ChatService;
        let svc = test_service();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(svc.handle_webhook(
            "",
            "token=webhook-secret",
            &http::HeaderMap::new(),
            &bytes::Bytes::from("{}"),
        ));
        assert_eq!(outcome.status, 200);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-mattermost -- service
```

Expected: FAIL.

- [ ] **Step 3: Implement `service.rs`**

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use fabro_chat::{
    AnswerSubmission, ChatEventContext, ChatProviderKind, ChatService, WebhookOutcome,
};
use fabro_types::{
    EventBody, EventEnvelope, IntegrationConnectionKind, IntegrationConnectionState,
    IntegrationConnectionStatus, RunId,
};
use http::HeaderMap;
use tracing::warn;

use crate::attachments::{
    self, RunLifecycleDetails, RunLifecycleKind, PullRequestDetails,
};
use crate::client::{MattermostClient, PostedMessage};
use crate::connection::{self, ConnectionStatusSink, ConnectionStatusUpdate};
use crate::threads::ThreadRegistry;
use crate::webhook;

#[derive(Default, Clone)]
struct ConnectionRuntimeState {
    status:            IntegrationConnectionState,
    last_connected_at: Option<chrono::DateTime<Utc>>,
    last_error:        Option<String>,
}

pub struct MattermostService {
    client:          MattermostClient,
    team:            String,
    default_channel: Option<String>,
    webhook_secret:  String,
    posted_messages: Arc<Mutex<HashMap<(RunId, String), PostedMessage>>>,
    thread_registry: Arc<ThreadRegistry>,
    connection:      Arc<Mutex<ConnectionRuntimeState>>,
    on_submit:       Arc<Mutex<Option<Arc<dyn Fn(AnswerSubmission) + Send + Sync>>>>,
}

impl MattermostService {
    pub fn new(
        base_url:        String,
        token:           String,
        team:            String,
        default_channel: Option<String>,
        webhook_secret:  String,
    ) -> Self {
        Self {
            client:          MattermostClient::new(token, base_url),
            team,
            default_channel,
            webhook_secret,
            posted_messages: Arc::new(Mutex::new(HashMap::new())),
            thread_registry: Arc::new(ThreadRegistry::new()),
            connection:      Arc::new(Mutex::new(ConnectionRuntimeState::default())),
            on_submit:       Arc::new(Mutex::new(None)),
        }
    }

    fn status_sink(&self) -> ConnectionStatusSink {
        let connection = Arc::clone(&self.connection);
        Arc::new(move |update: ConnectionStatusUpdate| {
            let mut state = connection.lock().expect("mm connection state lock poisoned");
            match update {
                ConnectionStatusUpdate::Connecting => {
                    state.status = IntegrationConnectionState::Connecting;
                    state.last_error = None;
                }
                ConnectionStatusUpdate::Connected => {
                    state.status = IntegrationConnectionState::Connected;
                    state.last_connected_at = Some(Utc::now());
                    state.last_error = None;
                }
                ConnectionStatusUpdate::Error(err) => {
                    state.status = IntegrationConnectionState::Error;
                    state.last_error = Some(err.chars().take(240).collect());
                }
            }
        })
    }

    async fn finish_interview(&self, run_id: RunId, qid: &str, question: &str, answer: &str) {
        let key = (run_id, qid.to_string());
        let posted = self
            .posted_messages
            .lock()
            .expect("mm posted messages lock poisoned")
            .remove(&key);
        let Some(posted) = posted else { return };
        self.thread_registry.remove(&posted.post_id);
        let attachment_json = attachments::answered_attachments(question, answer);
        let _ = self.client.update_post(&posted.post_id, "", &attachment_json).await;
    }
}

#[async_trait]
impl ChatService for MattermostService {
    fn kind(&self) -> ChatProviderKind { ChatProviderKind::Mattermost }

    async fn start(&self, on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>) {
        *self.on_submit.lock().expect("mm on_submit lock poisoned") = Some(Arc::clone(&on_submit));
        connection::run_with_status(
            &self.client,
            // token is held in client; expose a getter or clone it from MattermostService::new
            "",  // TODO: thread token through or expose client.token()
            &self.thread_registry,
            on_submit,
            self.status_sink(),
        )
        .await;
    }

    async fn handle_event(&self, envelope: &EventEnvelope, context: &dyn ChatEventContext) {
        let event = &envelope.event;
        match &event.body {
            EventBody::InterviewStarted(props) => {
                if props.question_id.is_empty() { return; }
                let Some(default_channel) = self.default_channel.as_deref() else { return };
                let key = (event.run_id, props.question_id.clone());
                if self.posted_messages.lock().expect("mm posted messages lock poisoned")
                    .contains_key(&key) { return; }
                let channel_id = match self.client.resolve_channel(&self.team, default_channel).await {
                    Ok(id) => id,
                    Err(err) => {
                        warn!(error = %err, "Mattermost: failed to resolve channel");
                        return;
                    }
                };
                let canonical = context.canonical_origin().unwrap_or_default();
                let webhook_url = format!(
                    "{}/api/v1/webhooks/mattermost?token={}",
                    canonical, self.webhook_secret
                );
                let run_url = context.run_web_url(&event.run_id);
                // Build question from props (copy pattern from SlackService::handle_event)
                use fabro_interview::Question;
                let question = Question {
                    id:              props.question_id.clone(),
                    text:            props.question.clone(),
                    question_type:   props.question_type.parse().unwrap_or_default(),
                    options:         props.options.clone(),
                    allow_freeform:  props.allow_freeform,
                    timeout_seconds: props.timeout_seconds,
                    context_display: props.context_display.clone(),
                };
                let attachment_json = attachments::question_to_attachments(
                    &event.run_id.to_string(),
                    &props.question_id,
                    &question,
                    run_url.as_deref(),
                    &webhook_url,
                );
                match self.client.post_message(&channel_id, "", &attachment_json, None).await {
                    Ok(posted) => {
                        if question.allow_freeform || question.question_type == fabro_types::QuestionType::Freeform {
                            self.thread_registry.register(&posted.post_id, &event.run_id.to_string(), &props.question_id);
                        }
                        self.posted_messages
                            .lock()
                            .expect("mm posted messages lock poisoned")
                            .insert(key, posted);
                    }
                    Err(err) => {
                        warn!(error = %err, "Mattermost: failed to post interview question");
                    }
                }
            }
            EventBody::InterviewCompleted(props) => {
                self.finish_interview(event.run_id, &props.question_id, &props.question, &props.answer).await;
            }
            EventBody::InterviewTimeout(props) => {
                self.finish_interview(event.run_id, &props.question_id, &props.question, "Timed out").await;
            }
            EventBody::InterviewInterrupted(props) => {
                self.finish_interview(event.run_id, &props.question_id, &props.question, "Interrupted").await;
            }
            EventBody::RunStarted(_) | EventBody::RunCompleted(_) | EventBody::RunFailed(_) => {
                self.handle_lifecycle_event(envelope, context).await;
            }
            _ => {}
        }
    }

    async fn handle_webhook(
        &self,
        _sub_path: &str,
        query_string: &str,
        _headers: &HeaderMap,
        body: &Bytes,
    ) -> WebhookOutcome {
        let token = extract_query_param(query_string, "token").unwrap_or_default();
        if !webhook::verify_token(&self.webhook_secret, &token) {
            return WebhookOutcome { status: 401, body: None };
        }
        let payload = match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(p)  => p,
            Err(_) => return WebhookOutcome { status: 400, body: None },
        };
        if let Some(submission) = webhook::parse_action(&payload) {
            let on_submit = self.on_submit.lock().expect("mm on_submit lock poisoned").clone();
            if let Some(cb) = on_submit { cb(submission); }
        }
        WebhookOutcome { status: 200, body: None }
    }

    fn connection_status(&self) -> IntegrationConnectionStatus {
        let state = self.connection.lock().expect("mm connection state lock poisoned").clone();
        IntegrationConnectionStatus {
            kind:              IntegrationConnectionKind::WebSocket,
            status:            state.status,
            last_connected_at: state.last_connected_at,
            last_error:        state.last_error,
        }
    }
}

impl MattermostService {
    async fn handle_lifecycle_event(&self, envelope: &EventEnvelope, context: &dyn ChatEventContext) {
        let event = &envelope.event;
        let event_name = event.body.event_name();
        let Some(projection) = context.get_run_projection(event.run_id).await else { return };

        let mut routes: Vec<_> = projection.spec.settings.run.notifications.iter()
            .filter(|(_, route)| {
                route.enabled
                    && route.provider.as_deref() == Some("mattermost")
                    && route.events.iter().any(|e| e == event_name)
            })
            .collect();
        if routes.is_empty() { return; }
        routes.sort_by_key(|(name, _)| *name);

        let (kind, result, duration_ms) = match &event.body {
            EventBody::RunStarted(_)       => (RunLifecycleKind::Started,   None,                       None),
            EventBody::RunCompleted(props) => (RunLifecycleKind::Completed, props.result.clone(),        props.duration_ms),
            EventBody::RunFailed(props)    => (RunLifecycleKind::Failed,    Some(props.error.clone()),   props.duration_ms),
            _                              => return,
        };

        let prior = context.get_run_events(event.run_id, envelope.seq).await;
        let (started_event_name, pull_request) = extract_prior_details(&prior);
        let workflow_label = [
            projection.spec.workflow_name(),
            projection.spec.workflow_slug(),
            started_event_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or(event_name)
        .to_string();

        let run_url = context.run_web_url(&event.run_id);
        let pr_details = pull_request.as_ref().map(|(number, title, url)| PullRequestDetails {
            number: *number,
            title:  title.as_deref(),
            url:    url.as_deref(),
        });
        let attachment_json = attachments::run_lifecycle_attachments(kind, &RunLifecycleDetails {
            run_id:         &event.run_id.to_string(),
            run_url:        run_url.as_deref(),
            workflow_label: &workflow_label,
            result:         result.as_deref(),
            duration_ms,
            pull_request:   pr_details,
        });

        for (route_name, route) in routes {
            let Some(mm_route) = route.mattermost.as_ref() else { continue };
            let Some(channel_name_interp) = mm_route.channel.as_ref() else { continue };
            let channel_name = match channel_name_interp.resolve(|n| context.resolve_env(n)) {
                Ok(r) => r.value,
                Err(err) => {
                    warn!(route = %route_name, error = %err, "Mattermost: unresolved channel");
                    continue;
                }
            };
            match self.client.resolve_channel(&self.team, &channel_name).await {
                Ok(channel_id) => {
                    if let Err(err) = self.client.post_message(&channel_id, "", &attachment_json, None).await {
                        warn!(route = %route_name, error = %err, "Mattermost: lifecycle post failed");
                    }
                }
                Err(err) => {
                    warn!(route = %route_name, error = %err, "Mattermost: channel resolve failed");
                }
            }
        }
    }
}

fn extract_prior_details(
    events: &[fabro_types::EventEnvelope],
) -> (Option<String>, Option<(u64, Option<String>, Option<String>)>) {
    let mut started_name = None;
    let mut pull_request = None;
    for envelope in events {
        match &envelope.event.body {
            EventBody::RunStarted(props) if !props.name.trim().is_empty() => {
                started_name = Some(props.name.clone());
            }
            EventBody::PullRequestCreated(props) => {
                pull_request = Some((props.pr_number, Some(props.title.clone()), Some(props.pr_url.clone())));
            }
            _ => {}
        }
    }
    (started_name, pull_request)
}

fn extract_query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == name { return Some(parts.next().unwrap_or("").to_string()); }
    }
    None
}
```

> **Key gap:** `start()` needs to pass the token to `run_with_status`. Expose
> `client.token: String` as a field (or add a `fn token(&self) -> &str` getter), and pass it
> as the second arg to `run_with_status`. Fix this before running tests.

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-mattermost -- service
```

Expected: PASS.

- [ ] **Step 5: Run full `fabro-mattermost` test suite**

```
cargo nextest run -p fabro-mattermost
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-mattermost/src/service.rs
git commit -m "feat(mattermost): implement MattermostService with ChatService"
```

---

### Task 15: Wire `MattermostService` into `fabro-server`

**Files:**
- Modify: `lib/crates/fabro-server/Cargo.toml`
- Modify: `lib/crates/fabro-server/src/server.rs`

- [ ] **Step 1: Add `fabro-mattermost` to `fabro-server` Cargo.toml**

```toml
fabro-mattermost = { path = "../fabro-mattermost" }
```

- [ ] **Step 2: Add `use` imports in `server.rs`**

```rust
use fabro_mattermost::{MattermostService, MattermostCredentialResolution,
                        resolve_credentials_status_with_lookup as resolve_mattermost_credentials};
```

- [ ] **Step 3: Extend `chat_services` construction in `build_app_state`**

In the block that builds `chat_services`, after the Slack section, add:

```rust
{
    let mm_settings = &current_server_settings.server.integrations.mattermost;
    if mm_settings.enabled {
        let vault_guard = vault.try_read().ok();
        match resolve_mattermost_credentials(|name| {
            vault_guard.as_ref().and_then(|v| v.get(name).map(str::to_string))
        }) {
            MattermostCredentialResolution::Configured(creds) => {
                let base_url = mm_settings
                    .url
                    .as_ref()
                    .map(|u| u.resolve(process_env_var).map(|r| r.value).unwrap_or_default())
                    .unwrap_or_default();
                let team = mm_settings
                    .team
                    .as_ref()
                    .map(|t| t.resolve(process_env_var).map(|r| r.value).unwrap_or_default())
                    .unwrap_or_default();
                let default_channel = mm_settings
                    .default_channel
                    .as_ref()
                    .map(|c| c.resolve(process_env_var).map(|r| r.value))
                    .transpose()
                    .map_err(anyhow::Error::from)?;
                if base_url.is_empty() || team.is_empty() {
                    info!("Mattermost integration disabled: url or team not configured");
                } else {
                    info!(
                        team = %team,
                        default_channel_configured = default_channel.is_some(),
                        "Mattermost integration enabled"
                    );
                    chat_services.push(Arc::new(MattermostService::new(
                        base_url,
                        creds.token,
                        team,
                        default_channel,
                        creds.webhook_secret,
                    )));
                }
            }
            MattermostCredentialResolution::Missing { env_vars } => {
                info!(
                    missing_env_vars = %env_vars.join(","),
                    "Mattermost integration disabled: missing credentials"
                );
            }
        }
    } else {
        info!("Mattermost integration disabled by server configuration");
    }
}
```

- [ ] **Step 4: Update `http_log_middleware`**

Find the `Principal::Slack` match arm in `http_log_middleware` and add parallel arms:

```rust
Principal::Mattermost { .. } => { /* same pattern as Slack arm */ }
Principal::Teams { .. }      => { /* same pattern as Slack arm */ }
```

- [ ] **Step 5: Build and run tests**

```
cargo build -p fabro-server
cargo nextest run -p fabro-server
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-server/Cargo.toml lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): wire MattermostService into chat_services"
```

---

### Task 16: Server webhook tests

**Files:**
- Modify: `lib/crates/fabro-server/tests/it/` (add a new test file or extend existing)

- [ ] **Step 1: Write webhook dispatch tests**

```rust
// lib/crates/fabro-server/tests/it/chat_webhooks.rs

use fabro_test::{expect_axum_status, test_http_client};

#[tokio::test]
async fn unknown_provider_is_404() {
    let app = test_app_state().await;
    let client = test_http_client();
    expect_axum_status(
        &client,
        "POST",
        "/api/v1/webhooks/noprovider",
        None,
        404,
    ).await;
}

#[tokio::test]
async fn slack_webhook_is_404() {
    let app = test_app_state().await;  // Slack always returns 404 from handle_webhook
    let client = test_http_client();
    expect_axum_status(
        &client,
        "POST",
        "/api/v1/webhooks/slack",
        None,
        404,
    ).await;
}
```

> **Note:** These tests require a running test server with `chat_services` populated. Follow the
> existing pattern in `fabro-server/tests/it/` for how to construct a test app state with specific
> integrations configured. If no existing pattern covers this, add the test to the conformance
> tests file and use mock services.

- [ ] **Step 2: Update the OpenAPI conformance test**

In `lib/crates/fabro-server/tests/it/openapi_conformance.rs`, find
`all_spec_routes_are_routable` and add the new webhook route:

```rust
"/api/v1/webhooks/{provider}"
```

(or `/*rest` if the router uses a wildcard segment).

- [ ] **Step 3: Run server tests**

```
cargo nextest run -p fabro-server
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add lib/crates/fabro-server/tests/
git commit -m "test(server): add chat webhook dispatch tests"
```

---

### Task 17: Full validation

- [ ] **Step 1: Run the full workspace test suite**

```
cargo nextest run --workspace
```

Expected: all green.

- [ ] **Step 2: Check formatting**

```
cargo +nightly-2026-04-14 fmt --check --all
```

Fix if needed:
```
cargo +nightly-2026-04-14 fmt --all
git add -u && git commit -m "style: fmt after mattermost integration"
```

- [ ] **Step 3: Run clippy**

```
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Fix all warnings.

- [ ] **Step 4: Create docs page stub**

Create `docs/public/integrations/mattermost.mdx` mirroring `docs/public/integrations/slack.mdx`.
Minimum required sections: Overview, Prerequisites, Configuration, Vault secrets, Notification
routes, Interview questions, Manual integration test instructions (from spec §Manual integration test).

```bash
git add docs/public/integrations/mattermost.mdx
git commit -m "docs: add Mattermost integration page"
```

- [ ] **Step 5: Update server configuration docs**

In `docs/public/administration/server-configuration.mdx`, add Mattermost and Teams sections
and the three new secrets to the secrets table.

```bash
git add docs/public/administration/server-configuration.mdx
git commit -m "docs: add Mattermost/Teams to server configuration reference"
```
