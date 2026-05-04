## Summary

Reviewed the 523-line diff with three parallel agents. Aggregated findings and fixed the high-value items:

### Fixed in `lib/crates/fabro-cli/src/commands/model.rs`
1. **Dropped redundant `failed: bool`** — `configured_model_test_status` now returns `(Color, String)`. The completed result is no longer a tuple `(CompletedModelTest, bool)`; `failures` is incremented based on `result_color == Color::Red`. This also removes the confusing inner-vs-outer `completed` shadowing in `for (completed, failed) in completed`.
2. **Removed unnecessary `status.clone()`** — reordered the row-build to push the JSON row using `&completed.status` first, then move `status` into the table cell. One `String` allocation saved per configured model.

### Fixed in `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`
3. **Fixed `add_permits` off-by-one** — `Semaphore::new(0).add_permits(self.expected)` over-permitted by 1 (only `expected - 1` tasks can ever be blocked, since the `expected`-th arrival is the trigger). Now uses `self.expected - 1` with a comment explaining why.
4. **Removed dead `expected == 0` defense** — replaced `released: AtomicBool::new(expected == 0)` with `assert!(expected > 0)` per the repo guideline against validation for impossible scenarios.
5. **Deduped 5-model fixtures** — extracted `FIVE_ANTHROPIC_MODEL_IDS` const and `five_anthropic_models()` helper. The ordering test's reverse-delay map now derives from the same constant, and the final assert reuses `FIVE_ANTHROPIC_MODEL_IDS.to_vec()` instead of a duplicated literal list.

### Skipped (intentional)
- **Color-as-discriminant refactor** (replace `Color`/`String` with an `Outcome` enum): larger refactor that touches `model_test_row_from_status` and the single-model path; pre-existing pattern, plan didn't request it.
- **Parameter sprawl on `test_models_via_server`**: pre-existing, not caused by this change.
- **Help-snapshot ordering**: clap's heuristic; not load-bearing.
- **Replace `tokio::time::sleep` with `Notify`-based deterministic ordering**: ~250ms test runtime is acceptable; current implementation works reliably.
- **Unify single-model and bulk status helpers**: meaningful semantic divergence (Skip is yellow + `skipped++` for single-model, red + `failures++` for bulk-after-listing) — they shouldn't share code.

### Verification
- `cargo +nightly-2026-04-14 clippy -p fabro-cli --all-targets -- -D warnings` ✅
- `cargo +nightly-2026-04-14 fmt --check --all` ✅
- `cargo nextest run -p fabro-cli --test it cmd::model_test` — 12/12 pass ✅