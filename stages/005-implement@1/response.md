Everything is green. Let me summarize what was done:

## Summary

Implemented the compound-engineering PR title and body recipe per plan:

### Code changes (`lib/crates/fabro-workflow/src/pipeline/pull_request.rs`)

1. **Schema + typed struct** — Added `PR_CONTENT_SCHEMA` (`LazyLock<serde_json::Value>`) with required `title` (max 72) and `body` (min 1), and `GeneratedPrContent { title, body }` deserialized from structured output. Strict (no `additionalProperties`).

2. **System prompt** — Added `PR_BODY_SYSTEM_PROMPT` constant containing the verbatim recipe from the plan: output format, "do not include" guardrails, sizing matrix, writing principles, plan-summary rule, visual-aid table.

3. **Truncation tiers** — Added six constants and `truncation_caps(model)` that selects the large tier (75k/75k/250k) when `Catalog::builtin().get(model).limits.context_window >= 200_000`, else the conservative tier (20k/20k/50k matching the previous values). Added `truncate_chars` helper for char-boundary-safe slicing.

4. **`enforce_title_cap(&str) -> String`** — 72-char Unicode-scalar cap, replacing the trailing char with `…`.

5. **Refactored builders** — `build_pr_body`, `build_pr_body_with_client`, `build_pr_body_with_source_and_state`, `build_pr_body_with_client_and_state` now return `Result<(String, String), String>`. The inner builder switches `generate` → `generate_object`, deserializes to `GeneratedPrContent`, applies the failure-mode table from the plan: missing/invalid output → `Err`, blank/whitespace body → `Err`, empty title → passes through unchanged (caller handles fallback), oversized title → capped via `enforce_title_cap`. The body is unchanged from the LLM and assembled normally with the trailing programmatic sections.

6. **`maybe_open_pull_request`** — Destructures `(llm_title, body)`. If `llm_title.trim().is_empty()`, falls back to `pr_title_from_goal(req.goal)`; otherwise uses the LLM title. Then `enforce_title_cap` runs unconditionally (covers both branches), so the fallback path can never breach the 72-char contract that the LLM path satisfies via the schema.

### Tests added (red→green)

- `build_pr_body_truncates_long_title` — MockProvider returns 200-char title; assert `chars().count() == 72` and ends with `…`.
- `build_pr_body_returns_err_when_body_empty` — schema rejects `""` (minLength 1).
- `build_pr_body_returns_err_when_body_whitespace` — schema accepts `"   \n"`, but the Rust trim-check rejects it; asserts the specific error message.
- `setup_fallback_test_harness` — shared helper standing up an `httpmock::MockServer` for OpenAI (`POST /v1/responses`, bearer `vault-openai-key`), a separate mock for GitHub (`POST /repos/owner/repo/pulls`, bearer `test-token`), a `Vault`-backed `VaultCredentialSource` with `OPENAI_BASE_URL` env override, `GitHubCredentials::Token("test-token")`, and a `RunStoreHandle` seeded with `WorkflowRunCompleted { final_patch: Some(...) }` so `load_pull_request_diff` returns content. The `_openai_server` field is held to keep the mock listener alive.
- `maybe_open_pull_request_falls_back_to_goal_title_when_llm_returns_empty_title` — OpenAI mock returns `{"title":"","body":"Narrative."}`, goal `"Fix telemetry leak\n\ndetails..."`; asserts `record.title == "Fix telemetry leak"`.
- `maybe_open_pull_request_caps_fallback_title_at_72_chars` — same harness, 200-char goal forces `pr_title_from_goal` to return ~120 chars; asserts the final title is exactly 72 chars and ends with `…`, proving the load-bearing unconditional `enforce_title_cap` after fallback selection.

### Existing tests updated

- All 5 existing `build_pr_body_*` tests in `pull_request.rs` now destructure `(title, body)` and route their MockProvider/openai responses through the new `pr_content_json(title, body)` helper. `build_pr_body_uses_in_memory_conclusion` and `build_pr_body_uses_vault_only_openai_codex_source` additionally assert on title equality.
- `lib/crates/fabro-workflow/tests/it/integration.rs:6893` (`workflow_run_with_vault_only_openai_codex_builds_pr_body`) — updated to destructure the tuple, return structured-output JSON from the OpenAI mock, and assert `title == "Vault title"`.
- `lib/crates/fabro-server/src/server/tests.rs:3668` (`create_run_pull_request_creates_and_persists_record`) — updated the OpenAI responses mock to return structured-output JSON instead of plain text.

### Verification

- `cargo build --workspace` ✅
- `cargo nextest run -p fabro-workflow` ✅ (1057/1057 pass, 34 skipped)
- `cargo nextest run -p fabro-server` ✅ (441/443 pass; the two `*_returns_svg` failures are pre-existing and unrelated — confirmed by reproducing them on the parent commit `dc09842a`)
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅
- `cargo +nightly-2026-04-14 fmt --check --all` ✅

### Out of scope items (per plan) — not changed

- No `pull_request.prompt_preset`/`prompt_override` config.
- No separate `pull_request.model` override.
- No dynamic budgeting based on tokenizer math (two-tier static lookup as planned).
- No stage-response inclusion.
- Goal truncation is silent — full goal is *not* attached as a `<details>` block (option A).
- `assemble_pr_body`, `format_retro_section`, `format_arc_details_section`, `read_plan_text`, `truncate_pr_body`, `load_pull_request_diff`, `pr_title_from_goal` (with its 120-char cap), `OpenPullRequestRequest`, `PullRequestRecord`, `AutoMergeOptions`, the 65,536-char body cap, and the `_(truncated)_` suffix are unchanged.