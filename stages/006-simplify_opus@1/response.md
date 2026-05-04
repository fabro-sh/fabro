All green. Here's a summary of what I fixed:

## Review Summary

Three parallel review agents reviewed the silent-degrade observability changes. Most findings were either pre-existing patterns out of this PR's scope (e.g. inlined `InitOptions` boilerplate appearing in 7 tests) or false positives (the per-iteration `.clone()` in the failover loop is unavoidable since `Event::Failover` owns its strings; the `tracing::warn!` + notice pairs are justified per the plan's "structured fields not in notice" rule).

## Fixes applied

1. **(Quality P1)** `pipeline/initialize.rs` — added a comment explaining why `let _ = initialize(...).await` discards the result in the new `worktree_skipped_no_git` test (the assertion runs against the captured event stream regardless of downstream resolution). This prevents a future reader from "fixing" it and silently losing the test.

2. **(Quality P2)** `handler/parallel.rs` — aligned the `parallel_base_checkpoint_failed` notice to render the error via `fabro_sandbox::display_for_log(&e)`, matching the structured-field rendering in the accompanying `tracing::warn!`. Removes a small message-quality asymmetry.

3. **(Quality P2)** `handler/prompt.rs` — added `use crate::event::Emitter;` to the test module so the three test-stub `one_shot` impls can write the bare `&Arc<Emitter>` consistent with the trait signature, instead of the qualified `&Arc<crate::event::Emitter>`.

## Skipped (false positives or out of scope)

- Extracting a `default_init_options(...)` test helper — pre-existing pattern across 7 tests; refactoring would be scope creep.
- Extracting an `emit_failover()` helper — only 2 call sites; not a strong duplication win.
- Lifting notice codes to shared constants — pre-existing pattern of raw string literals; new codes are single-producer.
- Adding an `Emitter::warn_notice(...)` helper — would touch many sites; the plan's per-site discrimination ("structured fields justify a separate warn") is intentional.
- Param sprawl on `one_shot` (5 args) — agent confirmed acceptable; sibling `run()` already has 7 args with `#[allow(clippy::too_many_arguments)]`.
- Lost `(prompt)` log breadcrumb — `Event::Failover`'s structured `stage` field still distinguishes the path.

## Verification

- ✅ `cargo check --workspace --all-targets`
- ✅ `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`
- ✅ `cargo +nightly-2026-04-14 fmt --check --all`
- ✅ `cargo nextest run -p fabro-workflow -p fabro-sandbox` — 1173 tests passed