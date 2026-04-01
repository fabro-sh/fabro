All clean. Here's a summary of all changes made:

## Summary of Changes

### Files modified

**1. `lib/crates/fabro-cli/src/commands/doctor.rs`**

- **Import additions**: Added `Path` to `std::path::{Path, PathBuf}` import and added `use fabro_config::FabroSettingsExt;` for the `storage_dir()` method on `FabroSettings`.

- **New pure function `check_storage_dir`**: Added between `check_config` and `check_llm_providers`. Takes `path: &Path`, `readable: bool`, `writable: bool` and returns a `CheckResult`:
  - **Pass**: directory exists, readable, and writable. Summary shows the resolved path.
  - **Error (not exists)**: remediation suggests `mkdir -p <path>`.
  - **Error (permission issues)**: remediation suggests fixing permissions.
  - Verbose details always show existence/read/write status lines.

- **State gathering in `run_doctor`**: After config path checks, resolves the storage directory using `globals.storage_dir` (CLI arg / env var) with fallback to `cli_settings.storage_dir()`. Probes readability via `std::fs::read_dir()` and writability via `tempfile::tempfile_in()`.

- **Wired into "Required" section**: Inserted `check_storage_dir()` result after "Configuration" and before "LLM providers".

- **3 unit tests added** in `mod tests`:
  - `check_storage_dir_pass` — real tempdir → Pass
  - `check_storage_dir_not_exists` — nonexistent path → Error with `mkdir -p` remediation
  - `check_storage_dir_not_writable` — exists but not writable → Error with "Fix permissions" remediation

**2. `lib/crates/fabro-cli/tests/it/cmd/doctor.rs`**

- **Updated `dry_run_flag` snapshot**: Added the new `[✓] Storage directory ([STORAGE_DIR])` line.

- **2 new integration tests added**:
  - `storage_dir_shown_in_output` — runs `doctor --dry-run` and verifies "Storage directory" appears with `[STORAGE_DIR]` in the summary.
  - `storage_dir_missing_shows_error` — overrides `FABRO_STORAGE_DIR` to a nonexistent path, verifies `[✗]` error status and `mkdir -p` remediation text.