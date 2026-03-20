All checks pass. Here's a summary of the two fixes:

1. **Removed unused `init_repo_with_remote` function** in `lib/crates/fabro-workflows/src/git.rs` (lines 1152-1195) — this was a test helper that was never called, causing a clippy `-D warnings` failure.

2. **Updated trycmd snapshots** in `lib/crates/fabro-cli/tests/cmd/model/` — the `list.trycmd` and `bare.trycmd` files needed updating because the catalog aliases column now includes the new hyphenated aliases (`gpt-54`, `gpt-54-pro`, `gpt-54-mini`), which changed the column width and content.