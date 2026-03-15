All checks pass. Here's a summary of what was fixed:

## Summary of Changes

### Fixed Issues

1. **Leaky abstraction + parameter sprawl + behavioral bug (Medium):** Moved the sleep guard acquisition from inside `fabro_workflows::cli::run::run_command()` to the call site in `fabro-cli/src/main.rs`. This:
   - Removed `prevent_idle_sleep: bool` parameter from `run_command` (was the 7th positional arg)
   - Removed `fabro-beastie` dependency and `sleep_inhibitor` feature from `fabro-workflows` entirely — sleep prevention is a CLI concern, not a workflow engine concern
   - Fixed a bug where `--run-branch` resume would skip sleep inhibition (the old guard was acquired *after* the early return to `run_from_branch`)
   - Simplified `fabro-cli/Cargo.toml` feature — no longer needs to forward to `fabro-workflows/sleep_inhibitor`

2. **Copy-paste duplication (Low):** Extracted `spawn_inhibitor(cmd, args)` helper in `linux.rs`, eliminating ~30 lines of duplicated process spawning boilerplate between `try_systemd_inhibit()` and `try_gnome_inhibit()`.

3. **Inconsistent reason string (Low):** Normalized the macOS reason from `"fabro workflow in progress"` to `"Workflow in progress"` to match the Linux backends.

4. **Missing trailing newlines:** Restored trailing newlines in `fabro-cli/Cargo.toml` and `fabro-workflows/Cargo.toml`.