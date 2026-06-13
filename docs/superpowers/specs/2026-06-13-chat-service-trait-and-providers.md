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

> **Why this spec looks the way it does:** Every significant design choice is documented in the
> [Decision Log appendix](#appendix-decision-log) at the end of this document, ordered from most
> to least architecturally significant. Inline links throughout the spec (`→ Decision N`) point
> to the relevant entry.

## Design Principles

- Mirror the existing `SandboxProvider` pattern: shared trait crate, concrete implementations
  in provider crates, server holds `Vec<Arc<dyn ChatService>>`. ([→ Decision 1](#decision-1-chatservice-trait), [→ Decision 5](#decision-5-vec-of-chatservice-in-appstate))
- Keep `axum` a dependency of `fabro-server` only. Provider crates use `http` crate types
  (`HeaderMap`, status codes) but never Axum router types. ([→ Decision 4](#decision-4-handle_webhook-method-vs-router-fragment), [→ Decision 8](#decision-8-webhookoutcome-struct-instead-of-axum-response))
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

([→ Decision 2](#decision-2-fabro-chat-crate-placement))

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

`fabro-server` replaces `Option<Arc<SlackService>>` with `Vec<Arc<dyn ChatService>>`. ([→ Decision 5](#decision-5-vec-of-chatservice-in-appstate))

## `fabro-chat` — Trait and Shared Types

### `ChatService` trait ([→ Decision 1](#decision-1-chatservice-trait))

```rust
#[async_trait]
pub trait ChatService: Send + Sync {
    fn kind(&self) -> ChatProviderKind;

    /// Start the provider's inbound event loop.
    /// Slack: spawns Socket Mode WebSocket loop.
    /// Mattermost: spawns WebSocket loop.
    /// Teams: no-op — Teams delivers events via inbound HTTP only. (→ Decision 6, 7)
    async fn start(&self, on_submit: Arc<dyn Fn(AnswerSubmission) + Send + Sync>);

    /// Handle an outbound Fabro event (send notifications, post interview questions).
    async fn handle_event(
        &self,
        envelope: &EventEnvelope,
        context: &dyn ChatEventContext,  // → Decision 3
    );

    /// Handle an inbound HTTP webhook (button actions, Teams activities).
    /// Slack always returns 404 — it has no inbound HTTP surface. (→ Decision 4)
    async fn handle_webhook(
        &self,
        sub_path: &str,
        headers: &HeaderMap,
        body: &Bytes,
    ) -> WebhookOutcome;  // → Decision 8

    fn connection_status(&self) -> IntegrationConnectionStatus;
}
```

### `ChatEventContext` trait ([→ Decision 3](#decision-3-chateventcontext-for-circular-dependency))

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

**`WebhookOutcome`** — avoids Axum types in provider crates ([→ Decision 8](#decision-8-webhookoutcome-struct-instead-of-axum-response)):
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
| `handle_event(envelope, context)` | existing logic; `state.store.get_cached_run()` → `context.get_run_projection()`; `state.env_lookup` → `context.resolve_env()` ([→ Decision 3](#decision-3-chateventcontext-for-circular-dependency)) |
| `handle_webhook(...)` | returns `WebhookOutcome { status: 404, body: None }` — Slack has no inbound HTTP surface ([→ Decision 4](#decision-4-handle_webhook-method-vs-router-fragment)) |
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
| `FABRO_MATTERMOST_TOKEN` | Bot/personal access token for REST API (`Authorization: Bearer`) and WebSocket authentication. Single token — no equivalent of Slack's separate app token. ([→ Decision 9](#decision-9-single-mattermost-token)) |
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
([→ Decision 10](#decision-10-on_submit-stored-on-service))

`handle_event` dispatches on `EventBody`:
- `InterviewStarted` → resolve channel, post question attachment, register thread (freeform/allow_freeform), store `PostedMessage`
- `InterviewCompleted / Timeout / Interrupted` → remove from `posted_messages`, remove from `thread_registry`, update post with `answered_attachments`
- `RunStarted / RunCompleted / RunFailed` → filter notification routes where `provider == "mattermost"` and event matches, resolve channel, post lifecycle attachment

## Phase 3: MS Teams Stubs

### Purpose

The stub must be complete enough to prove the `ChatService` trait contract holds for a
provider with no outbound connection. It exercises the `start()` no-op path and the
`handle_webhook()` as sole event delivery mechanism. ([→ Decision 6](#decision-6-ms-teams-stubs-included-now))

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
| `start(on_submit)` | documented no-op; logs `"Teams: no outbound connection (pure inbound HTTP)"` ([→ Decision 7](#decision-7-start-required-even-for-no-op-providers)) |
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

**Added:** ([→ Decision 5](#decision-5-vec-of-chatservice-in-appstate))
```rust
chat_services:         Vec<Arc<dyn ChatService>>,
chat_services_started: AtomicBool,
```

### `AppState` implements `ChatEventContext` ([→ Decision 3](#decision-3-chateventcontext-for-circular-dependency))

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

### Webhook dispatch ([→ Decision 4](#decision-4-handle_webhook-method-vs-router-fragment))

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
  `insta` inline JSON snapshots. ([→ Decision 11](#decision-11-fabro-idiomatic-test-assertions))
- `connection.rs`: `process_message` for `hello`, `posted` (registered + unregistered thread),
  `goodbye`, malformed JSON; `wss_url_from_base` for `https://` and `http://` inputs.
- `dispatch.rs`: table-driven over all `DispatchAction` variants.
- `webhook.rs`: `parse_action` for yes/no/selected; missing fields return `None`;
  `constant_time_eq` correct/incorrect/different-length inputs.
- `handle_webhook`: correct token → `200`, wrong token → `401`, missing token → `401` —
  asserted with `expect_axum_status` helpers from `fabro-test`, not raw `assert_eq!`. ([→ Decision 11](#decision-11-fabro-idiomatic-test-assertions))

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
- HTTP assertions use `expect_axum_status` / `expect_axum_ok_json` from `fabro-test`. ([→ Decision 11](#decision-11-fabro-idiomatic-test-assertions))

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

---

## Appendix: Decision Log

Decisions are ordered from most architecturally significant (shapes the whole design) to minor
tactical choices.

---

### Decision 1: ChatService Trait

**Decision:** Extract a shared `ChatService` trait that Slack, Mattermost, and Teams all
implement, rather than leaving each integration as an independent concrete struct wired
separately into `server.rs`.

**Why:** The original Mattermost design (superseded by this one) proposed a
`MattermostService` struct sitting beside `SlackService` in `server.rs` — no shared
abstraction. Bryan Helmkamp's review noted this approach does not scale: adding MS Teams would
require a third round of identical server wiring, and provider-specific details would keep
leaking into `server.rs`. A trait enforces the boundary and makes adding future providers
additive-only changes to the server.

**Alternatives considered:** Keeping sibling concrete services was the original design. It is
simpler in the short term but makes `server.rs` grow with each new provider and creates no
pressure to keep provider code self-contained.

---

### Decision 2: fabro-chat Crate Placement

**Decision:** The `ChatService` trait lives in a new `lib/crates/fabro-chat/` crate. Provider
crates depend on it and implement it. `fabro-server` depends on it and holds
`Vec<Arc<dyn ChatService>>`.

**Why:** This mirrors the existing `fabro-sandbox` / `SandboxProvider` pattern exactly —
the most referenced multi-provider abstraction in the codebase. Three alternatives were
considered:

- **Trait in `fabro-types`:** `fabro-types` is a pure-types crate with no async dependencies.
  Adding async trait methods and `http::HeaderMap` would pull the crate in the wrong direction.
- **Trait in `fabro-server` (server-internal):** The trait cannot be tested outside the server,
  and provider crates cannot implement it without depending on `fabro-server`, which creates a
  circular dependency (`fabro-server` → `fabro-slack` → `fabro-server`).
- **New `fabro-chat` crate:** Clean layering, no circular dependencies, testable in isolation.
  The new crate analogy to `fabro-sandbox` made this feel natural rather than novel.

---

### Decision 3: ChatEventContext for Circular Dependency

**Decision:** `handle_event` takes `&dyn ChatEventContext` instead of `&AppState`. `AppState`
implements `ChatEventContext` in `fabro-server`.

**Why:** `handle_event` needs to fetch run projections and resolve environment variables — both
live in `AppState`. Putting `AppState` directly in the trait signature would require
`fabro-chat` to depend on `fabro-server`, which depends on `fabro-chat` — a circular dependency
Rust's crate system forbids.

`ChatEventContext` is a narrow interface (`get_run_projection`, `resolve_env`) defined in
`fabro-chat`. `AppState` satisfies it in `fabro-server`. Provider crates never see `AppState`.
This is the standard Rust pattern for breaking large dependency cycles via trait objects — the
equivalent of defining a Ruby module in a gem that `ApplicationController` then `include`s.

---

### Decision 4: handle_webhook Method vs Router Fragment

**Decision:** Each provider implements `handle_webhook(sub_path, headers, body) -> WebhookOutcome`
on the trait. The server owns a single `POST /api/v1/webhooks/:provider/*rest` route and
dispatches to the right service. Providers do not return Axum `Router` fragments.

**Why:** Two options were considered:

- **Option A — `fn router(&self, state: Arc<AppState>) -> Option<Router>`:** Each provider
  builds and returns Axum routes. Maximally flexible — providers own their full HTTP surface.
  But it requires every provider crate to depend on `axum`, which today is a dependency of
  `fabro-server` only. It also couples the trait to `AppState` via the signature.
- **Option B — `handle_webhook` method (chosen):** The server owns all Axum machinery.
  Provider crates use `http` crate types (`HeaderMap`, status codes) but never `axum`. The
  `AppState` decoupling is preserved. The existing `github_webhook_routes` function in
  `server.rs` — a plain function that returns a `Router` and gets `.merge()`d in — showed that
  HTTP routing is already treated as a server-level concern in this codebase.

The `*rest` wildcard sub-path gives each provider a namespaced URL space without requiring
full router ownership.

---

### Decision 5: Vec of ChatService in AppState

**Decision:** `AppState` holds `chat_services: Vec<Arc<dyn ChatService>>` rather than
`slack_service: Option<Arc<SlackService>>`, `mattermost_service: Option<Arc<MattermostService>>`,
etc.

**Why:** Named fields require `server.rs` to be modified for every new provider — exactly the
coupling the trait exists to eliminate. A `Vec` makes adding a fourth provider a zero-change
operation in `server.rs`. Dispatch in the webhook handler and event loop iterates the vec;
`kind()` is the discriminant. This mirrors how `SandboxProviderRegistry` holds
`Vec<Arc<dyn SandboxProvider>>`.

---

### Decision 6: MS Teams Stubs Included Now

**Decision:** A `fabro-teams` stub crate is included in this design even though it ships no
real Teams functionality.

**Why:** The `ChatService` trait needs to be validated against all three event-delivery shapes
before the implementation plan is written. Slack and Mattermost both have an outbound WebSocket
loop; Teams does not — it is a pure-inbound-HTTP provider. Without a Teams stub, the trait
could accidentally encode WebSocket assumptions that would require a breaking trait change when
Teams is eventually implemented. The stub makes those assumptions visible (e.g. that `start()`
must be a valid no-op) before any code is written.

---

### Decision 7: start() Required Even for No-op Providers

**Decision:** `start()` is a required method on the trait. Teams' implementation is a
documented no-op that logs one line and returns immediately.

**Why:** The alternative — making `start()` optional via a default method — would obscure the
contract for providers that do have an inbound connection loop. Every provider must declare how
it handles inbound events; "I have no connection to start" is a valid answer and is expressed
clearly by a no-op implementation rather than by omitting the method. A no-op that logs is
also operationally useful: it confirms in server logs that Teams is active and waiting for
inbound HTTP.

---

### Decision 8: WebhookOutcome Struct Instead of Axum Response

**Decision:** `handle_webhook` returns a lightweight `WebhookOutcome` struct defined in
`fabro-chat`, not an Axum `Response`.

**Why:** Returning `axum::response::Response` from the trait would require every provider
crate to depend on `axum` — the same problem as the router fragment option (Decision 4). A
plain struct with a status code and optional body is sufficient for all three providers' needs
and is convertible to an Axum response in `fabro-server`'s dispatch handler with two lines.

---

### Decision 9: Single Mattermost Token

**Decision:** One vault secret covers both REST API calls and WebSocket authentication.

**Why:** Mattermost has no equivalent of Slack's Socket Mode app token. Slack requires two
tokens because Socket Mode uses a separate app-level token to open the WebSocket, distinct from
the bot token used for API calls. Mattermost's WebSocket authentication uses the same bearer
token as the REST API. Introducing a second Mattermost token would have no purpose and would
add operator confusion.

---

### Decision 10: on_submit Stored on Service

**Decision:** `MattermostService` stores `on_submit` in an `Arc<Mutex<Option<...>>>` field
populated when `start()` is called. `handle_webhook` reads it from the field.

**Why:** The trait's `handle_webhook` signature does not include `on_submit` — it has no
`on_submit` parameter. Adding one would make the signature inconsistent with Slack (which never
calls `on_submit` from `handle_webhook`) and would expose a callback that most providers do not
need. Storing it after `start()` is the minimal coupling: `handle_webhook` can fire the
callback without the server needing to pass it explicitly on every HTTP request.

---

### Decision 11: Fabro-Idiomatic Test Assertions

**Decision:** Attachment JSON is asserted with `insta::assert_json_snapshot!`. HTTP status
assertions in server tests use `expect_axum_status` / `expect_axum_ok_json` from `fabro-test`.

**Why:** The existing Fabro testing strategy (`docs/internal/testing-strategy.md`) specifies:
prefer snapshots over ad hoc assertions for structured output; use shared HTTP assertion helpers
rather than raw `assert_eq!(status, ...)`. Attachment objects are structured JSON payloads —
exactly the "good structured snapshot target" the strategy describes. The `expect_axum_*`
helpers include the request shape in failure output, making test failures easier to diagnose.
Following these conventions keeps new tests consistent with the rest of the test suite.
