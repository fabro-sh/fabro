Both clean. 

## Summary of cleanups

Aggregated findings across the three review agents and fixed the actionable ones:

**Code reuse**
- **Unified `enforce_title_cap` and `pr_title_from_goal`'s truncation logic** into a shared `truncate_with_ellipsis(s, max) -> String` helper. Eliminates the duplicated `chars().count() > N → take(N-1).collect() + '…'` algorithm. `pr_title_from_goal` is now a one-liner.
- **Switched from field access to the `m.context_window()` accessor** in `truncation_caps`, matching the convention used by the other 4 catalog-consumer call sites in the workspace (`fabro-agent`, `fabro-llm`, etc.).

**Code quality**
- **Hoisted the magic `72`** into a module-level `PR_TITLE_MAX_CHARS: usize` constant, referenced from both the JSON schema (`maxLength: PR_TITLE_MAX_CHARS`) and `enforce_title_cap`. The system prompt's "max 72 characters" wording got an explicit sync-comment.
- **Replaced `(String, String)` return type with a named `PrContent { title, body }` struct** across the four `build_pr_body*` signatures and all five call sites. Prevents accidental field swap and self-documents at the call site.
- **Collapsed the six tier constants** (`MAX_GOAL_CHARS_LARGE/SMALL`, etc.) into a `TruncationCaps` struct with two `const`s (`TRUNCATION_LARGE`, `TRUNCATION_SMALL`). Call site went from `let (g, p, d) = truncation_caps(...)` to `let caps = truncation_caps(...)`.
- **Consolidated split `use` statements** at the top of the file. The file previously had ~150 lines of new constants/functions wedged between two `use` blocks; all imports are now at the top.
- **Eliminated the redundant double `enforce_title_cap`** in `maybe_open_pull_request` by moving it into the fallback branch only — the LLM-title branch is already capped inside the builder. Added a comment explaining why the fallback branch needs re-capping (`pr_title_from_goal` has a wider 120-char cap).
- **Fixed misleading `truncate_chars` doc comment** — it was documented as "chars" but actually byte-caps and aligns to a UTF-8 boundary. Doc now correctly says "bytes" and notes the char/byte equivalence for ASCII.

**Skipped (false positives or low value)**
- `Catalog::builtin()` cost — verified to be a static `LazyLock`, no rebuild per call.
- `PR_CONTENT_SCHEMA.clone()` — `generate_object` takes `Value` by value; clone is dwarfed by the LLM HTTP call.
- `pr_content_json` cross-crate sharing — only two external duplications, low value to expose via `test-support`.
- `setup_fallback_test_harness` partial overlap with the existing vault test — different shapes (one needs GitHub mock + final_patch event, the other doesn't); merging would tangle.
- Whitespace-leak in title — verified non-issue: builder trims via `enforce_title_cap(generated.title.trim())` before storing on the struct.
- `_vault_dir`/`_openai_server` keepalive pattern in test harness — already commented; standard Rust pattern.

**Verification**: `cargo check --workspace --all-targets` clean, `cargo nextest run -p fabro-workflow` 1057/1057 passed, `cargo nextest run -p fabro-server` 443/443 passed, `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` clean, `cargo +nightly-2026-04-14 fmt --check --all` clean.