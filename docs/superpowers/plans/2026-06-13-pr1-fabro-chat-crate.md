# PR 1: `fabro-chat` Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `lib/crates/fabro-chat/` with the `ChatService` and `ChatEventContext` traits, all shared types, and test-support helpers — the foundation every subsequent PR builds on.

**Architecture:** A new crate between `fabro-interview` and the provider crates. Defines the trait contract only — no implementations, no server wiring. Mirrors the `fabro-sandbox` / `SandboxProvider` pattern exactly.

**Tech Stack:** Rust, `async-trait`, `bytes`, `http` (via `axum::http` re-exports), `serde`, `strum`, `insta` (snapshot tests in dev-deps)

**Spec:** `docs/superpowers/specs/2026-06-13-chat-service-trait-and-providers.md` — §`fabro-chat` — Trait and Shared Types

---

## File Structure

**Create:**
- `lib/crates/fabro-chat/Cargo.toml`
- `lib/crates/fabro-chat/src/lib.rs` — public re-exports
- `lib/crates/fabro-chat/src/service.rs` — `ChatService` trait
- `lib/crates/fabro-chat/src/context.rs` — `ChatEventContext` trait
- `lib/crates/fabro-chat/src/types.rs` — `ChatProviderKind`, `AnswerSubmission`, `WebhookOutcome`
- `lib/crates/fabro-chat/src/test_support.rs` — `MockChatEventContext`, `assert_chat_service_contract`

---

### Task 1: Create the crate skeleton

**Files:**
- Create: `lib/crates/fabro-chat/Cargo.toml`
- Create: `lib/crates/fabro-chat/src/lib.rs`

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "fabro-chat"
edition.workspace = true
version.workspace = true
publish = false
license.workspace = true
description = "ChatService trait and shared types for Fabro chat integrations"

[features]
test-support = []

[lib]
doctest = false

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
bytes.workspace = true
serde.workspace = true
strum.workspace = true
fabro-types = { path = "../fabro-types" }
fabro-interview = { path = "../fabro-interview" }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
serde_json.workspace = true
```

- [ ] **Step 2: Write `src/lib.rs` with empty module declarations**

```rust
pub mod context;
pub mod service;
pub mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use context::ChatEventContext;
pub use service::ChatService;
pub use types::{AnswerSubmission, ChatProviderKind, WebhookOutcome};
```

- [ ] **Step 3: Create placeholder source files so the crate compiles**

Create `lib/crates/fabro-chat/src/context.rs` with empty content:
```rust
```

Create `lib/crates/fabro-chat/src/service.rs` with empty content:
```rust
```

Create `lib/crates/fabro-chat/src/types.rs` with empty content:
```rust
```

- [ ] **Step 4: Verify the crate is picked up by the workspace**

The workspace `Cargo.toml` uses `members = ["lib/crates/*"]` so no changes are needed. Run:

```
cargo build -p fabro-chat
```

Expected: compiles (with unused import warnings — fine for now).

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-chat/
git commit -m "feat(chat): scaffold fabro-chat crate"
```

---

### Task 2: Shared types

**Files:**
- Modify: `lib/crates/fabro-chat/src/types.rs`

- [ ] **Step 1: Write the failing test**

Add to `lib/crates/fabro-chat/src/types.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_provider_kind_display() {
        assert_eq!(ChatProviderKind::Slack.to_string(), "slack");
        assert_eq!(ChatProviderKind::Mattermost.to_string(), "mattermost");
        assert_eq!(ChatProviderKind::Teams.to_string(), "teams");
    }

    #[test]
    fn chat_provider_kind_as_str() {
        assert_eq!(ChatProviderKind::Slack.as_str(), "slack");
    }

    #[test]
    fn webhook_outcome_default_body_is_none() {
        let outcome = WebhookOutcome { status: 200, body: None };
        assert_eq!(outcome.status, 200);
        assert!(outcome.body.is_none());
    }

    #[test]
    fn answer_submission_round_trips_json() {
        use fabro_interview::{Answer, AnswerSubmission};
        use fabro_types::Principal;

        let sub = AnswerSubmission {
            run_id: "run_01abc".to_string(),
            qid:    "q1".to_string(),
            answer: Answer::yes(),
            actor:  Principal::Slack {
                user_id:   "U123".to_string(),
                user_name: Some("alice".to_string()),
            },
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: AnswerSubmission = serde_json::from_str(&json).unwrap();
        assert_eq!(back.run_id, "run_01abc");
        assert_eq!(back.qid, "q1");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo nextest run -p fabro-chat
```

Expected: FAIL — types not defined yet.

- [ ] **Step 3: Implement the types**

Replace the body of `lib/crates/fabro-chat/src/types.rs`:

```rust
use fabro_interview::{Answer, AnswerSubmission as InterviewAnswerSubmission};
use fabro_types::Principal;
use serde::{Deserialize, Serialize};
use strum::{Display, IntoStaticStr};

/// Discriminant for webhook dispatch and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChatProviderKind {
    Slack,
    Mattermost,
    Teams,
}

impl ChatProviderKind {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// An answer submission from a chat platform, enriched with routing information.
///
/// Distinct from `fabro_interview::AnswerSubmission` which lacks `run_id`/`qid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSubmission {
    pub run_id: String,
    pub qid:    String,
    pub answer: Answer,
    pub actor:  Principal,
}

impl AnswerSubmission {
    /// Convert into an `fabro_interview::AnswerSubmission` for passing to the interview layer.
    pub fn into_interview_submission(self) -> InterviewAnswerSubmission {
        InterviewAnswerSubmission::new(self.answer, self.actor)
    }
}

/// Lightweight HTTP response returned by `ChatService::handle_webhook`.
///
/// Keeps provider crates free of axum; `fabro-server` converts this into an Axum response.
#[derive(Debug, Clone)]
pub struct WebhookOutcome {
    pub status: u16,
    pub body:   Option<String>,
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```
cargo nextest run -p fabro-chat
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-chat/src/types.rs
git commit -m "feat(chat): add ChatProviderKind, AnswerSubmission, WebhookOutcome types"
```

---

### Task 3: `ChatEventContext` trait

**Files:**
- Modify: `lib/crates/fabro-chat/src/context.rs`

- [ ] **Step 1: Write the failing test**

Add to `lib/crates/fabro-chat/src/context.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct AlwaysNoneContext;

    #[async_trait::async_trait]
    impl ChatEventContext for AlwaysNoneContext {
        async fn get_run_projection(
            &self,
            _run_id: fabro_types::RunId,
        ) -> Option<Arc<fabro_types::RunProjection>> {
            None
        }
        fn resolve_env(&self, _name: &str) -> Option<String> {
            None
        }
        fn run_web_url(&self, _run_id: &fabro_types::RunId) -> Option<String> {
            None
        }
        fn canonical_origin(&self) -> Option<String> {
            None
        }
    }

    #[tokio::test]
    async fn context_methods_callable() {
        let ctx = AlwaysNoneContext;
        let run_id = fabro_types::RunId::new();
        assert!(ctx.get_run_projection(run_id.clone()).await.is_none());
        assert!(ctx.resolve_env("FOO").is_none());
        assert!(ctx.run_web_url(&run_id).is_none());
        assert!(ctx.canonical_origin().is_none());
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo nextest run -p fabro-chat
```

Expected: FAIL — `ChatEventContext` not defined.

- [ ] **Step 3: Implement `ChatEventContext`**

Replace the body of `lib/crates/fabro-chat/src/context.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use fabro_types::{RunId, RunProjection};

#[async_trait]
pub trait ChatEventContext: Send + Sync {
    /// Fetch the current projection for a run (used to build notification content).
    async fn get_run_projection(&self, run_id: RunId) -> Option<Arc<RunProjection>>;

    /// Look up a named environment variable (used to resolve per-run config).
    fn resolve_env(&self, name: &str) -> Option<String>;

    /// Build a web UI deep link for a run, e.g. `https://app.example.com/runs/01abc`.
    fn run_web_url(&self, run_id: &RunId) -> Option<String>;

    /// The canonical server origin, e.g. `https://fabro.example.com`.
    /// Used by Mattermost to build button `integration.url` values.
    fn canonical_origin(&self) -> Option<String>;
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```
cargo nextest run -p fabro-chat
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-chat/src/context.rs
git commit -m "feat(chat): add ChatEventContext trait"
```

---

### Task 4: `ChatService` trait

**Files:**
- Modify: `lib/crates/fabro-chat/src/service.rs`

- [ ] **Step 1: Write the failing test**

Add to `lib/crates/fabro-chat/src/service.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use bytes::Bytes;
    use fabro_types::{
        IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
    };
    use crate::context::ChatEventContext;
    use crate::types::{AnswerSubmission, ChatProviderKind, WebhookOutcome};

    struct SlackStub;

    #[async_trait::async_trait]
    impl ChatService for SlackStub {
        fn kind(&self) -> ChatProviderKind {
            ChatProviderKind::Slack
        }
        async fn start(&self, _on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>) {}
        async fn handle_event(
            &self,
            _envelope: &fabro_types::EventEnvelope,
            _context: &dyn ChatEventContext,
        ) {
        }
        async fn handle_webhook(
            &self,
            _sub_path: &str,
            _query_string: &str,
            _headers: &http::HeaderMap,
            _body: &Bytes,
        ) -> WebhookOutcome {
            WebhookOutcome { status: 404, body: None }
        }
        fn connection_status(&self) -> IntegrationConnectionStatus {
            IntegrationConnectionStatus {
                kind:              IntegrationConnectionKind::SocketMode,
                status:            IntegrationConnectionState::Connected,
                last_connected_at: None,
                last_error:        None,
            }
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe() {
        let svc: Arc<dyn ChatService> = Arc::new(SlackStub);
        assert_eq!(svc.kind(), ChatProviderKind::Slack);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo nextest run -p fabro-chat
```

Expected: FAIL — `ChatService` not defined, `http` crate not available.

- [ ] **Step 3: Add `http` to `Cargo.toml`**

The `http` crate ships as part of axum's public API surface but is also a standalone crate. Add it to `lib/crates/fabro-chat/Cargo.toml` directly so provider crates don't need axum:

```toml
[dependencies]
# existing entries ...
http = "1"
```

Check the version axum uses:

```
cargo tree -p axum --depth 1 | grep "^http"
```

Use whatever version is listed (axum 0.8 uses `http` 1.x).

- [ ] **Step 4: Implement `ChatService`**

Replace the body of `lib/crates/fabro-chat/src/service.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use fabro_types::{EventEnvelope, IntegrationConnectionStatus};
use http::HeaderMap;

use crate::context::ChatEventContext;
use crate::types::{AnswerSubmission, ChatProviderKind, WebhookOutcome};

#[async_trait]
pub trait ChatService: Send + Sync {
    fn kind(&self) -> ChatProviderKind;

    /// Start the provider's inbound event loop.
    /// - Slack: spawns Socket Mode WebSocket loop.
    /// - Mattermost: spawns WebSocket loop.
    /// - Teams: documented no-op — delivers events via inbound HTTP only.
    async fn start(&self, on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>);

    /// Handle an outbound Fabro event (send notifications, post interview questions).
    async fn handle_event(&self, envelope: &EventEnvelope, context: &dyn ChatEventContext);

    /// Handle an inbound HTTP webhook (button actions, Teams activities).
    /// `query_string` is the raw URI query, e.g. `"token=abc&foo=bar"`.
    /// Slack always returns 404 — it has no inbound HTTP surface.
    async fn handle_webhook(
        &self,
        sub_path: &str,
        query_string: &str,
        headers: &HeaderMap,
        body: &Bytes,
    ) -> WebhookOutcome;

    fn connection_status(&self) -> IntegrationConnectionStatus;
}
```

- [ ] **Step 5: Run tests to confirm they pass**

```
cargo nextest run -p fabro-chat
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-chat/src/service.rs lib/crates/fabro-chat/Cargo.toml
git commit -m "feat(chat): add ChatService trait"
```

---

### Task 5: Test-support helpers

**Files:**
- Modify: `lib/crates/fabro-chat/src/test_support.rs`

These helpers are gated behind `#[cfg(any(test, feature = "test-support"))]` and are used by `fabro-slack`, `fabro-mattermost`, and `fabro-teams` in their unit tests.

- [ ] **Step 1: Write the failing test**

Add a test to `lib/crates/fabro-chat/src/test_support.rs` that imports and exercises the helpers:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use bytes::Bytes;
    use fabro_types::{
        IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
        RunId,
    };
    use crate::context::ChatEventContext;
    use crate::service::ChatService;
    use crate::types::{AnswerSubmission, ChatProviderKind, WebhookOutcome};

    struct TeamsStub;

    #[async_trait::async_trait]
    impl ChatService for TeamsStub {
        fn kind(&self) -> ChatProviderKind { ChatProviderKind::Teams }
        async fn start(&self, _on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>) {}
        async fn handle_event(
            &self,
            _envelope: &fabro_types::EventEnvelope,
            _context: &dyn ChatEventContext,
        ) {}
        async fn handle_webhook(
            &self,
            _sub_path: &str,
            _query_string: &str,
            _headers: &http::HeaderMap,
            _body: &Bytes,
        ) -> WebhookOutcome {
            WebhookOutcome { status: 200, body: None }
        }
        fn connection_status(&self) -> IntegrationConnectionStatus {
            IntegrationConnectionStatus {
                kind:              IntegrationConnectionKind::SocketMode,
                status:            IntegrationConnectionState::Connected,
                last_connected_at: None,
                last_error:        None,
            }
        }
    }

    #[tokio::test]
    async fn mock_context_get_run_projection_returns_none_by_default() {
        let ctx = MockChatEventContext::default();
        assert!(ctx.get_run_projection(RunId::new()).await.is_none());
    }

    #[test]
    fn mock_context_resolve_env_returns_configured_value() {
        let ctx = MockChatEventContext::default()
            .with_env("FABRO_URL", "https://app.example.com");
        assert_eq!(ctx.resolve_env("FABRO_URL").as_deref(), Some("https://app.example.com"));
        assert!(ctx.resolve_env("OTHER").is_none());
    }

    #[test]
    fn mock_context_canonical_origin() {
        let ctx = MockChatEventContext::default()
            .with_canonical_origin("https://fabro.example.com");
        assert_eq!(ctx.canonical_origin().as_deref(), Some("https://fabro.example.com"));
    }

    #[tokio::test]
    async fn contract_checker_passes_for_teams_stub() {
        let svc = TeamsStub;
        assert_chat_service_contract(&svc).await;
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo nextest run -p fabro-chat
```

Expected: FAIL — `MockChatEventContext` and `assert_chat_service_contract` not defined.

- [ ] **Step 3: Implement the test-support helpers**

Write `lib/crates/fabro-chat/src/test_support.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use fabro_types::{RunId, RunProjection};
use http::HeaderMap;

use crate::context::ChatEventContext;
use crate::service::ChatService;

/// A configurable `ChatEventContext` for use in provider unit tests.
#[derive(Default)]
pub struct MockChatEventContext {
    env:              HashMap<String, String>,
    canonical_origin: Option<String>,
    run_web_url:      Option<String>,
}

impl MockChatEventContext {
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    pub fn with_canonical_origin(mut self, origin: impl Into<String>) -> Self {
        self.canonical_origin = Some(origin.into());
        self
    }

    pub fn with_run_web_url(mut self, url: impl Into<String>) -> Self {
        self.run_web_url = Some(url.into());
        self
    }
}

#[async_trait]
impl ChatEventContext for MockChatEventContext {
    async fn get_run_projection(&self, _run_id: RunId) -> Option<Arc<RunProjection>> {
        None
    }

    fn resolve_env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn run_web_url(&self, _run_id: &RunId) -> Option<String> {
        self.run_web_url.clone()
    }

    fn canonical_origin(&self) -> Option<String> {
        self.canonical_origin.clone()
    }
}

/// Thin conformance checker: verifies a `ChatService` implementation satisfies
/// the basic contract without panicking or returning obviously invalid values.
pub async fn assert_chat_service_contract(service: &dyn ChatService) {
    // kind() returns a valid variant (doesn't panic)
    let _ = service.kind();

    // connection_status() doesn't panic
    let _ = service.connection_status();

    // handle_webhook with empty inputs returns a plausible HTTP status
    let outcome = service
        .handle_webhook("", "", &HeaderMap::new(), &Bytes::new())
        .await;
    assert!(
        outcome.status >= 200 && outcome.status < 600,
        "handle_webhook returned invalid HTTP status {}",
        outcome.status
    );
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```
cargo nextest run -p fabro-chat
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-chat/src/test_support.rs
git commit -m "feat(chat): add MockChatEventContext and assert_chat_service_contract"
```

---

### Task 6: Full workspace build check

- [ ] **Step 1: Build the full workspace without test-support**

```
cargo build --workspace
```

Expected: PASS. `fabro-chat` compiles; no other crates affected yet.

- [ ] **Step 2: Run the full workspace test suite**

```
cargo nextest run --workspace
```

Expected: PASS. New `fabro-chat` tests pass; no existing tests broken.

- [ ] **Step 3: Check formatting**

```
cargo +nightly-2026-04-14 fmt --check --all
```

If formatting issues are reported, fix them:

```
cargo +nightly-2026-04-14 fmt --all
git add -u
git commit -m "style: fmt fabro-chat"
```

- [ ] **Step 4: Run clippy**

```
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: PASS. Fix any warnings before proceeding.

- [ ] **Step 5: Final commit (if needed after fmt/clippy)**

```bash
git add -u
git commit -m "fix(chat): address clippy warnings in fabro-chat"
```
