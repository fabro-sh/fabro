Implemented cleanup fixes after review.

Notes:
- I launched the three review agents twice in parallel, but all six subagents failed with an internal LLM item-persistence error before returning findings. I proceeded with the review manually.

Fixed:
- Removed the extra public `PrContent` struct and restored the requested `Result<(String, String), String>` return shape for `build_pr_body*`.
- Updated callers/tests to destructure `(title, body)` instead of using a new public wrapper type.
- Made `maybe_open_pull_request` apply `enforce_title_cap` unconditionally after selecting LLM title vs. fallback title.
- Changed prompt input truncation helper to actually cap by Unicode scalar count instead of byte count while preserving UTF-8 safety.
- Strengthened fallback PR tests to assert both OpenAI and GitHub mocks were called exactly once.
- Clarified the empty-body test comment so it doesn’t falsely claim local mock providers perform schema `minLength` validation.

Validated:
- `cargo check -q -p fabro-workflow`
- `cargo check -q -p fabro-server`
- `cargo nextest run -p fabro-workflow pull_request`
- `cargo nextest run -p fabro-workflow workflow_run_with_vault_only_openai_codex_builds_pr_body`
- `cargo nextest run -p fabro-server create_run_pull_request_creates_and_persists_record`
- `cargo +nightly-2026-04-14 clippy -q -p fabro-workflow --all-targets -- -D warnings`
- `cargo +nightly-2026-04-14 fmt --check --all`