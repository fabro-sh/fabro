# Simplify `RunEvent` While Keeping Wire JSON Stable

## Summary
- Refactor the event model so `RunEvent` stores only envelope metadata plus a typed `EventBody`; the JSON wire format stays `{ ..., "event": "...", "properties": { ... } }`.
- Treat this as an internal Rust API break now: remove public `RunEvent.event` and `RunEvent.properties`, update repo call sites in one pass, and align the code with `docs-internal/events-strategy.md`'s "canonical envelope built once" rule.
- Preserve forward-compatibility for unknown stored events explicitly instead of relying on duplicate cached fields.
- Phase the refactor into three commits so the direct `Event -> RunEvent` mapping can land and be verified before the cached-field removal.

## Implementation Changes
- Sequencing
  - Commit 1: add `EventBody::event_name() -> &str`, remove `event_name_from_body()` and `properties_from_body()`, and replace both with explicit implementations that keep cached fields working through commits 1 and 2.
  - Commit 2: rework `fabro-workflow` to construct `RunEvent` directly from `Event`; this is the main structural refactor and the primary regression risk.
  - Commit 3: remove cached `RunEvent.event` / `RunEvent.properties`, update callers and tests, and replace `EventBody::Unknown` with a raw-preserving variant.
- `lib/crates/fabro-types/src/run_event/mod.rs`
  - Redefine `RunEvent` to contain `id`, `ts`, `run_id`, optional envelope metadata, and `body: EventBody` only.
  - Keep `RunEvent::from_value`, `from_json_str`, and `to_value`, but make them thin wire-boundary helpers around a private raw wire struct for `{ event, properties }`.
  - Replace `EventBody::Unknown` with a raw-preserving variant such as `Unknown { name: String, properties: Value }`.
  - Add `EventBody::event_name() -> &str` implemented as an exhaustive `match` returning the serde rename string for each known variant and `name.as_str()` for `Unknown`.
  - In commit 1, replace `properties_from_body()` with an explicit property-serialization helper that derives the inner properties payload without the current serialize-and-pluck helper pattern; it may still serialize as an interim step, but it should exist only to support cached fields and wire serialization during the transition.
  - Keep JSON property extraction as a serialization helper, not a hot-path public API. Use it only in `RunEvent::to_value` / `Serialize` and in wire-shape tests that need JSON-level assertions.
  - Remove `refresh_cache`, `event_name_from_body`, and `properties_from_body`.
  - Call out unknown-event fallback explicitly: `Unknown { name, properties }` cannot rely on `#[serde(other)]`, so `RunEvent::from_value` must use a custom fallback path that preserves raw `event` and `properties` when typed `EventBody` deserialization fails.
- `lib/crates/fabro-workflow/src/event.rs`
  - Split the current conversion into two explicit pieces: envelope metadata extraction and `Event -> EventBody` construction.
  - Rework `to_run_event_at()` to build `RunEvent` directly, not via `json!` plus `RunEvent::from_value`.
  - Keep all existing canonicalization rules, but express them as Rust matches: `run_id` stripping, node/session extraction, node-label defaults, failure/error normalization, and agent/sandbox nested event flattening.
  - Treat `Event::Agent` and `Event::Sandbox` as the bulk of the work:
    - `Event::Agent` must expand each `AgentEvent` sub-variant into the corresponding `EventBody` variant while also lifting `stage -> node_id`, preserving `session_id` / `parent_session_id` in the envelope, and merging `visit` into the inner props where required.
    - `Event::Sandbox` must unwrap each `SandboxEvent` sub-variant into the corresponding `EventBody` variant while preserving the current flattened wire shape.
    - `Event::WorkflowRunFailed` must continue converting `FabroError` into the stored string form used by `RunFailedProps`.
    - stage/parallel/prompt/watchdog variants must continue moving `node_id`/`stage`/`branch`/`node` into the envelope with the same current `node_label` defaults.
  - Make the lossy cross-crate conversions explicit in the implementation and guard them with wire-shape characterization tests:
    - `fabro_agent::AgentError -> String`
    - `fabro_llm::error::SdkError -> string fields in retry props`
    - `fabro_llm` usage types -> `fabro_types` usage structs
    - `fabro_workflow::error::FabroError -> String`
  - Delete `tagged_variant_fields*` once all variant mapping is direct and covered by tests.
  - Keep redaction/persistence logic driven by serialized `RunEvent` wire value; no wire-shape change and no redaction contract change.
  - Explicitly keep the `build_redacted_event_payload` pipeline out of scope for this pass: no changes to `to_value() -> normalize -> to_string -> redact -> from_str`.
- `lib/crates/fabro-store/src/types.rs`, `lib/crates/fabro-store/src/run_state.rs`, and repo consumers
  - Update call sites to stop reading `RunEvent.event` and `RunEvent.properties` directly.
  - Default rule: production consumers match on `body`; only serialization/wire tests should rely on JSON property extraction.
  - Route store decoding through one helper path (`TryFrom<&EventPayload>` or `RunEvent::from_value`) and keep clone-based payload parsing for now; zero-copy parsing is out of scope for this pass.
  - Update strategy/docs terminology only, not code naming: leave `RunEvent` as the code type in this pass and align `docs-internal/events-strategy.md` if needed.

## Test Plan
- `lib/crates/fabro-types/src/run_event/mod.rs`
  - known event round-trip preserves the wire JSON shape
  - unknown event round-trip preserves raw `event` and `properties`
  - known event name with invalid properties still fails deserialization
  - absent optional envelope fields serialize as omitted fields, not `null`
- `lib/crates/fabro-workflow/src/event.rs`
  - characterization tests for representative variants: stage event, agent event, sandbox event, and run failure
  - assert direct construction produces the same wire JSON and envelope fields as today
  - assert `build_redacted_event_payload` still returns a valid `EventPayload`
  - add focused coverage for agent flattening and sandbox flattening, since those wrappers are the highest-risk conversion paths
- `lib/crates/fabro-store/src/run_state.rs` or adjacent store tests
  - replay persisted payloads into `RunProjection` still reconstructs run, status, checkpoint, retro, and pull-request state correctly
- Test migration rules
  - behavior tests should prefer matching on typed `body` instead of reintroducing JSON-shaped assertions
  - wire-contract tests should assert on `to_value()` / serialized JSON when the exact `properties` shape matters
  - do not replace all former `stored.properties["foo"]` assertions with a general-purpose allocating helper in production code
  - `fabro-cli/src/commands/run/run_progress/event.rs::from_run_event()` is already aligned with the target design because it matches on `EventBody`; only any remaining CLI tests asserting through `stored.properties` need migration in commit 3
- Verification
  - run `cargo nextest run -p fabro-types`
  - run `cargo nextest run -p fabro-workflow`
  - run `cargo nextest run -p fabro-store`
  - run `cargo fmt --check --all` and `cargo clippy --workspace -- -D warnings`

## Assumptions
- The JSON wire protocol stays compatible; only the internal Rust representation and helper APIs change.
- Internal Rust API break is acceptable now; tests and internal consumers will be updated in the same pass.
- Unknown stored events are a supported forward-compatibility case and must survive parse/serialize unchanged.
- This pass optimizes for simplicity and maintainability first; deeper read-side performance work such as borrowed parsing, eliminating `Value` clones in projection, or optimizing the redaction pipeline can follow separately.
