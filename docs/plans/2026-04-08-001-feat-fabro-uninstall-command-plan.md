---
title: "feat: Add `fabro uninstall` command"
type: feat
status: completed
date: 2026-04-08
origin: docs/ideation/2026-04-08-fabro-uninstall-ideation.md
deepened: 2026-04-08
---

# feat: Add `fabro uninstall` command

## Overview

Add a `fabro uninstall` top-level CLI command that reverses the effects of `fabro install` and `install.sh`. The command stops a running server, removes the `~/.fabro/` directory tree, cleans shell config PATH entries, and handles binary self-removal. Defaults to dry-run (preview) mode, requiring `--yes` to execute.

## Problem Frame

There is no supported way to uninstall Fabro. Users must manually `rm -rf ~/.fabro`, hunt for stale PATH entries in their shell configs, and figure out how to stop the server. This creates friction, erodes trust, and leaves detritus after uninstall.

## Requirements Trace

- R1. Remove the `~/.fabro/` directory and all contents
- R2. Stop a running server before removing files
- R3. Remove `# fabro` PATH lines from shell configs (.zshrc, .bashrc, .bash_profile, .config/fish/config.fish)
- R4. Default to dry-run with size reporting; require `--yes` to execute
- R5. Delete the binary when installed via `install.sh` to `~/.fabro/bin/`; print a hint otherwise
- R6. Support `--json` output for scriptability

## Scope Boundaries

- No selective/component uninstall (users can back up manually)
- No GitHub App deregistration via API
- No export/backup archive feature
- No telemetry farewell event
- No per-repo cascade cleanup (repos can be cleaned individually via `fabro repo deinit`)

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-cli/src/commands/install.rs` — what `fabro install` creates (settings.toml, certs/, secrets.json)
- `apps/marketing/public/install.sh` — what the shell installer creates (binary at `~/.fabro/bin/fabro`, PATH lines with `# fabro` sentinel)
- `lib/crates/fabro-cli/src/commands/server/stop.rs` — `execute(storage_dir, timeout)`: SIGTERM, poll, SIGKILL, record+socket cleanup
- `lib/crates/fabro-cli/src/commands/server/record.rs` — `active_server_record_details()` for detecting running server
- `lib/crates/fabro-cli/src/commands/system/prune.rs` — dry-run-by-default pattern with `--yes`, size reporting via `format_size()`
- `lib/crates/fabro-cli/src/commands/repo/deinit.rs` — cleanup command pattern: green checkmarks, NotFound tolerance, `Vec<String>` return, `--json` support
- `lib/crates/fabro-util/src/home.rs` — `Home` struct with all path accessors (`root()`, `certs_dir()`, `storage_dir()`, etc.)
- `lib/crates/fabro-config/src/storage.rs` — `Storage` struct with storage path accessors
- `lib/crates/fabro-cli/src/args.rs` — `Commands` enum for command registration, `Commands::name()` for telemetry
- `lib/crates/fabro-cli/src/commands/upgrade.rs` — `std::env::current_exe()?.canonicalize()?` for binary path detection
- `lib/crates/fabro-config/src/user_config.rs` — `load_settings()` for resolving effective configuration including non-default `storage_dir`

### Institutional Learnings

No `docs/solutions/` directory exists. No prior learnings on install/uninstall patterns.

## Key Technical Decisions

- **Top-level command, not subcommand of `system`**: Matches `install`/`upgrade` placement. Users will search for `fabro uninstall` — discoverability matters. (see origin: docs/ideation/2026-04-08-fabro-uninstall-ideation.md)
- **`--yes` confirmation model (not `--dry-run`)**: Matches `system prune` pattern — destructive operations default to preview. The flag name `--yes` is already established in the codebase.
- **Use `Home::from_env()` as the canonical root**: Respects `$FABRO_HOME` for non-default installations. Never hardcode `~/.fabro/`.
- **Shell config cleanup uses exact `# fabro` sentinel match**: The `install.sh` script writes `# fabro` as a marker comment before the PATH export. Match with `line.trim() == "# fabro"` (exact match, not substring) to avoid deleting unrelated lines like `# fabro workflow helper`. After matching the sentinel, validate that the following line matches an expected PATH pattern (`export PATH=` or `fish_add_path`) before removing it — this prevents accidentally deleting an innocent line if the file was manually edited.
- **Binary path resolved during inventory, not after deletion**: `std::env::current_exe()?.canonicalize()?` must be called during the inventory phase (Unit 2) and stored. On macOS, `canonicalize()` fails with `NotFound` after the file is deleted by `remove_dir_all`. The stored path is used later by Unit 5.
- **Binary self-deletion is the final step**: On Unix, unlinking a running binary works (the inode stays alive until the process exits). Must be absolutely last since nothing can run after the binary is gone.
- **Do not call `stop::execute()` without a guard**: `server::stop::execute()` calls `std::process::exit(1)` when no server is running. The uninstall command must check `record::active_server_record_details()` first and only call `stop::execute()` when a server is confirmed running.
- **Resolve `storage_dir` through settings, not just `Home`**: If `settings.toml` configures a non-default `storage_dir`, the server record lives at that custom path. Load effective settings via `user_config::load_settings()` during inventory to resolve the actual `storage_dir`.
- **Compute-once inventory**: The inventory is built once (Unit 2) and consumed by all subsequent units. No step should re-detect artifacts during execution — the directory may already be partially deleted.
- **Exit code policy**: Exit 0 only if all critical steps (server stop, directory removal) succeeded. Exit 1 if any critical step failed. Shell config cleanup and binary handling failures warn but do not affect the exit code.
- **Synchronous implementation**: Server stop logic (`stop::execute`) is synchronous. The uninstall function itself should be async (matching the dispatch pattern in main.rs) but can call sync helpers.

## Open Questions

### Resolved During Planning

- **Should uninstall clean up shell configs that `fabro install` (Rust) didn't create?** Yes — `install.sh` creates them, and the user experience of "uninstall" should reverse the full installation, not just the Rust command's portion.
- **What if the server won't stop?** Follow the existing SIGTERM→SIGKILL pattern from `server/stop.rs`. If SIGKILL fails (shouldn't happen on Unix), warn and continue with removal.

- **Should the dry-run preview show active workflow run count?** Yes — if the server is running with in-flight workflows, stopping it will terminate them. The preview should report the count so users can make an informed decision. The run count can be obtained from the server API if available, or noted as "server running (active runs unknown)" if the API is unreachable.
- **What about `$ZDOTDIR` mismatch between install and uninstall time?** If the user's `$ZDOTDIR` was set differently at install time versus uninstall time, the uninstall will look in the wrong file. This is an inherent limitation — document it but do not try to solve it.

### Deferred to Implementation

- **Fish shell syntax differences**: `fish_add_path` vs `export PATH=` — the removal logic needs shell-specific handling. Determine exact patterns during implementation.

## Implementation Units

- [x] **Unit 1: Command registration and skeleton**

**Goal:** Register `fabro uninstall` as a top-level CLI command with args parsing.

**Requirements:** R4, R6

**Dependencies:** None

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs`
- Modify: `lib/crates/fabro-cli/src/commands/mod.rs`
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Create: `lib/crates/fabro-cli/src/commands/uninstall.rs`

**Approach:**
- Add `UninstallArgs` struct with `--yes` bool field (clap attribute: `#[arg(long)]`)
- Add `Uninstall(UninstallArgs)` variant to `Commands` enum with doc comment `/// Uninstall Fabro from this machine`
- Add `Self::Uninstall(_) => "uninstall"` to `Commands::name()`
- Add `pub(crate) mod uninstall;` to `commands/mod.rs`
- Add dispatch arm in main.rs calling `commands::uninstall::run_uninstall(&args, &globals).await?`
- Skeleton `run_uninstall` that prints "not yet implemented" and returns Ok

**Patterns to follow:**
- `InstallArgs` struct and `Commands::Install` variant in `args.rs`
- Dispatch pattern in `main.rs` (line ~176)
- Module declaration pattern in `commands/mod.rs`

**Test scenarios:**
- Happy path: `fabro uninstall --help` outputs usage text including `--yes` flag description
- Happy path: `fabro uninstall` (no args) parses successfully and runs the skeleton

**Verification:**
- `cargo build -p fabro-cli` succeeds
- `fabro uninstall --help` shows the expected usage

---

- [x] **Unit 2: Inventory and dry-run preview**

**Goal:** Discover all Fabro artifacts on the system, compute sizes, and display a preview manifest. When `--yes` is not passed, this is the complete behavior.

**Requirements:** R1, R2, R3, R4, R5, R6

**Dependencies:** Unit 1

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/uninstall.rs`

**Approach:**
- Use `Home::from_env()` to resolve the root directory
- Check if `Home::root()` exists; if not, print "Fabro is not installed" and exit 0 (skip settings loading entirely)
- If home exists, attempt to load effective settings via `user_config::load_settings()` to resolve actual `storage_dir`. If settings loading fails (e.g., settings.toml missing or corrupt), fall back to `Home::from_env().storage_dir()` — this handles partial installs and repeated uninstall attempts
- Build an inventory struct containing all information needed by subsequent units:
  - `home_root`: resolved Home root path
  - `storage_dir`: resolved storage directory path
  - `home_exists`: whether the home directory exists
  - `home_size`: total size of home directory (recursive walk)
  - `server_running`: whether a server is detected via `record::active_server_record_details(&storage_dir)`
  - `shell_configs`: list of shell config file paths that contain the `# fabro` sentinel (exact match: `line.trim() == "# fabro"`)
  - `binary_path`: resolved path from `std::env::current_exe()?.canonicalize()?` (must be resolved NOW, before any deletion)
  - `binary_is_managed`: whether `binary_path` starts with `home_root`
- Shell config files to scan: `$ZDOTDIR/.zshrc` or `~/.zshrc`, `~/.bashrc`, `~/.bash_profile`, `~/.config/fish/config.fish`
- In dry-run mode (no `--yes`): print each item that would be removed with its size, including active run warning if server is running, then print `"Pass --yes to confirm."` summary
- Support `--json` output: serialize inventory as JSON to stdout
- Follow the `system prune` output style for human-readable format

**Patterns to follow:**
- `system prune` dry-run output format ("would delete: ...", "N item(s) would be deleted (X freed)")
- `format_size()` for human-readable byte formatting
- `console::Style` / `Styles::detect_stderr()` for colored output
- `GlobalArgs.json` check for JSON vs human output

**Test scenarios:**
- Happy path: preview with populated `~/.fabro/` lists all directories and files with sizes
- Happy path: preview with `--json` outputs structured JSON to stdout
- Edge case: `~/.fabro/` does not exist — prints "Fabro is not installed" and exits cleanly
- Edge case: shell config files exist but contain no fabro lines — omitted from preview
- Edge case: binary is not in `~/.fabro/bin/` — preview notes it must be removed manually
- Happy path: running server detected — preview notes it will be stopped and warns about active runs
- Edge case: `current_exe()` or `canonicalize()` fails — inventory stores `None` for binary path, warns in preview

**Verification:**
- `fabro uninstall` (no `--yes`) prints a complete manifest and does NOT delete anything
- `fabro uninstall --json` outputs valid JSON with inventory details
- Preview accurately reflects what exists on disk

---

- [x] **Unit 3: Server shutdown and directory removal**

**Goal:** When `--yes` is passed, stop a running server and remove `~/.fabro/` and all contents.

**Requirements:** R1, R2

**Dependencies:** Unit 2

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/uninstall.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/uninstall.rs`

**Approach:**
- Use the inventory's `server_running` and `storage_dir` fields (computed in Unit 2)
- **Critical**: Only call `server::stop::execute()` when `inventory.server_running` is true. `stop::execute()` calls `std::process::exit(1)` when no server is found — calling it unconditionally would terminate the process before any cleanup happens.
- If running, call `server::stop::execute(&inventory.storage_dir, Duration::from_secs(5))`
- If not running, skip with no error
- **Safety guardrail before deletion**: Validate `inventory.home_root` is reasonable before calling `remove_dir_all`. Refuse to proceed if the resolved path is a filesystem root (`/`), the user's home directory (`$HOME`), or does not contain an expected marker file (e.g., `settings.toml` or `certs/`). This prevents catastrophic data loss from a misconfigured `$FABRO_HOME`.
- After validation, remove `inventory.home_root` with `std::fs::remove_dir_all`
- If `storage_dir` differs from the default and is outside `home_root`, validate it contains Fabro artifacts (e.g., `secrets.json` or `store/`) before removing
- Handle `ErrorKind::NotFound` gracefully (already uninstalled)
- Report each step with green checkmark output following `deinit.rs` pattern
- The `fabro.sock` socket file is inside `~/.fabro/` so it's removed as part of the directory deletion

**Patterns to follow:**
- `server::stop::execute()` for server shutdown (call ONLY when server is confirmed running)
- `record::active_server_record_details()` for server detection (already done in inventory)
- `deinit.rs` checkmark output pattern
- `RunScratch::remove()` for NotFound tolerance

**Test scenarios:**
- Happy path: with `--yes` and no running server, `~/.fabro/` is removed successfully
- Happy path: with `--yes` and running server, server is stopped before removal
- Edge case: `~/.fabro/` does not exist — reports "nothing to remove" without error
- Error path: server stop fails (SIGKILL also fails) — warns and continues with removal
- Error path: `$FABRO_HOME` set to `/` — refuses to delete, prints error
- Error path: `$FABRO_HOME` set to `$HOME` — refuses to delete, prints error
- Error path: `$FABRO_HOME` points to a directory without Fabro marker files — refuses to delete
- Integration: after removal, `~/.fabro/` directory does not exist on disk

**Verification:**
- `fabro uninstall --yes` removes `~/.fabro/` completely
- A running server is stopped before file removal
- No orphaned processes remain after uninstall

---

- [x] **Unit 4: Shell config cleanup**

**Goal:** Remove PATH lines that `install.sh` added to shell configuration files, using the `# fabro` sentinel comment.

**Requirements:** R3

**Dependencies:** Unit 2 (uses shell config detection from inventory)

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/uninstall.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/uninstall.rs`

**Approach:**
- Use the inventory's `shell_configs` list (detected in Unit 2)
- For each shell config file in the list:
  - Read the file contents
  - Find lines where `line.trim() == "# fabro"` (exact match, not substring — avoids matching `# fabro workflow helper`)
  - Validate that the following line matches an expected PATH pattern before removing it:
    - zsh/bash: following line starts with `export PATH=`
    - fish: following line starts with `fish_add_path`
  - If the following line does NOT match, remove only the sentinel line (defensive — the file was manually edited)
  - Remove the sentinel line AND the validated following line
  - Write the modified contents back
  - Handle edge cases: sentinel at end of file, multiple sentinels, sentinel with no following line
- Report each modified file with the deinit checkmark pattern
- If a shell config file is not found or has no fabro lines, skip silently

**Patterns to follow:**
- `deinit.rs` reporting pattern (green checkmarks per item)
- **Atomic write pattern**: Write modified content to a temporary file in the same directory, then `std::fs::rename()` over the original (atomic on POSIX). This prevents a truncated/corrupt dotfile if the process crashes mid-write. Check `std::fs::symlink_metadata()` before modifying — if the file is a symlink, follow it (matching `std::fs::read_to_string` behavior) but note this in output so users with dotfile managers are aware.

**Test scenarios:**
- Happy path: `.zshrc` with `# fabro` + PATH line — both lines removed, rest of file intact
- Happy path: `.bashrc` with `# fabro` + PATH line — both lines removed
- Happy path: `.bash_profile` with `# fabro` + PATH line — both lines removed
- Happy path: `config.fish` with `# fabro` + `fish_add_path` — both lines removed
- Edge case: shell config exists but has no `# fabro` line — file is not modified
- Edge case: shell config file does not exist — no error
- Edge case: `# fabro` is the last line in the file (no following PATH line) — only sentinel line removed
- Edge case: multiple `# fabro` blocks in the same file — all are removed
- Edge case: `# fabro` followed by an unrelated line (not PATH export) — only sentinel removed, following line preserved
- Edge case: line contains `# fabro` as substring (`# fabro-related`) — not matched, file unchanged
- Integration: after cleanup, opening a new shell does not add fabro to PATH

**Verification:**
- Shell config files have fabro lines removed without corrupting other content
- Files without fabro lines are not modified (mtime unchanged)

---

- [x] **Unit 5: Binary status reporting and final output**

**Goal:** Report whether the binary was already removed (managed install) or print a removal hint (external install). Print final summary.

**Requirements:** R5

**Dependencies:** Unit 3 (must run after directory removal)

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/uninstall.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/uninstall.rs`

**Approach:**
- Use the inventory's pre-resolved `binary_path` and `binary_is_managed` fields (resolved in Unit 2 before any deletion — `canonicalize()` fails on macOS after the file is deleted)
- If `binary_is_managed` is true: the binary was inside `~/.fabro/bin/` and was already removed by Unit 3's `remove_dir_all` — report as removed
- If `binary_is_managed` is false: print a tailored hint
  - Check if path contains `/Cellar/` (Homebrew): "run `brew uninstall fabro`"
  - Check if path contains `.cargo/bin/` (cargo): "run `cargo uninstall fabro`"
  - Otherwise: "The fabro binary at {path} must be removed manually."
- If `binary_path` is `None` (resolution failed in Unit 2): warn and skip
- Print final summary: "Fabro has been uninstalled." (bold, to stderr)
- For `--json` output: include `binary_removed` and `binary_hint` fields

**Patterns to follow:**
- `deinit.rs` final summary line pattern (bold text)
- Package manager path detection is heuristic — keep it simple

**Test scenarios:**
- Happy path: binary was in `~/.fabro/bin/` and was already removed by directory deletion — reports binary removed
- Happy path: binary is in `/opt/homebrew/Cellar/` — prints "run `brew uninstall fabro`" hint
- Happy path: binary is in `~/.cargo/bin/` — prints "run `cargo uninstall fabro`" hint
- Edge case: binary path detection fails (`current_exe()` error) — warns but does not fail the overall uninstall
- Happy path: final summary message is printed after all steps complete

**Verification:**
- When binary was installed via `install.sh`, it is deleted or confirmed deleted
- When binary was installed via other means, a clear removal instruction is printed
- `fabro uninstall --yes --json` outputs valid JSON including inventory, execution results, and binary status
- The command exits successfully after all steps

## System-Wide Impact

- **Interaction graph:** The uninstall command interacts with: `server::stop` (server lifecycle), `Home`/`Storage` (path resolution), shell config files (external to fabro), and the running binary itself. No callbacks, middleware, or observers are affected.
- **Error propagation:** Each step warns on failure but continues. Critical steps (server stop, directory removal) affect the exit code (exit 1 on failure). Non-critical steps (shell config, binary) warn only. A partial uninstall is better than aborting on the first error.
- **State lifecycle risks:** Stopping the server terminates in-flight workflow runs — the dry-run preview warns about this. Mitigated by mandatory server stop as the first step (prevents filesystem corruption) and by defaulting to dry-run (gives user a chance to see the warning before committing).
- **API surface parity:** No API endpoint equivalent needed — uninstall is a local-only operation.
- **Unchanged invariants:** `fabro install`, `fabro server`, `fabro repo deinit`, and `system prune` are not modified by this plan. The uninstall command reuses their code but does not change their behavior.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Shell config surgery corrupts user's dotfiles | Use sentinel-based detection (not regex on PATH content). Only remove exact `# fabro` + next line. Write tests with realistic file content. |
| Binary self-deletion race condition | Delete binary as the absolute last step. On Unix, unlink of a running binary is safe (inode persists until process exits). |
| `$FABRO_HOME` set to dangerous path (`/`, `$HOME`) | Safety guardrail: validate resolved path is not a root or home dir, and contains expected marker files, before `remove_dir_all`. |
| Server stop timeout blocks uninstall | Use a short timeout (5s). SIGKILL as fallback. Continue with removal even if stop fails. |
| `stop::execute()` exits process when no server found | Guard with `active_server_record_details()` check; never call `stop::execute()` unconditionally. |
| `canonicalize()` fails after file deletion on macOS | Resolve binary path during inventory phase (Unit 2), before any deletion occurs. |
| Non-default `storage_dir` in settings.toml | Load settings during inventory to resolve actual storage_dir; don't assume it's inside `~/.fabro/`. |
| `$ZDOTDIR` differs between install and uninstall time | Known limitation — document it. Uninstall uses current `$ZDOTDIR`. |
| Partial `remove_dir_all` failure (locked files on macOS) | Warn and report which files remain. Exit 1. User can retry or manually remove. |

## Sources & References

- **Origin document:** [docs/ideation/2026-04-08-fabro-uninstall-ideation.md](docs/ideation/2026-04-08-fabro-uninstall-ideation.md)
- Related code: `lib/crates/fabro-cli/src/commands/install.rs`, `lib/crates/fabro-cli/src/commands/server/stop.rs`, `lib/crates/fabro-cli/src/commands/system/prune.rs`, `lib/crates/fabro-cli/src/commands/repo/deinit.rs`
- Related code: `lib/crates/fabro-util/src/home.rs`, `lib/crates/fabro-config/src/storage.rs`
- Related code: `apps/marketing/public/install.sh`
