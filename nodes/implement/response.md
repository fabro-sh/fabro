All good. Let me provide a summary of all the changes made:

## Summary of Changes

### 1. New file: `lib/crates/fabro-workflows/src/cli/asset.rs`
- **`AssetEntry` struct** — represents an individual asset file with `node_slug`, `retry`, `relative_path`, `absolute_path`, and `size` fields. Implements `serde::Serialize` for JSON output.
- **`scan_assets(run_dir, node_filter)`** — walks `{run_dir}/artifacts/assets/*/retry_*/manifest.json`, deserializes each `AssetCollectionSummary`, stats each file, and returns `Vec<AssetEntry>`. Supports optional node filtering.
- **`AssetListArgs`** — clap args: `run_id` (positional), `--node` (filter), `--json` (JSON output)
- **`AssetCpArgs`** — clap args: `source` (positional, supports `RUN_ID` or `RUN_ID:path`), `dest` (positional, defaults to `.`), `--node` (filter), `--tree` (preserve directory structure)
- **`parse_source()`** — splits `RUN_ID:path` on first colon, using the same local-path detection logic as `cp.rs:split_run_path()`
- **`list_command()`** — prints a formatted table (NODE, RETRY, SIZE, PATH) with total count and size, or JSON array
- **`cp_command()`** — copies assets flat or with `--tree` structure; detects filename collisions in flat mode; supports copying a specific asset via `RUN_ID:path` syntax
- **Tests** — 4 unit tests for `parse_source()` covering bare run IDs, paths with colons, absolute paths, and relative paths

### 2. Modified: `lib/crates/fabro-workflows/src/cli/mod.rs`
- Added `pub mod asset;` to expose the new module

### 3. Modified: `lib/crates/fabro-workflows/src/cli/runs.rs`
- Changed `format_size` from `fn` to `pub(crate) fn` so it can be reused by the asset module

### 4. Modified: `lib/crates/fabro-cli/src/main.rs`
- Added `AssetCommand` enum with `List` and `Cp` variants
- Added `Asset { command: AssetCommand }` to the `Command` enum
- Added `command_name` match arms returning `"asset list"` and `"asset cp"`
- Added `main_inner` dispatch calling `asset::list_command` and `asset::cp_command`