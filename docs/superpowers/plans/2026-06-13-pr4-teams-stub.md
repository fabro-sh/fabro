# PR 4: MS Teams Stub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a minimal `fabro-teams` crate with a `TeamsService` that satisfies the `ChatService` contract for a pure-inbound-HTTP provider — no outbound connection, no real JWT validation, returns 200 for any POST — wired into `chat_services` in `fabro-server`.

**Architecture:** Two-module crate (`lib.rs` for credential resolution, `service.rs` for `TeamsService`). `TeamsIntegrationSettings` and `FABRO_TEAMS_WEBHOOK_SECRET` were added in PR 3. This PR creates the crate, wires it into the server, and confirms the `ChatService` contract holds for a no-op provider. Real Teams JWT validation, Adaptive Card formatting, and event routing are explicitly deferred (out of scope).

**Tech Stack:** Rust, `async-trait`, `fabro-chat`, `fabro-static`, `fabro-types`

**Spec:** `docs/superpowers/specs/2026-06-13-chat-service-trait-and-providers.md` — §Phase 3: MS Teams Stubs, §Server Wiring

**Branches:** Stacks on PR 3. Branch from PR 3's branch or from `main` after PR 3 merges.

---

## Before you start

```
cargo nextest run --workspace
```

All tests must pass before you make any changes.

---

## File Structure

**Create:**
- `lib/crates/fabro-teams/Cargo.toml`
- `lib/crates/fabro-teams/src/lib.rs` — credential resolution
- `lib/crates/fabro-teams/src/service.rs` — `TeamsService` + `impl ChatService`

**Modify:**
- `lib/crates/fabro-server/Cargo.toml` — add `fabro-teams` dependency
- `lib/crates/fabro-server/src/server.rs` — wire `TeamsService` into `chat_services` construction

---

### Task 1: Create `fabro-teams` crate skeleton

**Files:**
- Create: `lib/crates/fabro-teams/Cargo.toml`
- Create: `lib/crates/fabro-teams/src/lib.rs`

- [ ] **Step 1: Write the failing credential resolution tests**

Create `lib/crates/fabro-teams/src/lib.rs` with tests only (no implementation yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fabro_static::EnvVars;

    #[test]
    fn missing_when_secret_absent() {
        let result = resolve_credentials_status_with_lookup(|_| None);
        assert!(matches!(result, TeamsCredentialResolution::Missing { .. }));
    }

    #[test]
    fn missing_when_secret_empty() {
        let result = resolve_credentials_status_with_lookup(|_| Some(String::new()));
        assert!(matches!(result, TeamsCredentialResolution::Missing { .. }));
    }

    #[test]
    fn configured_when_secret_present() {
        let result = resolve_credentials_status_with_lookup(|name| {
            if name == EnvVars::FABRO_TEAMS_WEBHOOK_SECRET {
                Some("secret123".into())
            } else {
                None
            }
        });
        match result {
            TeamsCredentialResolution::Configured(creds) => {
                assert_eq!(creds.webhook_secret, "secret123");
            }
            _ => panic!("expected Configured"),
        }
    }

    #[test]
    fn missing_lists_correct_env_var() {
        let result = resolve_credentials_status_with_lookup(|_| None);
        match result {
            TeamsCredentialResolution::Missing { env_vars } => {
                assert!(env_vars.contains(&EnvVars::FABRO_TEAMS_WEBHOOK_SECRET));
            }
            _ => panic!("expected Missing"),
        }
    }
}
```

- [ ] **Step 2: Create `Cargo.toml`**

```toml
[package]
name    = "fabro-teams"
version.workspace = true
edition.workspace = true

[dependencies]
async-trait.workspace = true
bytes.workspace = true
fabro-chat  = { path = "../fabro-chat" }
fabro-static = { path = "../fabro-static" }
fabro-types = { path = "../fabro-types" }
http = "1"
tracing.workspace = true

[dev-dependencies]
fabro-chat = { path = "../fabro-chat", features = ["test-support"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

- [ ] **Step 3: Run tests to confirm they fail**

```
cargo nextest run -p fabro-teams
```

Expected: FAIL — types not defined yet.

- [ ] **Step 4: Implement `lib.rs`**

```rust
pub mod service;

pub use service::TeamsService;

use fabro_static::EnvVars;

#[derive(Debug, Clone)]
pub struct TeamsCredentials {
    pub webhook_secret: String,
}

#[derive(Debug, Clone)]
pub enum TeamsCredentialResolution {
    Configured(TeamsCredentials),
    Missing { env_vars: Vec<&'static str> },
}

pub fn resolve_credentials_status_with_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> TeamsCredentialResolution {
    match non_empty(lookup(EnvVars::FABRO_TEAMS_WEBHOOK_SECRET)) {
        Some(webhook_secret) => {
            TeamsCredentialResolution::Configured(TeamsCredentials { webhook_secret })
        }
        None => TeamsCredentialResolution::Missing {
            env_vars: vec![EnvVars::FABRO_TEAMS_WEBHOOK_SECRET],
        },
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 go here)
}
```

Add a placeholder `service.rs` so the crate compiles:

```rust
// lib/crates/fabro-teams/src/service.rs
// placeholder
```

- [ ] **Step 5: Run tests**

```
cargo nextest run -p fabro-teams
```

Expected: PASS (credential tests pass; `service.rs` is a placeholder).

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-teams/
git commit -m "feat(teams): create crate skeleton with credential resolution"
```

---

### Task 2: Implement `TeamsService`

**Files:**
- Modify: `lib/crates/fabro-teams/src/service.rs`

- [ ] **Step 1: Write tests**

```rust
// lib/crates/fabro-teams/src/service.rs

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_chat::test_support::assert_chat_service_contract;
    use fabro_chat::ChatService;

    fn test_service() -> TeamsService {
        TeamsService::new("webhook-secret".into())
    }

    #[tokio::test]
    async fn chat_service_contract_holds() {
        assert_chat_service_contract(&test_service()).await;
    }

    #[test]
    fn kind_is_teams() {
        assert_eq!(test_service().kind(), fabro_chat::ChatProviderKind::Teams);
    }

    #[tokio::test]
    async fn start_returns_immediately() {
        let svc = test_service();
        let on_submit = std::sync::Arc::new(|_| {});
        // Must return without blocking — timeout if it doesn't
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            svc.start(on_submit),
        )
        .await
        .expect("start() should return immediately for Teams stub");
    }

    #[tokio::test]
    async fn handle_webhook_any_body_is_200() {
        let svc = test_service();
        let outcome = svc
            .handle_webhook("", "", &http::HeaderMap::new(), &bytes::Bytes::from("{}"))
            .await;
        assert_eq!(outcome.status, 200);
    }

    #[tokio::test]
    async fn handle_webhook_empty_body_is_200() {
        let svc = test_service();
        let outcome = svc
            .handle_webhook("", "", &http::HeaderMap::new(), &bytes::Bytes::new())
            .await;
        assert_eq!(outcome.status, 200);
    }

    #[test]
    fn connection_status_is_always_connected() {
        use fabro_types::IntegrationConnectionState;
        let svc = test_service();
        let status = svc.connection_status();
        assert_eq!(status.status, IntegrationConnectionState::Connected);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```
cargo nextest run -p fabro-teams -- service
```

Expected: FAIL.

- [ ] **Step 3: Implement `TeamsService`**

```rust
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use fabro_chat::{
    AnswerSubmission, ChatEventContext, ChatProviderKind, ChatService, WebhookOutcome,
};
use fabro_types::{
    EventEnvelope, IntegrationConnectionKind, IntegrationConnectionState,
    IntegrationConnectionStatus,
};
use http::HeaderMap;
use tracing::info;

pub struct TeamsService {
    _webhook_secret: String,
}

impl TeamsService {
    pub fn new(webhook_secret: String) -> Self {
        Self { _webhook_secret: webhook_secret }
    }
}

#[async_trait]
impl ChatService for TeamsService {
    fn kind(&self) -> ChatProviderKind {
        ChatProviderKind::Teams
    }

    async fn start(&self, _on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>) {
        info!("Teams: no outbound connection (pure inbound HTTP)");
        // Documented no-op: Teams delivers events via HTTP POST, not outbound WebSocket.
    }

    async fn handle_event(&self, _envelope: &EventEnvelope, _context: &dyn ChatEventContext) {
        info!("Teams notifications not yet implemented");
    }

    async fn handle_webhook(
        &self,
        _sub_path: &str,
        _query_string: &str,
        _headers: &HeaderMap,
        _body: &Bytes,
    ) -> WebhookOutcome {
        WebhookOutcome { status: 200, body: None }
    }

    fn connection_status(&self) -> IntegrationConnectionStatus {
        IntegrationConnectionStatus {
            kind:              IntegrationConnectionKind::None,
            status:            IntegrationConnectionState::Connected,
            last_connected_at: None,
            last_error:        None,
        }
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 go here)
}
```

> **Note:** `IntegrationConnectionKind::None` may not exist yet — check `fabro-types`. If the
> enum only has `SocketMode` and `WebSocket`, add `None` as a variant (or use a suitable
> existing variant like `NotApplicable`). If adding a new variant, update any exhaustive matches
> in the codebase. Use `grep -rn "IntegrationConnectionKind" lib/` to find all match sites.

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-teams
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-teams/src/service.rs
git commit -m "feat(teams): implement TeamsService stub"
```

---

### Task 3: Wire `TeamsService` into `fabro-server`

**Files:**
- Modify: `lib/crates/fabro-server/Cargo.toml`
- Modify: `lib/crates/fabro-server/src/server.rs`

- [ ] **Step 1: Add `fabro-teams` to `fabro-server` Cargo.toml**

```toml
fabro-teams = { path = "../fabro-teams" }
```

- [ ] **Step 2: Add `use` import in `server.rs`**

```rust
use fabro_teams::{TeamsService, TeamsCredentialResolution,
                  resolve_credentials_status_with_lookup as resolve_teams_credentials};
```

- [ ] **Step 3: Extend `chat_services` construction**

In the `build_app_state` block that builds the `chat_services` vec, add after the Mattermost
section:

```rust
{
    let teams_settings = &current_server_settings.server.integrations.teams;
    if teams_settings.enabled {
        let vault_guard = vault.try_read().ok();
        match resolve_teams_credentials(|name| {
            vault_guard.as_ref().and_then(|v| v.get(name).map(str::to_string))
        }) {
            TeamsCredentialResolution::Configured(creds) => {
                info!("Teams integration enabled (stub — inbound HTTP only)");
                chat_services.push(Arc::new(TeamsService::new(creds.webhook_secret)));
            }
            TeamsCredentialResolution::Missing { env_vars } => {
                info!(
                    missing_env_vars = %env_vars.join(","),
                    "Teams integration disabled: missing credentials"
                );
            }
        }
    } else {
        info!("Teams integration disabled by server configuration");
    }
}
```

- [ ] **Step 4: Build and run tests**

```
cargo build -p fabro-server
cargo nextest run -p fabro-server
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-server/Cargo.toml lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): wire TeamsService stub into chat_services"
```

---

### Task 4: Full validation

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
git add -u && git commit -m "style: fmt after teams stub"
```

- [ ] **Step 3: Run clippy**

```
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Fix all warnings before merging. Pay particular attention to `dead_code` warnings on
`TeamsService::_webhook_secret` — if clippy flags the leading underscore convention, either
suppress with `#[allow(dead_code)]` or restructure to consume the value (e.g., store as `()`
until real validation is implemented, and add a `#[expect(dead_code, reason = "...")]`).

- [ ] **Step 4: Verify the full stacked-PR picture**

At the end of PR 4, the workspace should contain:
- `fabro-chat` — `ChatService` trait, `AnswerSubmission`, `WebhookOutcome`, `ChatProviderKind`, `ChatEventContext`, `MockChatEventContext`
- `fabro-slack` — `SlackService implements ChatService` (moved from server.rs)
- `fabro-mattermost` — `MattermostService implements ChatService` (new)
- `fabro-teams` — `TeamsService implements ChatService` (new, stub)
- `fabro-server` — `chat_services: Vec<Arc<dyn ChatService>>`, `ChatEventContext for AppState`, `start_chat_services`, `POST /api/v1/webhooks/:provider` route

Confirm by running:

```
grep -rn "ChatService" lib/crates/fabro-{slack,mattermost,teams}/src/service.rs
```

Expected: each file shows `impl ChatService for <Name>Service`.
