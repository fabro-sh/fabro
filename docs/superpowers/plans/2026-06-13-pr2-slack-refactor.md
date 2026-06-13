# PR 2: Slack Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `SlackService` out of `fabro-server/src/server.rs` into `fabro-slack` as a `ChatService` implementation, rewire `AppState` to hold `Vec<Arc<dyn ChatService>>`, and add the webhook dispatch route — with zero user-visible behavior change.

**Architecture:** Pure structural refactor. All existing Slack behavior (Socket Mode WebSocket, interview prompts, lifecycle notifications) is preserved exactly. The `SlackService` struct moves crates and gains a `ChatService` impl; `server.rs` loses ~300 lines of provider-specific code and gains a generic dispatch loop. All existing tests must pass unchanged.

**Tech Stack:** Rust, `async-trait`, `axum`, `fabro-chat` (from PR 1), `fabro-slack`, `tokio`

**Spec:** `docs/superpowers/specs/2026-06-13-chat-service-trait-and-providers.md` — §Phase 1: Slack Refactor, §Server Wiring

**Branches:** Stacks on PR 1. Branch from PR 1's branch or from `main` after PR 1 merges.

---

## Before you start

Run the full test suite to establish a green baseline:

```
cargo nextest run --workspace
```

All tests must pass before you make any changes. If they don't, stop and investigate.

---

## File Structure

**Modify:**
- `lib/crates/fabro-chat/src/context.rs` — add `get_run_events` method (needed by Slack's lifecycle code)
- `lib/crates/fabro-chat/src/test_support.rs` — stub `get_run_events` on `MockChatEventContext`
- `lib/crates/fabro-slack/Cargo.toml` — add `fabro-chat` dependency
- `lib/crates/fabro-slack/src/lib.rs` — expose `service` module
- `lib/crates/fabro-server/Cargo.toml` — add `fabro-chat` dependency
- `lib/crates/fabro-server/src/server.rs` — remove `SlackService`, rewire `AppState`

**Create:**
- `lib/crates/fabro-slack/src/service.rs` — `SlackService` + `impl ChatService`

---

### Task 1: Extend `ChatEventContext` with `get_run_events`

**Files:**
- Modify: `lib/crates/fabro-chat/src/context.rs`
- Modify: `lib/crates/fabro-chat/src/test_support.rs`

`SlackService::handle_lifecycle_event` currently calls a server-only helper
(`load_prior_slack_lifecycle_event_details`) that reads past run events to find the workflow
start event name and any PR link. After the move, the service can't access the store directly.
Add a narrow method to `ChatEventContext` so providers can request this.

- [ ] **Step 1: Add `get_run_events` to the trait**

In `lib/crates/fabro-chat/src/context.rs`, add one method to the trait:

```rust
use fabro_types::{EventEnvelope, RunId, RunProjection};

#[async_trait]
pub trait ChatEventContext: Send + Sync {
    async fn get_run_projection(&self, run_id: RunId) -> Option<Arc<RunProjection>>;
    fn resolve_env(&self, name: &str) -> Option<String>;
    fn run_web_url(&self, run_id: &RunId) -> Option<String>;
    fn canonical_origin(&self) -> Option<String>;

    /// Return all recorded events for a run up to (not including) `before_seq`.
    /// Returns an empty vec if the run does not exist or events cannot be loaded.
    async fn get_run_events(&self, run_id: RunId, before_seq: u32) -> Vec<EventEnvelope>;
}
```

- [ ] **Step 2: Update `MockChatEventContext` to implement the new method**

In `lib/crates/fabro-chat/src/test_support.rs`:

```rust
#[async_trait]
impl ChatEventContext for MockChatEventContext {
    // ... existing methods unchanged ...

    async fn get_run_events(&self, _run_id: RunId, _before_seq: u32) -> Vec<EventEnvelope> {
        vec![]
    }
}
```

Also update the `AlwaysNoneContext` in `context.rs` tests:

```rust
#[async_trait::async_trait]
impl ChatEventContext for AlwaysNoneContext {
    // ... existing methods ...
    async fn get_run_events(&self, _run_id: fabro_types::RunId, _before_seq: u32)
        -> Vec<fabro_types::EventEnvelope> { vec![] }
}
```

- [ ] **Step 3: Run tests**

```
cargo nextest run -p fabro-chat
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add lib/crates/fabro-chat/src/context.rs lib/crates/fabro-chat/src/test_support.rs
git commit -m "feat(chat): add get_run_events to ChatEventContext"
```

---

### Task 2: Add `fabro-chat` to `fabro-slack`

**Files:**
- Modify: `lib/crates/fabro-slack/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `lib/crates/fabro-slack/Cargo.toml`, add under `[dependencies]`:

```toml
fabro-chat = { path = "../fabro-chat" }
bytes.workspace = true
http = "1"
```

- [ ] **Step 2: Verify the crate still compiles**

```
cargo build -p fabro-slack
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add lib/crates/fabro-slack/Cargo.toml
git commit -m "feat(slack): add fabro-chat dependency"
```

---

### Task 3: Create `SlackService` in `fabro-slack`

**Files:**
- Create: `lib/crates/fabro-slack/src/service.rs`

This is the main work of the PR. The `SlackService` struct moves from `server.rs` into
`fabro-slack/src/service.rs` and gets a `ChatService` impl. The key changes:

- `handle_event` takes `&dyn ChatEventContext` instead of `(&AppState, run_web_url: Option<&str>)`
- `handle_lifecycle_event` uses `context.get_run_projection()`, `context.get_run_events()`,
  `context.resolve_env()`, and `context.run_web_url()` instead of accessing `AppState` directly
- `start()` takes `on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>` (chat type, not slack type)
  and wraps it to convert `SlackAnswerSubmission` → `fabro_chat::AnswerSubmission` before calling
- `submit_answer` is removed entirely — the `on_submit` callback now owns that responsibility
- `handle_webhook` always returns `WebhookOutcome { status: 404, body: None }`

- [ ] **Step 1: Write a failing test that imports `SlackService` from `fabro-slack`**

Add to `lib/crates/fabro-slack/src/service.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fabro_chat::test_support::assert_chat_service_contract;

    fn test_service() -> SlackService {
        SlackService::new("bot-token".into(), "app-token".into(), None)
    }

    #[tokio::test]
    async fn chat_service_contract_holds() {
        assert_chat_service_contract(&test_service()).await;
    }

    #[test]
    fn handle_webhook_returns_404() {
        let svc = test_service();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(svc.handle_webhook(
            "",
            "",
            &http::HeaderMap::new(),
            &bytes::Bytes::new(),
        ));
        assert_eq!(outcome.status, 404);
    }

    #[test]
    fn kind_is_slack() {
        use fabro_chat::ChatService;
        assert_eq!(test_service().kind(), fabro_chat::ChatProviderKind::Slack);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo nextest run -p fabro-slack
```

Expected: FAIL — `SlackService` not yet in `service.rs`.

- [ ] **Step 3: Implement `service.rs`**

Create `lib/crates/fabro-slack/src/service.rs` with the full implementation. Copy the
`SlackService` struct and all its methods from `server.rs`, adapting as described above:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use fabro_chat::{
    AnswerSubmission, ChatEventContext, ChatProviderKind, ChatService, WebhookOutcome,
};
use fabro_interview::Answer;
use fabro_types::{
    EventBody, EventEnvelope, IntegrationConnectionKind, IntegrationConnectionState,
    IntegrationConnectionStatus, Principal, QuestionType, RunId,
};
use futures::future::join_all;
use http::HeaderMap;
use tracing::warn;

use crate::blocks as slack_blocks;
use crate::client::{SlackClient, SlackPostedMessage};
use crate::connection::{self as slack_connection, SlackConnectionRuntimeState};
use crate::payload::SlackAnswerSubmission;
use crate::threads::ThreadRegistry;

fn sanitize_integration_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 240;
    let sanitized = error.replace(['\r', '\n'], " ");
    sanitized.chars().take(MAX_ERROR_CHARS).collect()
}

pub struct SlackService {
    pub(crate) client:          SlackClient,
    pub(crate) app_token:       String,
    pub(crate) default_channel: Option<String>,
    pub(crate) posted_messages: Arc<Mutex<HashMap<(RunId, String), SlackPostedMessage>>>,
    pub(crate) thread_registry: Arc<ThreadRegistry>,
    pub(crate) connection:      Arc<Mutex<SlackConnectionRuntimeState>>,
}

impl SlackService {
    pub fn new(bot_token: String, app_token: String, default_channel: Option<String>) -> Self {
        Self {
            client:          SlackClient::new(bot_token),
            app_token,
            default_channel,
            posted_messages: Arc::new(Mutex::new(HashMap::new())),
            thread_registry: Arc::new(ThreadRegistry::new()),
            connection:      Arc::new(Mutex::new(SlackConnectionRuntimeState::default())),
        }
    }

    pub(crate) fn status_sink(&self) -> slack_connection::ConnectionStatusSink {
        let connection = Arc::clone(&self.connection);
        Arc::new(move |update| {
            let mut state = connection
                .lock()
                .expect("slack connection state lock poisoned");
            match update {
                slack_connection::ConnectionStatusUpdate::Connecting => {
                    state.status = IntegrationConnectionState::Connecting;
                    state.last_error = None;
                }
                slack_connection::ConnectionStatusUpdate::Connected => {
                    state.status = IntegrationConnectionState::Connected;
                    state.last_connected_at = Some(Utc::now());
                    state.last_error = None;
                }
                slack_connection::ConnectionStatusUpdate::Error(error) => {
                    state.status = IntegrationConnectionState::Error;
                    state.last_error = Some(sanitize_integration_error(&error));
                }
            }
        })
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
            .expect("slack posted messages lock poisoned")
            .remove(&key);
        let Some(posted) = posted else { return };
        self.thread_registry.remove(&posted.ts);
        let blocks = slack_blocks::answered_blocks(question_text, answer_text);
        let _ = self
            .client
            .update_message(&posted.channel_id, &posted.ts, &blocks)
            .await;
    }

    async fn handle_lifecycle_event(
        &self,
        envelope: &EventEnvelope,
        context: &dyn ChatEventContext,
    ) {
        use fabro_types::EventBody;
        let event = &envelope.event;
        let Some(details) = slack_lifecycle_details(event) else { return };
        let event_name = event.body.event_name();

        let projection = match context.get_run_projection(event.run_id).await {
            Some(p) => p,
            None => {
                warn!(
                    run_id = %event.run_id,
                    event = event_name,
                    "Skipping Slack lifecycle notification: run projection missing"
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
                    && route.provider.as_deref() == Some("slack")
                    && route.events.iter().any(|e| e == event_name)
            })
            .collect();
        if routes.is_empty() { return; }
        routes.sort_by_key(|(name, _)| *name);

        let prior = if matches!(details.kind, slack_blocks::RunLifecycleKind::Started) {
            PriorSlackLifecycleEventDetails::default()
        } else {
            load_prior_slack_lifecycle_event_details(context, event.run_id, envelope.seq).await
        };

        let workflow_label = slack_lifecycle_workflow_label(
            projection.as_ref(),
            details.started_event_name.as_deref()
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
        let run_url_owned = context.run_web_url(&event.run_id);
        let run_url = run_url_owned
            .as_deref()
            .or(projection.web_url.as_deref());
        let pull_request_blocks =
            pull_request
                .as_ref()
                .map(|pr| slack_blocks::RunLifecyclePullRequest {
                    number: pr.number,
                    title:  pr.title.as_deref(),
                    url:    pr.url.as_deref(),
                });
        let blocks =
            slack_blocks::run_lifecycle_blocks(details.kind, &slack_blocks::RunLifecycleBlocks {
                run_id: &run_id,
                run_url,
                workflow_label: &workflow_label,
                result: details.result.as_deref(),
                duration_ms: details.duration_ms,
                pull_request: pull_request_blocks,
            });

        let posts = routes.into_iter().filter_map(|(route_name, route)| {
            let channel = resolve_slack_lifecycle_route_channel(
                context,
                event.run_id,
                route_name,
                route,
                event_name,
            )?;
            Some(async move {
                if let Err(err) = self.client.post_message(&channel, &blocks, None).await {
                    warn!(
                        run_id = %event.run_id,
                        event = event_name,
                        notification_route = route_name.as_str(),
                        error = %err,
                        "Failed to post Slack lifecycle notification"
                    );
                }
            })
        });
        join_all(posts).await;
    }
}

#[async_trait]
impl ChatService for SlackService {
    fn kind(&self) -> ChatProviderKind {
        ChatProviderKind::Slack
    }

    async fn start(&self, on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>) {
        let thread_registry = Arc::clone(&self.thread_registry);
        let on_submit_wrapped: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync> =
            Arc::new(move |slack_sub: SlackAnswerSubmission| {
                let chat_sub = AnswerSubmission {
                    run_id: slack_sub.run_id,
                    qid:    slack_sub.qid,
                    answer: slack_sub.answer,
                    actor:  slack_sub.actor,
                };
                on_submit(chat_sub);
            });
        slack_connection::run_with_status(
            &self.client,
            &self.app_token,
            &thread_registry,
            on_submit_wrapped,
            self.status_sink(),
        )
        .await;
    }

    async fn handle_event(&self, envelope: &EventEnvelope, context: &dyn ChatEventContext) {
        use crate::interaction::runtime_question_from_interview_record;
        use fabro_types::InterviewQuestionRecord;

        let event = &envelope.event;
        match &event.body {
            EventBody::InterviewStarted(props) => {
                if props.question_id.is_empty() { return; }
                let Some(default_channel) = self.default_channel.as_deref() else { return };
                let key = (event.run_id, props.question_id.clone());
                if self
                    .posted_messages
                    .lock()
                    .expect("slack posted messages lock poisoned")
                    .contains_key(&key)
                {
                    return;
                }
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
                let run_web_url = context.run_web_url(&event.run_id);
                let blocks = slack_blocks::question_to_blocks(
                    &event.run_id.to_string(),
                    &props.question_id,
                    &question,
                    run_web_url.as_deref(),
                );
                if let Ok(posted) = self
                    .client
                    .post_message(default_channel, &blocks, None)
                    .await
                {
                    if question.allow_freeform || question.question_type == QuestionType::Freeform {
                        self.thread_registry.register(
                            &posted.ts,
                            &event.run_id.to_string(),
                            &props.question_id,
                        );
                    }
                    self.posted_messages
                        .lock()
                        .expect("slack posted messages lock poisoned")
                        .insert(key, posted);
                }
            }
            EventBody::InterviewCompleted(props) => {
                self.finish_interview(
                    event.run_id, &props.question_id, &props.question, &props.answer,
                ).await;
            }
            EventBody::InterviewTimeout(props) => {
                self.finish_interview(
                    event.run_id, &props.question_id, &props.question, "Timed out",
                ).await;
            }
            EventBody::InterviewInterrupted(props) => {
                self.finish_interview(
                    event.run_id, &props.question_id, &props.question, "Interrupted",
                ).await;
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
        _query_string: &str,
        _headers: &HeaderMap,
        _body: &Bytes,
    ) -> WebhookOutcome {
        WebhookOutcome { status: 404, body: None }
    }

    fn connection_status(&self) -> IntegrationConnectionStatus {
        let state = self
            .connection
            .lock()
            .expect("slack connection state lock poisoned")
            .clone();
        IntegrationConnectionStatus {
            kind:              IntegrationConnectionKind::SocketMode,
            status:            state.status,
            last_connected_at: state.last_connected_at,
            last_error:        state.last_error,
        }
    }
}

// --- Lifecycle helpers (previously in server.rs) ---

#[derive(Default)]
struct PriorSlackLifecycleEventDetails {
    started_event_name: Option<String>,
    pull_request:       Option<SlackLifecyclePullRequest>,
}

struct SlackLifecyclePullRequest {
    number: u64,
    title:  Option<String>,
    url:    Option<String>,
}

struct SlackLifecycleDetails {
    kind:               slack_blocks::RunLifecycleKind,
    started_event_name: Option<String>,
    result:             Option<String>,
    duration_ms:        Option<u64>,
}

async fn load_prior_slack_lifecycle_event_details(
    context: &dyn ChatEventContext,
    run_id: RunId,
    before_seq: u32,
) -> PriorSlackLifecycleEventDetails {
    let mut details = PriorSlackLifecycleEventDetails::default();
    for envelope in context.get_run_events(run_id, before_seq).await {
        match envelope.event.body {
            EventBody::RunStarted(props) if !props.name.trim().is_empty() => {
                details.started_event_name = Some(props.name);
            }
            EventBody::PullRequestCreated(props) => {
                details.pull_request = Some(SlackLifecyclePullRequest {
                    number: props.pr_number,
                    title:  Some(props.title),
                    url:    Some(props.pr_url),
                });
            }
            _ => {}
        }
    }
    details
}

fn slack_lifecycle_details(event: &fabro_types::RunEvent) -> Option<SlackLifecycleDetails> {
    match &event.body {
        EventBody::RunStarted(props) => Some(SlackLifecycleDetails {
            kind:               slack_blocks::RunLifecycleKind::Started,
            started_event_name: Some(props.name.clone()),
            result:             None,
            duration_ms:        None,
        }),
        EventBody::RunCompleted(props) => Some(SlackLifecycleDetails {
            kind:               slack_blocks::RunLifecycleKind::Completed,
            started_event_name: None,
            result:             Some(slack_lifecycle_completed_result(props)),
            duration_ms:        props.duration_ms,
        }),
        EventBody::RunFailed(props) => Some(SlackLifecycleDetails {
            kind:               slack_blocks::RunLifecycleKind::Failed,
            started_event_name: None,
            result:             Some(props.error.clone()),
            duration_ms:        props.duration_ms,
        }),
        _ => None,
    }
}

fn slack_lifecycle_completed_result(props: &fabro_types::RunCompletedProps) -> String {
    // Copy the logic from server.rs exactly
    if let Some(result) = &props.result {
        result.clone()
    } else {
        "Completed".to_string()
    }
}

fn slack_lifecycle_workflow_label(
    projection: &fabro_store::RunProjection,
    started_event_name: Option<&str>,
    event_name: &str,
) -> String {
    [
        projection.spec.workflow_name(),
        projection.spec.workflow_slug(),
        projection.spec.graph_name(),
        started_event_name,
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .unwrap_or(event_name)
    .to_string()
}

fn slack_lifecycle_pull_request_from_link(
    link: &fabro_types::PullRequestLink,
) -> SlackLifecyclePullRequest {
    SlackLifecyclePullRequest {
        number: link.number,
        title:  None,
        url:    Some(link.html_url()),
    }
}

fn resolve_slack_lifecycle_route_channel(
    context: &dyn ChatEventContext,
    run_id: RunId,
    route_name: &str,
    route: &fabro_types::NotificationRouteSettings,
    event_name: &str,
) -> Option<String> {
    let Some(channel) = route.slack.as_ref().and_then(|s| s.channel.as_ref()) else {
        warn!(
            run_id = %run_id,
            notification_route = route_name,
            event = event_name,
            "Skipping Slack lifecycle notification route without channel"
        );
        return None;
    };
    let resolved = match channel.resolve(|name| context.resolve_env(name)) {
        Ok(r) => r.value,
        Err(err) => {
            warn!(
                run_id = %run_id,
                notification_route = route_name,
                event = event_name,
                error = %err,
                "Skipping Slack lifecycle notification route with unresolved channel"
            );
            return None;
        }
    };
    if resolved.trim().is_empty() {
        warn!(
            run_id = %run_id,
            notification_route = route_name,
            event = event_name,
            "Skipping Slack lifecycle notification route with empty channel"
        );
        return None;
    }
    Some(resolved)
}
```

> **Note:** Some types like `RunCompletedProps`, `PullRequestLink`, `NotificationRouteSettings` may need additional imports from `fabro_types`. Also `fabro_store::RunProjection` is used for `slack_lifecycle_workflow_label` — add `fabro-store` to `fabro-slack`'s dependencies if it's not already there (`fabro-store = { path = "../fabro-store" }`). Check what `fabro_types::NotificationRouteSettings` vs `fabro_store::RunProjection` is needed and add only the missing deps.

- [ ] **Step 4: Run tests**

```
cargo nextest run -p fabro-slack
```

Expected: PASS (new tests + all existing tests).

- [ ] **Step 5: Commit**

```bash
git add lib/crates/fabro-slack/src/service.rs
git commit -m "feat(slack): move SlackService to fabro-slack, impl ChatService"
```

---

### Task 4: Export `SlackService` from `fabro-slack`

**Files:**
- Modify: `lib/crates/fabro-slack/src/lib.rs`

- [ ] **Step 1: Add the module and re-export**

```rust
pub mod blocks;
pub mod client;
pub mod config;
pub mod connection;
pub mod dispatch;
pub mod interaction;
pub mod payload;
pub mod service;
pub mod socket;
pub mod threads;

pub use service::SlackService;
```

- [ ] **Step 2: Build `fabro-slack`**

```
cargo build -p fabro-slack
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add lib/crates/fabro-slack/src/lib.rs
git commit -m "feat(slack): export SlackService from fabro-slack"
```

---

### Task 5: Add `fabro-chat` to `fabro-server`

**Files:**
- Modify: `lib/crates/fabro-server/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `[dependencies]` in `lib/crates/fabro-server/Cargo.toml`:

```toml
fabro-chat = { path = "../fabro-chat" }
```

- [ ] **Step 2: Verify**

```
cargo build -p fabro-server
```

Expected: PASS (no code changes yet).

- [ ] **Step 3: Commit**

```bash
git add lib/crates/fabro-server/Cargo.toml
git commit -m "feat(server): add fabro-chat dependency"
```

---

### Task 6: Rewire `AppState` in `server.rs`

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`

This task makes four targeted changes to `server.rs`. Read the full file first.

**Change A — Remove the `SlackService` struct and all its `impl` blocks**

Delete lines 581–875 (the `SlackService` struct, `impl SlackService`, and all helpers that
lived inside it: `sanitize_integration_error`, `slack_lifecycle_details`,
`slack_lifecycle_workflow_label`, `slack_lifecycle_pull_request_from_link`,
`resolve_slack_lifecycle_route_channel`, `load_prior_slack_lifecycle_event_details`).

Also remove the `SlackAnswerSubmission` import at line 82.

Add instead:
```rust
use fabro_chat::{AnswerSubmission as ChatAnswerSubmission, ChatService};
use fabro_slack::SlackService;
```

**Change B — Update `AppState` fields**

Replace:
```rust
slack_service: Option<Arc<SlackService>>,
slack_started: AtomicBool,
```

With:
```rust
chat_services:         Vec<Arc<dyn ChatService>>,
chat_services_started: AtomicBool,
```

**Change C — Update `AppState` construction**

Replace the `slack_service` block (lines ~2358–2396) and the corresponding `slack_service` /
`slack_started` fields in the `Ok(Arc::new(AppState { ... }))` struct literal:

```rust
// Build chat_services vec
let mut chat_services: Vec<Arc<dyn ChatService>> = Vec::new();
{
    let slack_settings = &current_server_settings.server.integrations.slack;
    if slack_settings.enabled {
        let default_channel = slack_settings
            .default_channel
            .as_ref()
            .map(|value| {
                value
                    .resolve(process_env_var)
                    .map(|resolved| resolved.value)
                    .map_err(anyhow::Error::from)
            })
            .transpose()?;
        let vault_guard = vault.try_read().ok();
        match resolve_slack_credentials_status_with_lookup(|name| {
            vault_guard
                .as_ref()
                .and_then(|vault| vault.get(name).map(str::to_string))
        }) {
            SlackCredentialResolution::Configured(credentials) => {
                info!(
                    default_channel_configured = default_channel.is_some(),
                    "Slack integration enabled"
                );
                chat_services.push(Arc::new(SlackService::new(
                    credentials.bot_token,
                    credentials.app_token,
                    default_channel,
                )));
            }
            SlackCredentialResolution::Missing { env_vars } => {
                info!(
                    missing_env_vars = %env_vars.join(","),
                    "Slack integration disabled; missing credentials"
                );
            }
        }
    } else {
        info!("Slack integration disabled by server configuration");
    }
}
```

In the struct literal replace:
```rust
slack_service,
slack_started: AtomicBool::new(false),
```
with:
```rust
chat_services,
chat_services_started: AtomicBool::new(false),
```

- [ ] **Step 1: Make the three changes described above**

- [ ] **Step 2: Verify the crate compiles**

```
cargo build -p fabro-server
```

Expected: will fail until the next tasks are done (missing `ChatEventContext` impl and startup function). That's fine — continue.

---

### Task 7: Implement `ChatEventContext` for `AppState`

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`

Add the `ChatEventContext` impl directly after the `AppState` struct definition. It uses
existing `AppState` methods and fields:

- [ ] **Step 1: Add the impl**

```rust
#[async_trait]
impl fabro_chat::ChatEventContext for AppState {
    async fn get_run_projection(
        &self,
        run_id: fabro_types::RunId,
    ) -> Option<Arc<fabro_store::RunProjection>> {
        self.store
            .get_cached_run(&run_id)
            .await
            .ok()
            .flatten()
            .map(|r| r.projection)
    }

    fn resolve_env(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name)
    }

    fn run_web_url(&self, run_id: &fabro_types::RunId) -> Option<String> {
        self.run_web_url(run_id)
    }

    fn canonical_origin(&self) -> Option<String> {
        self.canonical_origin().ok()
    }

    async fn get_run_events(
        &self,
        run_id: fabro_types::RunId,
        before_seq: u32,
    ) -> Vec<fabro_types::EventEnvelope> {
        let run_store = match self.store.open_run_reader(&run_id).await {
            Ok(s) => s,
            Err(err) => {
                warn!(
                    run_id = %run_id,
                    error = %err,
                    "Unable to open run reader for ChatEventContext::get_run_events"
                );
                return vec![];
            }
        };
        match run_store.list_events().await {
            Ok(events) => events.into_iter().take_while(|e| e.seq < before_seq).collect(),
            Err(err) => {
                warn!(
                    run_id = %run_id,
                    error = %err,
                    "Unable to list run events for ChatEventContext::get_run_events"
                );
                vec![]
            }
        }
    }
}
```

> **Note:** `run_web_url` on `AppState` is `pub(crate)` and returns `Option<String>`. The method name collision between the `ChatEventContext` method and the `AppState` method is fine — the impl just calls `self.run_web_url(run_id)` which resolves to the `AppState` inherent method.

- [ ] **Step 2: Verify compilation**

```
cargo build -p fabro-server
```

Expected: closer to compiling; may still fail on the startup function.

---

### Task 8: Replace `start_optional_slack_service` with `start_chat_services`

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`

- [ ] **Step 1: Replace the function**

Delete `start_optional_slack_service` (lines ~1647–1702) and replace with:

```rust
fn start_chat_services(state: &Arc<AppState>) {
    if state.chat_services_started.swap(true, Ordering::SeqCst) {
        return;
    }

    for service in &state.chat_services {
        // Event listener task
        let event_state = Arc::clone(state);
        let event_service = Arc::clone(service);
        tokio::spawn(async move {
            let mut rx = event_state.global_event_tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        event_service
                            .handle_event(&envelope, event_state.as_ref())
                            .await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Inbound loop task (WebSocket for Slack/Mattermost, immediate return for Teams)
        let inbound_state = Arc::clone(state);
        let inbound_service = Arc::clone(service);
        tokio::spawn(async move {
            let on_submit: Arc<dyn Fn(fabro_chat::AnswerSubmission) + Send + Sync> = {
                Arc::new(move |submission: fabro_chat::AnswerSubmission| {
                    let state = Arc::clone(&inbound_state);
                    tokio::spawn(async move {
                        handle_chat_answer_submission(state, submission).await;
                    });
                })
            };
            inbound_service.start(on_submit).await;
        });
    }
}

async fn handle_chat_answer_submission(
    state: Arc<AppState>,
    submission: fabro_chat::AnswerSubmission,
) {
    use std::str::FromStr;
    let Ok(run_id) = fabro_types::RunId::from_str(&submission.run_id) else {
        return;
    };
    let Ok(pending) = load_pending_interview(state.as_ref(), run_id, &submission.qid).await
    else {
        return;
    };
    let answer_submission =
        fabro_interview::AnswerSubmission::new(submission.answer, submission.actor);
    let _ = submit_pending_interview_answer(state.as_ref(), &pending, answer_submission).await;
}
```

- [ ] **Step 2: Update the call site**

Find where `start_optional_slack_service(&state)` is called inside `build_router_with_options`
and replace it with:

```rust
start_chat_services(&state);
```

- [ ] **Step 3: Remove any test-support that referenced `slack_service` or `slack_started`**

Search for `slack_service` and `slack_started` in test-support code:

```
grep -rn "slack_service\|slack_started" lib/crates/fabro-server/
```

Update any `TestAppStateBuilder` or fixture that set these fields to use `chat_services` /
`chat_services_started` instead.

- [ ] **Step 4: Compile**

```
cargo build -p fabro-server
```

Expected: PASS.

---

### Task 9: Add the webhook dispatch route

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`

- [ ] **Step 1: Add the handler function**

Add this function near `github_webhook_routes`:

```rust
async fn chat_webhook_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(provider): axum::extract::Path<String>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl axum::response::IntoResponse {
    let query_string = uri.query().unwrap_or("");
    let service = state
        .chat_services
        .iter()
        .find(|s| s.kind().as_str() == provider.as_str());
    let Some(service) = service else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let outcome = service
        .handle_webhook("", query_string, &headers, &body)
        .await;
    let status = axum::http::StatusCode::from_u16(outcome.status)
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    match outcome.body {
        Some(body) => (status, body).into_response(),
        None => status.into_response(),
    }
}
```

- [ ] **Step 2: Register the route in `build_router_with_options`**

Add the route outside the `principal_layer` nest (alongside the GitHub webhook route):

```rust
.route(
    "/api/v1/webhooks/:provider",
    axum::routing::post(chat_webhook_handler),
)
```

> The spec uses `/api/v1/webhooks/:provider/*rest` for future sub-path routing; for now,
> registering just `/:provider` is sufficient — add `/*rest` only when a provider needs it.

- [ ] **Step 3: Build and test**

```
cargo build -p fabro-server
cargo nextest run -p fabro-server
```

Expected: PASS. The conformance test does not need updating because this route is not in the
OpenAPI spec (it's a provider-internal webhook path, not a documented API surface).

- [ ] **Step 4: Commit all server changes**

```bash
git add lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): replace SlackService wiring with Vec<Arc<dyn ChatService>>"
```

---

### Task 10: Full validation

- [ ] **Step 1: Run the full workspace test suite**

```
cargo nextest run --workspace
```

Expected: PASS. Every existing test must pass — this is a pure refactor.

- [ ] **Step 2: Confirm Slack behavior is unchanged**

If you have a `.env` with Slack credentials, run the live E2E tests:

```
set -a && source .env && set +a && cargo nextest run -p fabro-slack --profile e2e --run-ignored only
```

- [ ] **Step 3: Check formatting**

```
cargo +nightly-2026-04-14 fmt --check --all
```

Fix if needed:
```
cargo +nightly-2026-04-14 fmt --all
git add -u && git commit -m "style: fmt after slack refactor"
```

- [ ] **Step 4: Run clippy**

```
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Fix any warnings before merging.
