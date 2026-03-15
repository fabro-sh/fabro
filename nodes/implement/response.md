All 179 tests pass, 0 failures. Let me provide a summary of all changes made:

## Summary of Changes

### 1. `lib/crates/fabro-workflows/src/asset_snapshot.rs`

- **Made `EXCLUDE_DIRS` public** (`const` → `pub const`) so it can be shared with `engine.rs`
- **Expanded `EXCLUDE_DIRS`** with 7 new entries: `.venv`, `venv`, `.cache`, `.tox`, `.pytest_cache`, `.mypy_cache`, `dist`
- **Added `MAX_FILE_COUNT` constant** (100) to limit the number of files collected
- **Added count check** in `select_files_to_collect()` — `if selected.len() >= MAX_FILE_COUNT { break; }` alongside the existing size budget check
- **Added test `select_files_enforces_count_limit`** — creates 150 small files, verifies only 100 are selected
- **Added test `build_find_command_excludes_venv`** — verifies all new directory names appear in the find command's prune clause

### 2. `lib/crates/fabro-workflows/src/engine.rs`

- **Merged built-in excludes into `git_checkpoint()`** — the `git add` command now always includes `EXCLUDE_DIRS` entries (converted to `**/dirname/**` glob format) merged with any user-configured `exclude_globs`, sorted and deduped
- **Added test `git_checkpoint_includes_builtin_excludes`** — sets up a real git repo with files in `node_modules/` and `.venv/`, calls `git_checkpoint` with empty user excludes, and verifies those directories were not staged

### Verification
- ✅ `cargo test -p fabro-workflows` — 179 passed, 0 failed
- ✅ `cargo clippy --workspace -- -D warnings` — clean
- ✅ `cargo fmt --check --all` — clean