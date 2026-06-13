# Chat Service Trait and Providers Design

**Date:** 2026-06-13
**Status:** Approved, pending implementation
**Supersedes:** `2026-06-10-mattermost-integration-design.md`

## Overview

Introduce a `ChatService` trait that abstracts over chat platform integrations (Slack,
Mattermost, MS Teams), then deliver Mattermost as a full implementation and MS Teams as a
verified stub. The Slack integration is refactored to implement the trait as the preparatory
step. This replaces the earlier design that proposed sibling concrete services with no shared
abstraction.

The immediate deliverable is Mattermost parity with Slack: run lifecycle notifications and
interactive human interview prompts. The trait exists so future providers (Teams, and others)
do not require changes to `fabro-server`'s core wiring.

## Design Principles

- Mirror the existing `SandboxProvider` pattern: shared trait crate, concrete implementations
  in provider crates, server holds `Vec<Arc<dyn ChatService>>`.
- Keep `axum` a dependency of `fabro-server` only. Provider crates use `http` crate types
  (`HeaderMap`, status codes) but never Axum router types.
- Every design decision for Mattermost defaults to mirroring Slack. Divergences are driven
  solely by API differences.
- MS Teams stubs must be real enough to prove the trait contract holds for a pure-inbound-HTTP
  provider (no outbound WebSocket).

## Crate Structure

### New crates

```
lib/crates/fabro-chat/     — ChatService trait and shared types
lib/crates/fabro-teams/    — MS Teams stub implementation
```

### Dependency graph

```
fabro-types
    ↑
fabro-interview
    ↑
fabro-chat          ← ChatService trait, AnswerSubmission, ChatProviderKind, WebhookOutcome
    ↑         ↑         ↑
fabro-slack  fabro-mattermost  fabro-teams
         ↑         ↑         ↑
              fabro-server
```

`fabro-chat` depends on `fabro-interview` (for `Answer`) and `fabro-types` (for `Principal`,
`RunId`, `EventEnvelope`). It does not depend on `axum`.

`fabro-server` replaces `Option<Arc<SlackService>>` with `Vec<Arc<dyn ChatService>>`.

## `fabro-chat` — Trait and Shared Types

### `ChatService` trait

```rust
#[async_trait]
pub trait ChatService: Send + Sync {
    fn kind(&self) -> ChatProviderKind;

    /// Start the provider's inbound event loop.
    /// Slack: spawns Socket Mode WebSocket loop.
    /// Mattermost: spawns WebSocket loop.
    /// Teams: no-op — Teams delivers events via inbound HTTP only.
    async fn start(&self, on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>);

    /// Handle an outbound Fabro event (send notifications, post interview questions).
    async fn handle_event(
        &self,
        envelope: &EventEnvelope,
        context: &dyn ChatEventContext,
    );

    /// Handle an inbound HTTP webhook (button actions, Teams activities).
    /// Slack always returns 404 — it has no inbound HTTP surface.
    async fn handle_webhook(
        &self,
        sub_path: &str,
        headers: &HeaderMap,
        body: &Bytes,
    ) -> WebhookOutcome;

    fn connection_status(&self) -> IntegrationConnectionStatus;
}
```

### `ChatEventContext` trait

Breaks the circular dependency between `fabro-chat` and `fabro-server`. `AppState` implements
this in `fabro-server`; provider crates never see `AppState`.

```rust
#[async_trait]
pub trait ChatEventContext: Send + Sync {
    async fn get_run_projection(&self, run_id: RunId) -> Option<Arc<RunProjection>>;
    fn resolve_env(&self, name: &str) -> Option<String>;
}
```

### Shared types

**`ChatProviderKind`** — enum used for webhook dispatch and logging:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChatProviderKind { Slack, Mattermost, Teams }
```

**`AnswerSubmission`** — unified replacement for `SlackAnswerSubmission` and the planned
`MattermostAnswerSubmission`:
```rust
pub struct AnswerSubmission {
    pub run_id: String,
    pub qid:    String,
    pub answer: Answer,
    pub actor:  Principal,
}
```

**`WebhookOutcome`** — avoids Axum types in provider crates:
```rust
pub struct WebhookOutcome {
    pub status: u16,
    pub body:   Option<String>,
}
```

### Test support

`fabro-chat/src/test_support.rs` behind `#[cfg(any(test, feature = "test-support"))]`:
- `MockChatEventContext` — configurable fake projection and env vars for testing `handle_event`
  in provider crates without a database.
- `assert_chat_service_contract(service: &dyn ChatService)` — thin conformance checker:
  `kind()` returns a valid variant, `connection_status()` doesn't panic, `handle_webhook` with
  an empty body returns a valid status code. No setup hidden in the helper.

## Phase 1: Slack Refactor

### What moves

`SlackService` (currently a large private struct in `fabro-server/src/server.rs`) moves to
`lib/crates/fabro-slack/src/service.rs` and implements `ChatService`.

The internal modules (`client`, `connection`, `dispatch`, `blocks`, `threads`, `payload`,
`socket`) are unchanged.

### `impl ChatService for SlackService`

| Method | Implementation |
|---|---|
| `kind()` | `ChatProviderKind::Slack` |
| `start(on_submit)` | existing Socket Mode WebSocket loop |
| `handle_event(envelope, context)` | existing logic; `state.store.get_cached_run()` → `context.get_run_projection()`; `state.env_lookup` → `context.resolve_env()` |
| `handle_webhook(...)` | returns `WebhookOutcome { status: 404, body: None }` — Slack has no inbound HTTP surface |
| `connection_status()` | existing method, moved as-is |

The only meaningful code change is threading `ChatEventContext` through `handle_event`. Every
call to `state.store.get_cached_run()` becomes `context.get_run_projection()`, and
`(state.env_lookup)(name)` becomes `context.resolve_env(name)`.

## Phase 2: Mattermost Implementation

### Config & secrets

**Server config:**
```toml
[server.integrations.mattermost]
enabled = true
url = "https://mm.example.com"       # Mattermost server base URL
team = "myteam"                       # team name for channel-name → ID resolution
default_channel = "fabro-alerts"      # channel for interview questions
```

`MattermostIntegrationSettings` in `fabro-types/src/settings/server.rs`:
```rust
pub struct MattermostIntegrationSettings {
    pub enabled:         bool,           // default: true
    pub url:             Option<InterpString>,
    pub team:            Option<InterpString>,
    pub default_channel: Option<InterpString>,
}
```

**Vault secrets** (registered in `OPTIONAL_VAULT_SECRETS`):

| Secret | Purpose |
|---|---|
| `FABRO_MATTERMOST_TOKEN` | Bot/personal access token for REST API (`Authorization: Bearer`) and WebSocket authentication. Single token — no equivalent of Slack's separate app token. |
| `FABRO_MATTERMOST_WEBHOOK_SECRET` | Random token embedded as `?token=` in every button's `integration.url`. Handler verifies with constant-time compare before processing. |

Both must be present for the integration to be `Configured`. Missing either disables it with a
startup log line listing which are absent, parallel to Slack's credential resolution.

**Per-run settings** (reuses existing structs unchanged):
```toml
[run.notifications.on-start]
provider = "mattermost"
events = ["run.started"]
[run.notifications.on-start.mattermost]
channel = "fabro-alerts"

[run.interviews]
provider = "mattermost"
[run.interviews.mattermost]
channel = "fabro-reviews"   # exists in schema; unused by server today (mirrors Slack behavior)
```

### `fabro-mattermost` modules

| Module | Purpose | Slack parallel |
|---|---|---|
| `service.rs` | `MattermostService` struct + `impl ChatService` | `SlackService` |
| `client.rs` | `MattermostClient`: REST operations + channel-name-to-ID cache | `client.rs` |
| `attachments.rs` | Build `props.attachments` JSON for interview prompts and lifecycle events | `blocks.rs` |
| `connection.rs` | WebSocket loop: authenticate, read `posted` events, reconnect with backoff | `connection.rs` |
| `dispatch.rs` | Classify incoming WS events → `DispatchAction` | `dispatch.rs` |
| `threads.rs` | `ThreadRegistry`: post-ID → `(run_id, qid)` for thread-reply correlation | `threads.rs` |
| `webhook.rs` | Parse and token-verify inbound button action HTTP payloads | (no Slack parallel — Slack uses Socket Mode) |
| `lib.rs` | `MattermostCredentials`, `MattermostCredentialResolution`, credential resolution | `config.rs` |

### REST client (`client.rs`)

```rust
pub struct MattermostClient {
    token:         String,
    base_url:      String,
    http:          fabro_http::HttpClient,
    channel_cache: Mutex<HashMap<String, String>>,  // "team/name" → channel_id
}
```

Methods:
- `post_message(channel_id, message, attachments, root_id?)` → `POST /api/v4/posts`
  - Body: `{ channel_id, message, props: { attachments }, root_id }` (`root_id` omitted when `None`)
  - Returns `PostedMessage { post_id, channel_id }`
- `update_post(post_id, message, attachments)` → `PUT /api/v4/posts/{post_id}`
  - *Diverges from Slack*: Mattermost uses `PUT` to a post-ID URL; Slack uses `POST /chat.update` with channel+ts.
- `resolve_channel(team, name)` → `GET /api/v4/teams/name/{team}/channels/name/{name}`
  - Result cached keyed on `"{team}/{name}"`. Cache populated on first use; no invalidation (channel IDs are stable).

`PostedMessage { post_id: String, channel_id: String }` — parallel to Slack's
`PostedMessage { channel_id: String, ts: String }`. `post_id` serves the same role as `ts`.

### Message formatting (`attachments.rs`)

Mattermost uses `props.attachments` (Slack-compatible attachment objects) instead of Block Kit.

**`question_to_attachments(run_id, qid, question, run_web_url, webhook_url)`**

One attachment:
- `color`: `"#0072C6"` (neutral blue)
- `title`: question text, truncated to 200 chars (Mattermost attachment title limit)
- `text`: `context_display` if present, truncated to 8000 chars
- `actions`: button objects
  - Yes/No: two buttons, `integration.context: { run_id, qid, kind: "yes"|"no" }`
  - Multiple-choice: one button per option, `kind: "selected"`, `key: <option_key>`
  - Freeform: no buttons — user replies in thread
  - Multi-select: note in `text`, thread-based reply
  - Each button's `integration.url` is derived at event-handling time from `state.canonical_origin()` + `?token={FABRO_MATTERMOST_WEBHOOK_SECRET}`

**`run_lifecycle_attachments(kind, details)`**

One attachment per event:
- `color`: `"#36a64f"` started/completed, `"#cc0000"` failed
- `title`: event label + workflow name
- `text`: workflow label, run ID, result (completed/failed), duration, PR link if present

**`answered_attachments(question_text, answer_text)`**

Replaces the original question attachment after an answer is received.

### WebSocket inbound — thread replies (`connection.rs`)

1. Connect: `tokio_tungstenite::connect_async("wss://<host>/api/v4/websocket")`
2. Authenticate: send `{ "seq": 1, "action": "authentication_challenge", "data": { "token": "<FABRO_MATTERMOST_TOKEN>" } }`
3. Loop: read messages, dispatch, respond to `Ping` with `Pong`, break on `Close`
4. On `posted` event: `data.post` is a JSON-encoded string (a second `serde_json::from_str`
   parse is required — *diverges from Slack*, which sends structured JSON directly)
5. Ignore posts where `type` is non-empty (system messages)
6. Look up `root_id` in `ThreadRegistry` → `(run_id, qid)` → call `on_submit`

Reconnect/backoff: 1s initial, 2× doubling, 30s max — structurally identical to Slack.

`ConnectionStatusUpdate { Connecting, Connected, Error(String) }` and `ConnectionStatusSink`
mirror Slack exactly.

### WS dispatch (`dispatch.rs`)

```rust
pub enum DispatchAction {
    Connected,
    SubmitAnswer(Box<AnswerSubmission>),
    Reconnect,
    Ignored,
}
```

`hello` → `Connected`, `posted` with registered `root_id` → `SubmitAnswer`, `goodbye` →
`Reconnect`, everything else → `Ignored`.

### Thread registry (`threads.rs`)

Structural copy of `fabro-slack/src/threads.rs`, keyed on Mattermost `post_id` (root post ID)
instead of Slack `ts`. Registered when a freeform or `allow_freeform` interview question is
posted; removed when the interview is completed or timed out.

### Button webhook (`webhook.rs`)

Mattermost POSTs `application/json` to `integration.url` when a button is clicked:

```json
{
  "channel_id": "...", "team_id": "...", "user_id": "...", "user_name": "...",
  "context": { "run_id": "...", "qid": "...", "kind": "yes|no|selected", "key": "..." }
}
```

`parse_action(payload)` → `Option<AnswerSubmission>`:
- Extracts `context.run_id`, `context.qid`, `context.kind`, `context.key`
- Builds `Answer` from `kind` + `key`; returns `None` for unknown kinds
- Builds `actor: Principal::Mattermost { team_id, user_id, user_name }`

`constant_time_eq(a: &[u8], b: &[u8]) -> bool` — constant-time byte comparison for token
verification. The handler extracts the `token` query parameter and compares against the vault
secret before touching the body. Mismatched or missing tokens → `WebhookOutcome { status: 401 }`.

### `MattermostService` (`service.rs`)

```rust
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
```

`on_submit` is stored after `start()` is called so `handle_webhook` can invoke it when a button
is clicked (the only path where button-action answers arrive, separate from the WS loop).

`handle_event` dispatches on `EventBody`:
- `InterviewStarted` → resolve channel, post question attachment, register thread (freeform/allow_freeform), store `PostedMessage`
- `InterviewCompleted / Timeout / Interrupted` → remove from `posted_messages`, remove from `thread_registry`, update post with `answered_attachments`
- `RunStarted / RunCompleted / RunFailed` → filter notification routes where `provider == "mattermost"` and event matches, resolve channel, post lifecycle attachment

## Phase 3: MS Teams Stubs

### Purpose

The stub must be complete enough to prove the `ChatService` trait contract holds for a
provider with no outbound connection. It exercises the `start()` no-op path and the
`handle_webhook()` as sole event delivery mechanism.

### Config & secrets

```toml
[server.integrations.teams]
enabled = true
```

`TeamsIntegrationSettings` added to `ServerIntegrationsSettings` beside `slack` and
`mattermost`:

```rust
pub struct TeamsIntegrationSettings {
    pub enabled: bool,   // default: true
}
```

Vault secrets:
| Secret | Purpose |
|---|---|
| `FABRO_TEAMS_WEBHOOK_SECRET` | Future: validate Microsoft JWT bearer tokens. Stub: unused but required for credential resolution to return `Configured`. |

### `fabro-teams` modules

Minimal crate with `service.rs` and `lib.rs` only:
- `lib.rs` — `TeamsCredentials`, `TeamsCredentialResolution`, `resolve_credentials_status_with_lookup`
- `service.rs` — `TeamsService` struct + `impl ChatService`

### `impl ChatService for TeamsService`

| Method | Implementation |
|---|---|
| `kind()` | `ChatProviderKind::Teams` |
| `start(on_submit)` | documented no-op; logs `"Teams: no outbound connection (pure inbound HTTP)"` |
| `handle_event(envelope, context)` | logs `"Teams notifications not yet implemented"` and returns |
| `handle_webhook(sub_path, headers, body)` | returns `WebhookOutcome { status: 200, body: None }` — accepts any POST without validation (stub behavior) |
| `connection_status()` | always returns `IntegrationConnectionStatus { status: Connected, .. }` — no connection to track |

The stub confirms: the trait contract is valid for a pure-inbound-HTTP provider. Real Teams
JWT validation, Adaptive Card formatting, and event routing are deferred.

## Server Wiring (`fabro-server`)

### `AppState` changes

**Removed:**
```rust
slack_service: Option<Arc<SlackService>>,
slack_started: AtomicBool,
```

**Added:**
```rust
chat_services:         Vec<Arc<dyn ChatService>>,
chat_services_started: AtomicBool,
```

### `AppState` implements `ChatEventContext`

```rust
#[async_trait]
impl ChatEventContext for AppState {
    async fn get_run_projection(&self, run_id: RunId) -> Option<Arc<RunProjection>> {
        self.store.get_cached_run(&run_id).await.ok().flatten().map(|r| r.projection)
    }
    fn resolve_env(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name)
    }
}
```

### Service startup

Single `start_chat_services(state: &Arc<AppState>)` function replaces
`start_optional_slack_service` and `start_optional_mattermost_service`. For each service in
`chat_services` it spawns two Tokio tasks:

1. **Event listener** — subscribes to `global_event_tx`, calls `service.handle_event(envelope, state.as_ref())` for each event.
2. **Inbound loop** — calls `service.start(on_submit)`. For Slack and Mattermost this runs
   forever (WebSocket with reconnect). For Teams it returns immediately.

### Webhook dispatch

Single route outside the `principal_layer` nest:

```
POST /api/v1/webhooks/:provider/*rest
```

Handler:
1. Extract `:provider` path segment
2. Find service in `chat_services` where `service.kind().as_str() == provider`
3. Call `service.handle_webhook(rest, &headers, &body).await`
4. Convert `WebhookOutcome` to HTTP response

Unknown provider → 404. Slack → 404 (its `handle_webhook` always returns 404).

### `http_log_middleware`

`Principal::Mattermost` and `Principal::Teams` arms added alongside `Principal::Slack`.

### AppState construction

For each provider: resolve integration settings, resolve credentials from vault, log enabled /
disabled / missing-credentials, push `Arc<dyn ChatService>` into `chat_services`.

## `fabro-types` changes

### `Principal`

Add immediately after `Principal::Slack`:
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

`kind()` and `display()` updated for both variants.

### Settings

`ServerIntegrationsSettings`:
```rust
pub struct ServerIntegrationsSettings {
    pub github:     GithubIntegrationSettings,
    pub slack:      SlackIntegrationSettings,
    pub mattermost: MattermostIntegrationSettings,
    pub teams:      TeamsIntegrationSettings,
}
```

`NotificationRouteSettings` and `RunInterviewsSettings`: add `mattermost` and `teams`
optional provider settings fields parallel to existing `slack` fields.

## `fabro-static` changes

New constants in `EnvVars`, registered in `OPTIONAL_VAULT_SECRETS` (alphabetical order):
- `FABRO_MATTERMOST_TOKEN`
- `FABRO_MATTERMOST_WEBHOOK_SECRET`
- `FABRO_TEAMS_WEBHOOK_SECRET`

## Testing Strategy

### Layers

All chat integration tests are implementation-facing and belong at the unit / crate-level layer.
No CLI integration tests (`cmd/*`, `workflow/*`, `scenario/*`) are required — there is no
user-visible CLI surface for chat integrations.

### `fabro-chat`

- Shared types round-trip: `AnswerSubmission`, `WebhookOutcome`, `ChatProviderKind` serialize
  and deserialize correctly.
- `MockChatEventContext` is available from `test-support` feature; verified in `fabro-slack`
  and `fabro-mattermost` unit tests.

### `fabro-slack`

- `assert_chat_service_contract(&slack_service)` passes.
- `handle_event` with `MockChatEventContext` for each `EventBody` variant of interest.
- `handle_webhook` returns `WebhookOutcome { status: 404 }`.
- Connection status transitions: `Connecting` → `Connected` → `Error` → `Connecting`.

### `fabro-mattermost`

- `assert_chat_service_contract(&mattermost_service)` passes.
- Credential resolution: all combinations of present/absent/empty secrets.
- `client.rs`: `parse_post_response`, channel cache hit on second call.
- `attachments.rs`: `question_to_attachments` for each `QuestionType`, `run_lifecycle_attachments`
  for each `RunLifecycleKind`, `answered_attachments`, truncation at limits — all asserted with
  `insta` inline JSON snapshots.
- `connection.rs`: `process_message` for `hello`, `posted` (registered + unregistered thread),
  `goodbye`, malformed JSON; `wss_url_from_base` for `https://` and `http://` inputs.
- `dispatch.rs`: table-driven over all `DispatchAction` variants.
- `webhook.rs`: `parse_action` for yes/no/selected; missing fields return `None`;
  `constant_time_eq` correct/incorrect/different-length inputs.
- `handle_webhook`: correct token → `200`, wrong token → `401`, missing token → `401` —
  asserted with `expect_axum_status` helpers from `fabro-test`, not raw `assert_eq!`.

Live provider tests requiring a real Mattermost instance:
```rust
#[e2e_test(live("FABRO_MATTERMOST_TOKEN"))]
async fn mattermost_posts_lifecycle_notification() { ... }
```

### `fabro-teams`

- `assert_chat_service_contract(&teams_service)` passes.
- `start()` returns immediately without spawning tasks.
- `handle_webhook` returns `WebhookOutcome { status: 200 }` for any input.
- `connection_status()` always returns `Connected`.

### `fabro-server`

- Webhook dispatch route: `POST /api/v1/webhooks/mattermost` with correct token → 200; wrong
  token → 401; `POST /api/v1/webhooks/slack` → 404; unknown provider → 404.
- Existing conformance test updated to include `/api/v1/webhooks/:provider/*rest` route.
- HTTP assertions use `expect_axum_status` / `expect_axum_ok_json` from `fabro-test`.

### Manual integration test (Mattermost)

Against `docker run --name mattermost-preview -p 8065:8065 mattermost/mattermost-preview`:
1. Create a bot account, generate a personal access token → `fabro secret set FABRO_MATTERMOST_TOKEN <token>`
2. Generate a random secret → `fabro secret set FABRO_MATTERMOST_WEBHOOK_SECRET <secret>`
3. Configure `[server.integrations.mattermost]` pointing at `http://localhost:8065`, team `ad-1`, channel `town-square`
4. Start Fabro server; confirm "Mattermost integration enabled" in logs and WebSocket connects
5. Trigger a run; confirm lifecycle notification appears in `town-square`
6. Trigger a run with a yes/no interview; confirm question with buttons; click Yes; confirm post updates and answer is recorded
7. Trigger a run with a freeform interview; reply in thread; confirm answer is recorded
8. Confirm Slack integration is unaffected throughout

## Docs

- New: `docs/public/integrations/mattermost.mdx` (mirrors Slack integration page)
- New: `docs/public/integrations/teams.mdx` (stub — notes Teams support is coming)
- Updated: `docs/public/administration/server-configuration.mdx` — add Mattermost and Teams sections and new secrets to the secrets table
- Updated: `.env.example` — add `FABRO_MATTERMOST_TOKEN`, `FABRO_MATTERMOST_WEBHOOK_SECRET`, `FABRO_TEAMS_WEBHOOK_SECRET` (commented out)

## Out of Scope

- Full MS Teams implementation (Adaptive Cards, JWT validation, Bot Framework activity routing).
- Mattermost OAuth app flow. Personal access token / bot token only.
- Multi-team routing. One `team` per Fabro server; all channels must be in that team.
- Mattermost slash commands or outgoing webhooks as an alternative inbound path.
- A fourth chat provider beyond Slack, Mattermost, and Teams.
