# CLI Model Server Canonicalization And LLM Namespace Removal

## Summary
Take the next low-risk simplification step after the run lifecycle cleanup by:

- removing the entire unused `fabro llm` CLI namespace
- making the `fabro model` family fully server-canonical
- leaving `fabro exec` unchanged for now

This pass is intentionally asymmetric:
- `fabro llm` is removed from the CLI surface entirely
- `fabro model` remains, but becomes a server-backed command family instead of a mixed standalone/server command
- `fabro model test` keeps its current bulk/fan-out role in the CLI, but the server endpoint remains single-model only with an explicit test mode

Because backward compatibility is not required yet, this pass should prefer simplification over shims. `model test --deep` stays, but it becomes an explicit server capability via `mode=basic|deep` on the single-model test endpoint instead of relying on the old mixed local/server split.

## Scope Boundaries
In scope:
- remove `fabro llm`
- add `provider` and `query` filters to `GET /api/v1/models`
- keep `POST /api/v1/models/{id}/test` as a single-model health/test endpoint
- add optional `mode=basic|deep` to `POST /api/v1/models/{id}/test`
- make `fabro model list` and `fabro model test` always use the server

Out of scope:
- `fabro exec`
- broader removal of `ExecutionMode` / `resolve_mode` outside the `model` command family
- changing `POST /models/{id}/test` into a bulk endpoint
- changing model storage/catalog ownership away from the server’s built-in catalog
- deleting lower-level `fabro_llm::cli` prompt/chat helpers unless they become dead and trivially removable during the refactor

## Key Decisions
- `GET /api/v1/models` gets flat query params, not a generic filter object:
  - `provider=<name>`
  - `query=<substring>`
- `query` matches `id`, `display_name`, and `aliases`, case-insensitively.
- Pagination applies after filtering.
- filtered `/api/v1/models` results preserve the built-in catalog order.
- invalid `provider` filter values are rejected with `400`, not treated as “no filter” or “no matches”.
- "invalid" means the query value fails to parse as a known `fabro_model::Provider` enum variant.
- a parsed `provider` value that happens to match zero catalog entries still returns `200` with an empty page.
- `POST /api/v1/models/{id}/test` continues to test exactly one model per request and gains one optional query param:
  - `mode=basic|deep`
  - default: `basic`
- CLI fan-out stays in `fabro model test`, not in the HTTP API.
- `model test --deep` is preserved and maps to `mode=deep` on repeated single-model server calls.
- `fabro model` should use the generated `fabro_api::Client` for model HTTP calls, not ad hoc raw `reqwest` + URL assembly.
- `fabro model` gets a command-specific server-target helper rather than reusing global `ExecutionMode` branching.
  - if `--server-url` is present, use remote HTTP(S) with configured TLS
  - otherwise use local server auto-start + Unix socket for the selected storage dir
  - this does not change `resolve_mode` behavior for other commands
- `basic` is the explicit name for the current simple health check mode.
  - Rationale: an enum-shaped mode is clearer than `deep=true` and leaves room for future test kinds without changing the endpoint shape.
- `POST /api/v1/models/{id}/test` accepts either a canonical model ID or an alias.
  - The server resolves aliases using the built-in catalog.
  - The response should always return the canonical `model_id`, not the alias string from the path.
- `model test --model <id-or-alias>` should POST the provided value directly to `/api/v1/models/{id-or-alias}/test`.
  - the CLI does not pre-resolve aliases via `GET /models`
- CLI `model list --json` preserves its current output contract as a plain array of model objects.
  - The server remains paginated internally, but the CLI should flatten that to preserve the existing CLI JSON surface.
- Deep-mode API results remain binary at the schema level:
  - success cases return `status: ok`
  - any deep validation failure returns `status: error`
  - the failure details go in `error_message`
  - no new warning/partial result state is introduced in this pass
  - models without required tool support still return HTTP `200` with `status: error`
  - absence of reasoning traces alone does not fail deep mode in this pass

## Implementation Changes
### 1. Remove the `fabro llm` CLI namespace
Update the CLI surface so `fabro llm` no longer exists.

In `lib/crates/fabro-cli/src/args.rs`:
- remove the `Commands::Llm` variant
- remove `LlmNamespace` and `LlmCommand`
- remove `ChatArgs` / `PromptArgs` imports from `fabro_llm::cli`
- remove command-name mapping for `llm prompt` and `llm chat`

In `lib/crates/fabro-cli/src/main.rs`:
- remove the `Commands::Llm(...)` dispatch arm

In `lib/crates/fabro-cli/src/commands/mod.rs`:
- remove `pub(crate) mod llm;`

Delete:
- `lib/crates/fabro-cli/src/commands/llm/mod.rs`
- `lib/crates/fabro-cli/src/commands/llm/chat.rs`
- `lib/crates/fabro-cli/src/commands/llm/prompt.rs`

Test/support cleanup:
- remove `mod llm;` and `mod llm_prompt;` from `lib/crates/fabro-cli/tests/it/cmd/mod.rs`
- delete:
  - `lib/crates/fabro-cli/tests/it/cmd/llm.rs`
  - `lib/crates/fabro-cli/tests/it/cmd/llm_prompt.rs`
- remove the now-unused `TestContext::llm()` helper from `lib/crates/fabro-test/src/lib.rs`
- update top-level help snapshots in `lib/crates/fabro-cli/tests/it/cmd/fabro.rs` and any parser/help coverage that still mentions `llm`

This pass should only remove the CLI namespace. Do not widen the blast radius by opportunistically deleting unrelated lower-level LLM helpers unless the compiler proves they are now dead and the deletion is mechanical.

Because `fabro llm` is the only CLI surface for these paths today, expect some prompt/chat internals to become dead as a direct consequence of this removal. If the compiler confirms there are no remaining callers, delete the dead items in this pass rather than preserving unreachable code:
- `PromptArgs`
- `ChatArgs`
- `run_prompt`
- `run_chat`
- `run_prompt_via_server`
- `run_chat_via_server`

### 2. Make `/api/v1/models` a real server-owned list endpoint
Treat `/api/v1/models` as a canonical non-demo API surface.

In `docs/api-reference/fabro-api.yaml`:
- add optional query parameters to `GET /api/v1/models`:
  - `provider`
  - `query`
- document `query` matching semantics explicitly:
  - substring match
  - case-insensitive
  - fields: `id`, `display_name`, `aliases`
- add optional query parameter to `POST /api/v1/models/{id}/test`:
  - `mode=basic|deep`
  - default behavior when omitted: `basic`
  - document `basic` as the current simple prompt/availability check
  - document `deep` as the multi-turn tool-use / reasoning round-trip check

In `lib/crates/fabro-server/src/server.rs`:
- keep `/models/{id}/test` wired to the real server handler
- stop treating `/models` as demo semantics only
- replace the `demo::list_models` route usage with a non-demo handler
- implement a small query extractor for model filters and pagination in the real server path
- implement a small query extractor for model test mode in the `test_model` handler
- fix the `test_model` response to use `info.id` (the canonical model ID from the catalog lookup) instead of the raw `id` path parameter in all response branches, so alias lookups return the canonical ID

In `lib/crates/fabro-server/src/demo/mod.rs`:
- `demo::list_models` is currently wired in both `demo_routes()` and `real_routes()`; replace both usages with the new non-demo handler
- remove the now-unused `list_models` helper from the demo module

Behavior:
- server builds the list from `fabro_model::Catalog::builtin()`
- applies `provider` filter if present
- applies `query` filter if present
- paginates the filtered list
- returns the same `PaginatedModelList` schema shape as today
- preserves built-in catalog order after filtering
- rejects unknown `provider` values with `400`
  - this means parse failure against `fabro_model::Provider`
  - a valid parsed provider with zero matching models still returns `200` and an empty page
- `POST /models/{id}/test`:
  - defaults to `basic`
  - accepts either a canonical model ID or an alias
  - runs the current simple one-prompt check in `basic` mode
  - runs the deeper multi-turn tool-use / reasoning check in `deep` mode
  - still returns one result object for one model

### 3. Make `fabro model` server-canonical
Remove standalone/server branching from the CLI `model` command family.

In `lib/crates/fabro-cli/src/commands/model.rs`:
- stop using `resolve_mode`
- stop passing `Option<ServerConnection>` into `run_models(...)`
- construct a typed `fabro_api::Client` up front and pass it through unconditionally

In `lib/crates/fabro-cli/src/server_client.rs`:
- keep the existing local auto-start + Unix-socket path for storage-backed connections
- add one small model-command helper that returns a typed `fabro_api::Client` from either:
  - explicit remote base URL + configured TLS (`--server-url`), or
  - local auto-start + Unix socket for the selected storage dir
- if needed, split the current Unix-only local connect helper into:
  - a local auto-start helper, and
  - a thin typed-client constructor for remote HTTP(S) base URLs
- do not route `model` through `ServerStoreClient`; `model` should use the generated API client directly

This change is intentionally command-specific:
- `--server-url` keeps working for remote server usage
- `--storage-dir` or default local storage uses local server auto-start
- `model` no longer branches on `ExecutionMode`
- global `resolve_mode` behavior for other commands stays unchanged in this pass

### 4. Simplify model selection and test orchestration
Keep the CLI in charge of bulk orchestration, but move catalog authority to the server.

In `lib/crates/fabro-llm/src/cli.rs`:
- replace the raw `reqwest` + `base_url` model HTTP helpers with generated `fabro_api::Client` calls
- update `fetch_models_from_server(...)` to forward `provider` and `query` to the server instead of filtering locally after the response
- update the fetch helper to follow pagination until `meta.has_more` is false instead of assuming one page is enough
- remove local provider filtering from the fetch helper
- remove the local query filtering in `run_models(...)` (the `if let Some(q) = &query { ... models.retain(...) }` block), since the server now handles query filtering
- simplify `run_models(...)` so it no longer accepts an optional server connection; `model` should always call the server-backed path
- keep `model test` fan-out behavior in the CLI:
  - `model test --model <id-or-alias>` calls `POST /models/{id-or-alias}/test` once
  - `model test --provider <provider>` first resolves the filtered list via `GET /models?provider=...`, then POSTs `/test` once per returned model
  - bare `model test` first resolves the full list via `GET /models`, then POSTs `/test` once per model
  - `model test --deep` maps each request to `POST /models/{id}/test?mode=deep`
  - default `model test` behavior maps each request to `basic` mode
  - any shared fetch helper used from test fan-out paths passes `query=None`, because query filtering remains list-only in this pass

Preserve current broad behavior where reasonable:
- `--model` remains the direct single-model selector and should pass the user-supplied ID or alias through unchanged
- unknown model inputs should continue to surface an “unknown model” style error when the server returns 404
- CLI fan-out result formatting and failure aggregation should remain substantially the same
- `model list --json` should continue to print a plain JSON array, not the server’s paginated envelope
- CLI output order should preserve the server/catalog order
- CLI selection behavior remains:
  - `--model` takes precedence over `--provider`
  - query filtering applies only to `list`, not `test`, unless explicitly added later

### 5. Preserve `model test --deep` via server-owned test modes
`--deep` remains useful, but it should stop depending on the old local test execution branch. The server should own both single-model test modes, and the CLI should only orchestrate fan-out.

Server-side implementation:
- extract the single-model test logic out of CLI-only code and into a reusable non-CLI helper in `fabro-llm` so the server can execute:
  - `basic` mode
  - `deep` mode
- keep that logic in `fabro-llm`, not inline in the axum handler; this is LLM-domain behavior, not route-local glue
- shape the extracted helper around an internal outcome type that already matches the binary API contract:
  - success => `status: ok`
  - failure => `status: error` plus message detail
- preserve the current timeout budgets:
  - `basic` uses the current short timeout budget (`30s`)
  - `deep` uses the current long timeout budget (`90s`)
- keep deep mode's current in-process tool-closure pattern from `build_deep_test_params`; this pass does not add RPC or external tool-runner infrastructure
- `mode=deep` for a model without tool support returns HTTP `200` with `status: error` and a clear `error_message`
- if a reasoning-capable model completes deep mode without reasoning traces, do not fail for that fact alone in this pass
- the initial source material is the existing logic in `lib/crates/fabro-llm/src/cli.rs`:
  - `build_deep_test_params`
  - `validate_deep_result`
  - current one-model test logic
- the end state should be:
  - server calls a reusable `fabro-llm` helper for one-model test execution
  - CLI no longer owns authoritative one-model test behavior
- do not leave deep behavior only in the CLI-local path once `model` becomes server-canonical

CLI-side implementation:
- keep `deep` on `ModelsCommand::Test`
- update the server-call helper(s) to pass `mode=deep` when requested
- remove only the dead local standalone branches once the server owns both modes
- remove the `"Warning: --deep is not supported in server mode"` diagnostic from `test_models_via_server`, since deep mode is now a server-owned capability
- remove the `deep_unsupported` field from `ModelTestOutput` JSON serialization, since deep mode is now fully supported via the server

In CLI help/snapshots:
- keep `--deep` on `model test --help`
- update wording if needed so it reflects a server-backed deep test rather than a local-only path

### 6. Dependencies And Sequencing
Apply this in order so the refactor has stable interfaces to land on:

- land the OpenAPI changes for `/models` filters and `/models/{id}/test?mode=basic|deep` first
- update the server list/test handlers, including provider parsing, alias handling, and canonical response IDs
- extract the reusable single-model `basic` / `deep` test helper in `fabro-llm`
- regenerate the Rust typed API client via `cargo build -p fabro-api`
- repoint `fabro model` and its fetch/test helpers to the generated `fabro_api::Client`
- remove the `fabro llm` CLI namespace and any compiler-confirmed dead prompt/chat code
- regenerate the TypeScript client once the server contract is settled

Section 2's endpoint and mode changes must land before replacing the deep-mode server flow described in Section 5.

### 7. Regenerate typed API clients through the normal workflow
Because `/api/v1/models` changes, follow the repo’s API workflow instead of hand-editing generated clients.

Source of truth:
- `docs/api-reference/fabro-api.yaml`

Generated/regenerated artifacts:
- Rust API client/types via `cargo build -p fabro-api`
- TypeScript client via `cd lib/packages/fabro-api-client && bun run generate`

Expected generated updates include:
- `lib/packages/fabro-api-client/src/api/models-api.ts`
- related generated model/type files under `lib/packages/fabro-api-client/src/models/`

## Important Interface / Behavior Changes
- `fabro llm` is removed from the CLI surface completely.
- `fabro model` always talks to the server.
- `GET /api/v1/models` accepts:
  - `provider`
  - `query`
- invalid `provider` values return `400`
- invalid `provider` means the value does not parse as a known `fabro_model::Provider`
- valid provider filters that simply match zero models still return `200` with an empty page
- filtered results preserve built-in catalog order
- `POST /api/v1/models/{id}/test` accepts:
  - optional `mode=basic|deep`
  - default mode `basic`
- `/models/{id}/test` accepts aliases but returns the canonical `model_id`
- `model test --model <alias>` posts that alias directly and relies on server-side alias resolution
- `model test --deep` remains supported and maps to `mode=deep`.
- `model list --json` continues to output a plain JSON array.

## Test Plan
CLI command surface:
- `lib/crates/fabro-cli/tests/it/cmd/fabro.rs`
  - top-level help no longer lists `llm`
- `lib/crates/fabro-cli/tests/it/cmd/model.rs`
  - bare `model` and `model list` still work
  - provider/query list behavior still matches expected snapshots
  - JSON output still parses as a plain array
  - `model list` still works with an auto-started local server and with `--server-url`
- `lib/crates/fabro-cli/tests/it/cmd/model_list.rs`
  - help text still matches the new server-backed behavior
- `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`
  - help text still mentions `--deep`
  - unknown model still errors cleanly
  - `--model <alias>` calls the single-model test endpoint directly and succeeds via server-side alias resolution
  - deep mode still routes through the server-backed model test flow
  - JSON output no longer includes `deep_unsupported`
- delete now-obsolete `llm` CLI tests:
  - `lib/crates/fabro-cli/tests/it/cmd/llm.rs`
  - `lib/crates/fabro-cli/tests/it/cmd/llm_prompt.rs`

Server/API behavior:
- `lib/crates/fabro-server/src/server.rs`
  - add tests for `GET /api/v1/models` with:
    - no filters
    - `provider`
    - `query`
    - combined `provider + query`
    - case-insensitive query matching
    - pagination applied after filtering
    - invalid provider returns `400`
    - valid parsed provider + no matching query returns `200` with an empty page
    - filtered results preserve catalog order
  - extend `POST /api/v1/models/{id}/test` coverage for:
    - omitted mode defaults to `basic`
    - explicit `mode=basic`
    - explicit `mode=deep`
    - invalid mode rejected cleanly
    - alias path values resolve successfully and return the canonical `model_id`
    - unknown model still returns 404
    - `mode=deep` on a model without tool support returns `200` with `status: error`

LLM CLI internals:
- `lib/crates/fabro-llm/src/cli.rs`
  - update `fetch_models_from_server` unit tests to assert outgoing `provider` / `query` params
  - add coverage that multi-page server responses are fully traversed
  - update/remove tests that assumed client-side provider filtering
  - add coverage that test fan-out paths call the shared fetch helper with `query=None`
  - add or update server-call tests so deep mode is forwarded as `mode=deep`
  - add helper-level coverage for the binary deep-mode contract:
    - tool-less models return `error`
    - missing reasoning traces alone do not fail
  - remove only the dead standalone-only test coverage after server ownership is in place

Full verification:
- `cargo build -p fabro-api`
- `cargo nextest run -p fabro-cli --no-fail-fast`
- `cargo nextest run -p fabro-server`
- `cargo fmt --check --all`
- `cargo clippy --workspace -- -D warnings`
- `cd lib/packages/fabro-api-client && bun run generate`

## Assumptions And Defaults
- `fabro llm` is dead product surface and should be removed cleanly, not hidden.
- `exec` remains a separate product surface and is intentionally untouched in this pass.
- `GET /api/v1/models` remains a built-in catalog view; this pass does not introduce live provider discovery.
- CLI model fetches must traverse paginated `/models` responses until exhaustion.
- `model test` remains a CLI orchestrator over repeated single-model HTTP calls.
- local-storage-backed `fabro model` may auto-start the local server on first use.
- for local auto-started servers, model testing assumes the daemon inherits the CLI environment, including provider credentials.
- for explicit remote `--server-url` usage, provider credential availability is the remote server operator's responsibility.
- `POST /api/v1/models/{id}/test` uses a single optional query param for mode selection:
  - `basic`
  - `deep`
- Deep-mode validation failures are represented as `status: error` plus `error_message`, not a new intermediate status.
- If both `--model` and `--provider` are supplied to `model test`, keep the current effective behavior of prioritizing the explicit model selection rather than inventing a new validation rule in this pass.
- No backward-compatibility shims are needed for removed CLI commands or flags.
