Implemented cleanup from the review pass.

Notes:
- Tried launching the three requested parallel review agents twice, but the subagent wait calls failed with infrastructure “item not found” errors. I closed those agents and completed the review manually.
- Kept the existing `RangedU64ValueParser` in `args.rs` because the simpler `clap::value_parser!(usize).range(1..)` form does not compile with this Clap setup.

Cleanup applied:
- `lib/crates/fabro-cli/src/commands/model.rs`
  - `configured_model_test_status` now returns an explicit `failed: bool`.
  - Bulk failure counting now uses that semantic flag instead of inferring failure from `Color::Red`, avoiding coupling command behavior to presentation color.

Verification passed:
- `cargo check -p fabro-cli`
- `cargo nextest run -p fabro-cli --test it cmd::model_test`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`
- `cargo nextest run -p fabro-cli` — 766 passed, 34 skipped.