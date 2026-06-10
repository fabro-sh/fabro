# Mattermost Integration Design

**Date:** 2026-06-10
**Status:** Approved, pending implementation

## Overview

Add Mattermost as a first-class notification and interview backend, feature-parallel to the
existing Slack integration. A Fabro server can drive run lifecycle notifications and interactive
human interview prompts through Mattermost without using Slack at all.

This is a native Mattermost backend using the Mattermost REST API (`/api/v4`) and WebSocket
event API directly — not a Slack-compatibility proxy.

## Design Principles

Every design decision defaults to mirroring the existing Slack implementation. Divergences from
Slack's design are noted explicitly and are driven solely by Mattermost API differences, not by
preference.

## Architecture

### New crate: `lib/crates/fabro-mattermost/`

Mirrors `lib/crates/fabro-slack/` module for module.

| Module | Purpose | Slack parallel |
|---|---|---|
| `lib.rs` | `MattermostOptions`, `MattermostCredentials`, `MattermostCredentialResolution`, vault credential resolution | `config.rs` |
| `client.rs` | `MattermostClient`: REST operations + channel-name-to-ID cache | `client.rs` |
| `attachments.rs` | Build Mattermost `props.attachments` JSON for interview prompts and lifecycle events | `blocks.rs` |
| `connection.rs` | WebSocket loop: authenticate, read `posted` events, reconnect with backoff | `connection.rs` |
| `dispatch.rs` | Classify incoming WS events → `DispatchAction` | `dispatch.rs` |
| `threads.rs` | `ThreadRegistry`: post-ID → `(run_id, qid)` for thread-reply correlation | `threads.rs` |
| `webhook.rs` | Parse and token-verify inbound button action HTTP payloads | (no direct parallel — Slack uses Socket Mode for this) |

### Changes to existing crates

**`fabro-types`**
- Add `Principal::Mattermost { team_id: String, user_id: String, user_name: Option<String> }` variant (parallel to `Principal::Slack`)
- Add `MattermostIntegrationSettings` to `ServerIntegrationsSettings` beside `slack`
- Add `mattermost: Option<NotificationProviderSettings>` field to `NotificationRouteSettings` (reuses existing struct)
- Add `mattermost: Option<InterviewProviderSettings>` field to `RunInterviewsSettings` (reuses existing struct)

**`fabro-static`**
- Add `FABRO_MATTERMOST_TOKEN` and `FABRO_MATTERMOST_WEBHOOK_SECRET` to `EnvVars`
- Register both in `OPTIONAL_VAULT_SECRETS` in `secret_registry.rs`

**`fabro-server`**
- Add `MattermostService` struct (private, mirrors `SlackService`)
- Add `mattermost_service: Option<Arc<MattermostService>>` and `mattermost_started: AtomicBool` to `AppState`
- Add `start_optional_mattermost_service()` function
- Add `POST /api/v1/webhooks/mattermost` route
- Call both from `build_router_with_options()`

## Config & Secrets

### Server config (`settings.toml`)

```toml
[server.integrations.mattermost]
enabled = true                          # bool, defaults to true (mirrors SlackIntegrationSettings)
url = "https://mm.example.com"          # Option<InterpString> — Mattermost server base URL
team = "myteam"                         # Option<InterpString> — team name for channel resolution
default_channel = "fabro-alerts"        # Option<InterpString> — mirrors SlackIntegrationSettings.default_channel
```

`MattermostIntegrationSettings` in `fabro-types/src/settings/server.rs`:
```rust
pub struct MattermostIntegrationSettings {
    pub enabled:         bool,
    pub url:             Option<InterpString>,
    pub team:            Option<InterpString>,
    pub default_channel: Option<InterpString>,
}
// Default: enabled = true, all others None
```

### Vault secrets

Set via `fabro secret set`. Registered in `OPTIONAL_VAULT_SECRETS`.

| Secret | Purpose |
|---|---|
| `FABRO_MATTERMOST_TOKEN` | Bot/personal access token. Used for all REST API calls (`Authorization: Bearer`) and WebSocket authentication. Single token — no equivalent of Slack's separate app token. |
| `FABRO_MATTERMOST_WEBHOOK_SECRET` | Random token embedded as `?token=` in every action button's `integration.url`. Handler verifies with constant-time compare before processing any inbound action. |

Credential resolution follows the same pattern as Slack:
```rust
pub enum MattermostCredentialResolution {
    Configured(MattermostCredentials),
    Missing { env_vars: Vec<&'static str> },
}
```
Both secrets must be present for the integration to be `Configured`. Missing either disables it
with a startup log line listing which are absent.

### Per-run settings

Reuses `NotificationProviderSettings` and `InterviewProviderSettings` structs unchanged:

```toml
[run.notifications.on-start]
provider = "mattermost"
events = ["run.started"]
[run.notifications.on-start.mattermost]
channel = "fabro-alerts"

[run.notifications.on-finish]
provider = "mattermost"
events = ["run.completed", "run.failed"]
[run.notifications.on-finish.mattermost]
channel = "fabro-alerts"

[run.interviews]
provider = "mattermost"
[run.interviews.mattermost]
channel = "fabro-reviews"
```

### Startup log lines

```
info!(default_channel_configured = ..., "Mattermost integration enabled")
info!("Mattermost integration disabled by server configuration")
info!(missing_env_vars = %..., "Mattermost integration disabled; missing credentials")
```

## Outbound: REST Client & Message Formatting

### `MattermostClient` (`client.rs`)

```rust
pub struct MattermostClient {
    token:         String,
    base_url:      String,
    http:          fabro_http::HttpClient,
    channel_cache: Mutex<HashMap<String, String>>, // "team/name" -> channel_id
}
```

Methods:
- `post_message(channel_id, message, attachments, root_id?)` → `POST /api/v4/posts`
  - Body: `{ channel_id, message, props: { attachments }, root_id }` (`root_id` omitted when `None`)
  - Returns `PostedMessage { post_id, channel_id }`
- `update_post(post_id, message, attachments)` → `PUT /api/v4/posts/{post_id}`
  - *Diverges from Slack*: Mattermost uses `PUT` to a post-ID URL; Slack uses `POST /chat.update` with channel+ts.
- `resolve_channel(team, name)` → `GET /api/v4/teams/name/{team}/channels/name/{name}`
  - Result cached in `channel_cache` keyed on `"{team}/{name}"`. Cache is populated on first use; no invalidation needed (channel IDs are stable).

`PostedMessage { post_id: String, channel_id: String }` — parallel to Slack's `PostedMessage { channel_id: String, ts: String }`. `post_id` serves the same role as `ts`: the key for looking up the original message to update it.

`MattermostApiError` mirrors `SlackApiError`:
```rust
pub enum MattermostApiError {
    Http(String),
    Api(String),
}
```

### `attachments.rs`

Mattermost uses `props.attachments` (Slack-compatible attachment objects) instead of Block Kit.
Same conceptual outputs, different JSON wire format.

**`question_to_attachments(run_id, qid, question, run_web_url)`**

Builds one attachment:
- `color`: `"#0072C6"` (neutral blue for questions)
- `title`: question text (truncated to Mattermost's 200-char attachment title limit)
- `text`: `context_display` if present (truncated)
- `actions`: array of button objects
  - Yes/No: two buttons with `integration.context: { run_id, qid, kind: "yes"|"no" }`
  - Multiple-choice: one button per option, `kind: "selected"`, `key: <option_key>`
  - Freeform: no buttons — user replies in thread
  - Multi-select: note in `text` asking for comma-separated reply (thread-based; no button UX)
- Each button's `integration.url` is `{server_web_url}/api/v1/webhooks/mattermost?token={FABRO_MATTERMOST_WEBHOOK_SECRET}`

**`run_lifecycle_attachments(kind, details)`**

One attachment per lifecycle event:
- `color`: `"#36a64f"` (started/completed), `"#cc0000"` (failed)
- `title`: event label + run name
- `fields`: workflow label, duration (completed/failed only), PR link if present

**`answered_attachments(question_text, answer_text)`**

Replaces the original question attachment after an answer is received. Plain text showing what was asked and what was answered.

## Inbound: WebSocket (thread replies)

### `connection.rs`

Mirrors `fabro-slack/src/connection.rs` structurally.

1. Connect: `tokio_tungstenite::connect_async("wss://<host>/api/v4/websocket")`
2. Authenticate: send `{ "seq": 1, "action": "authentication_challenge", "data": { "token": "<FABRO_MATTERMOST_TOKEN>" } }`
3. Loop: read messages, dispatch, handle `Message::Ping` with `Pong`, break on `Close`
4. On `posted` event: parse `data.post` (a JSON-encoded string within the event), extract `root_id` and `message`
5. Look up `root_id` in `ThreadRegistry` → `(run_id, qid)` → dispatch `SubmitAnswer`

*Diverges from Slack*: Mattermost encodes the post as a JSON string inside the event's `data`
field (`data.post` is `"{\"id\":\"...\",\"root_id\":\"...\",\"message\":\"...\"}"`), requiring
a second `serde_json::from_str` parse step. Slack's Socket Mode sends structured JSON directly.

Reconnect/backoff loop (`run()` / `run_with_status()`) is structurally identical to Slack's: 1s initial, doubling, 30s max.

`ConnectionStatusUpdate`, `ConnectionStatusSink`, `ProcessOutcome` types mirror Slack exactly.

### `dispatch.rs`

```rust
pub enum DispatchAction {
    Connected,
    SubmitAnswer(Box<MattermostAnswerSubmission>),
    Reconnect,
    Ignored,
}
```

Classifies events: `hello` → `Connected`, `posted` with registered `root_id` → `SubmitAnswer`, `goodbye` → `Reconnect`, everything else → `Ignored`.

### `threads.rs`

`ThreadRegistry` is a straight structural copy of `fabro-slack/src/threads.rs`, keyed on
Mattermost's `post_id` (root post ID) instead of Slack's `ts`. Registered when an interview
question is posted (freeform or `allow_freeform` questions); looked up on `posted` events.

## Inbound: Button Webhook

### Route

`POST /api/v1/webhooks/mattermost` added to `build_router_with_options()`.

### Security

`FABRO_MATTERMOST_WEBHOOK_SECRET` is embedded as `?token=<secret>` in the `integration.url`
of every action button at message-build time. The handler extracts the `token` query parameter
and performs a constant-time byte comparison against the vault secret before touching the body.
Requests with missing or mismatched tokens return `401`.

*Diverges from Slack*: Slack uses Socket Mode (the server never receives inbound HTTP for
interactions). Mattermost has no Socket Mode; interactive button actions require an inbound
HTTP endpoint. The token-in-URL pattern is the standard Mattermost integration security model.

### `webhook.rs`

Mattermost POSTs `application/json` to `integration.url` when a button is clicked:

```json
{
  "channel_id": "...",
  "team_id": "...",
  "user_id": "...",
  "user_name": "...",
  "context": {
    "run_id": "...",
    "qid": "...",
    "kind": "yes|no|selected|submit_multi",
    "key": "..."
  }
}
```

`parse_action(payload)` → `Option<MattermostAnswerSubmission>`:
- Extracts `context.run_id`, `context.qid`, `context.kind`, `context.key`
- Builds `Answer` (yes/no/selected/multi-selected) from `kind` + `key`
- Builds `actor: Principal::Mattermost { team_id, user_id, user_name }`

`MattermostAnswerSubmission { run_id, qid, answer, actor }` — parallel to `SlackAnswerSubmission`.

## Server Wiring (`fabro-server`)

### `AppState` additions

```rust
mattermost_service: Option<Arc<MattermostService>>,
mattermost_started: AtomicBool,
```

Added immediately after `slack_service` / `slack_started`.

### `MattermostService`

```rust
struct MattermostService {
    client:          MattermostClient,
    team:            String,
    default_channel: Option<String>,
    posted_messages: Arc<Mutex<HashMap<(RunId, String), PostedMessage>>>,
    thread_registry: Arc<ThreadRegistry>,
    connection:      Arc<Mutex<MattermostConnectionRuntimeState>>,
}
```

Methods (all parallel to `SlackService`):
- `handle_event(state, envelope, run_web_url)` — dispatches on `EventBody` variant
- `handle_lifecycle_event(state, envelope, run_web_url)` — filters `route.provider == "mattermost"`, reads `route.mattermost.channel`, resolves channel ID, posts lifecycle attachment
- `finish_interview(run_id, qid, question, answer_text)` — updates original post with `answered_attachments`
- `submit_answer(state, submission)` — routes to `submit_pending_interview_answer`
- `connection_status()` → `IntegrationConnectionStatus`
- `status_sink()` → `ConnectionStatusSink`

Interview routing uses `default_channel` (from server config) for questions, matching the
current Slack behavior. The `[run.interviews.mattermost].channel` config field is added to the
schema for forward-compatibility, but is not read by the server — identical to how Slack's
`[run.interviews.slack].channel` field exists in the types but is currently unused by the
server's interview dispatch logic.

### `start_optional_mattermost_service()`

Spawns two Tokio tasks, parallel to `start_optional_slack_service()`:
1. Event broadcast listener → `service.handle_event()`
2. WebSocket loop → `mattermost_connection::run_with_status()`

### `build_router_with_options()`

```rust
start_optional_slack_service(&state);
start_optional_mattermost_service(&state);  // added
```

Webhook route added to the API router:
```rust
.route("/api/v1/webhooks/mattermost", post(handler::mattermost_webhook))
```

### AppState construction

Follows the same pattern as Slack credential resolution:
```rust
let mattermost_service = {
    let mm_settings = &current_server_settings.server.integrations.mattermost;
    if mm_settings.enabled {
        // resolve url, team, default_channel from settings
        // resolve credentials from vault
        // log enabled / disabled-missing-credentials
    } else {
        info!("Mattermost integration disabled by server configuration");
        None
    }
};
```

## `Principal::Mattermost`

Added to `fabro-types/src/principal.rs`:

```rust
Mattermost {
    team_id:   String,
    user_id:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_name: Option<String>,
},
```

Placed immediately after `Principal::Slack` in the enum. Derives the same traits. Added to
the round-trip test in that file.

## Tests

### `fabro-mattermost` unit tests

- `config.rs`: credential resolution with all combinations of present/absent/empty secrets
- `client.rs`: `parse_post_message_response`, `resolve_channel` cache behavior
- `attachments.rs`: `question_to_attachments` for each `QuestionType`, `run_lifecycle_attachments` for each `RunLifecycleKind`, `answered_attachments`; truncation at Mattermost limits
- `connection.rs`: `process_message` for `hello`, `posted` (registered + unregistered thread), `goodbye`, malformed JSON
- `dispatch.rs`: table-driven over all `DispatchAction` variants
- `webhook.rs`: `parse_action` for yes/no/selected; missing fields return `None`

### `fabro-server` unit tests

- Webhook handler: correct token → 200 + answer dispatched; wrong token → 401; missing token → 401
- Lifecycle routing: `route.provider == "mattermost"` routes to Mattermost service; `"slack"` routes to Slack service; each is independent

### Manual integration test plan

Against `docker run --name mattermost-preview -p 8065:8065 mattermost/mattermost-preview`:
1. Create a bot account, generate a personal access token → set `FABRO_MATTERMOST_TOKEN`
2. Generate a random webhook secret → set `FABRO_MATTERMOST_WEBHOOK_SECRET`
3. Configure `[server.integrations.mattermost]` pointing at `http://localhost:8065`, team `ad-1`, channel `town-square`
4. Start Fabro server; confirm "Mattermost integration enabled" in logs and WebSocket connects
5. Trigger a run; confirm lifecycle notification appears in `town-square`
6. Trigger a run with a yes/no interview; confirm question with buttons appears; click Yes; confirm answer is recorded and post updates
7. Trigger a run with a freeform interview; confirm question appears; reply in thread; confirm answer is recorded
8. Confirm Slack integration is unaffected throughout

## Docs

- New page: `docs/public/integrations/mattermost.mdx` (mirrors the Slack integration page)
- Updated: `docs/public/administration/server-configuration.mdx` — add `[server.integrations.mattermost]` section and both new secrets to the secrets table
- Updated: `docs/public/llms.txt` — add Mattermost integration page entry
- Updated: `.env.example` — add `FABRO_MATTERMOST_TOKEN` and `FABRO_MATTERMOST_WEBHOOK_SECRET` (commented out)

## Out of Scope

- Generic "chat provider" trait abstraction. No trait is introduced; both Slack and Mattermost remain concrete sibling implementations.
- Mattermost OAuth app flow. Personal access token / bot token only.
- Multi-team routing. One `team` per Fabro server; all channels must be in that team.
- Mattermost slash commands or outgoing webhooks as an alternative inbound path.
