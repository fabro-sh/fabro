Net reduction of **137 lines** from cleanup. Here's a summary of what was fixed:

## Summary of Fixes

1. **Deduplicated 5 helper functions** (`create.rs` → imports from `run.rs`): `resolve_cli_goal`, `apply_goal_override`, `parse_sandbox_provider`, `resolve_sandbox_provider`, `resolve_model_provider` were copied verbatim into `create.rs`. Made the `run.rs` versions `pub(crate)` and replaced the copies with imports. **(-92 lines)**

2. **Eliminated `ProgressAwareInterviewer` hide/show duplication**: Removed the private `hide_bars()`/`show_bars()` methods that duplicated `ProgressUI::hide_bars/show_bars`. The `ask()` and `inform()` methods now call through the public `ProgressUI` methods via the mutex lock.

3. **Removed unnecessary `Arc<Mutex<>>` from `attach.rs`**: `ProgressUI` is only accessed from the main loop (single-threaded), so the `Arc<Mutex<>>` wrapping was unnecessary overhead. Changed to bare `ProgressUI` with `&mut` access. Updated `drain_remaining` accordingly.

4. **Cached PID in attach loop**: The PID file was re-read and parsed every 100ms iteration. Now the PID is read once and cached in `cached_pid: Option<u32>`.

5. **Removed redundant `exists()` in `FileInterviewer`**: Replaced `if path.exists() { read_to_string() }` with direct `read_to_string()` + match on `ErrorKind::NotFound`. Eliminates an extra filesystem stat syscall per poll iteration.

6. **Avoided `.cloned()` on arguments Value**: In `handle_json_line`, the tool call arguments JSON tree was being deep-cloned. Changed to borrow with `unwrap_or(&empty)` since `on_tool_call_started` takes `&Value`.

7. **Fixed TOCTOU in `start.rs`**: Replaced `exists()`-then-load patterns with direct load + match on result. Status validation now does `RunStatusRecord::load()` directly and matches errors gracefully. Spec validation now calls `RunSpec::load()` directly.

8. **Simplified `RunSpec → RunArgs` sandbox mapping**: Added `From<SandboxProvider> for CliSandboxProvider` (reverse of existing conversion), replacing the 15-line exhaustive match with `.map(CliSandboxProvider::from)`.

9. **Derived `PartialEq` on `RunSpec`**: Replaced 17 field-by-field `assert_eq!` calls in the roundtrip test with a single `assert_eq!(loaded, spec)`.

10. **Fixed missing newline at EOF** in `fabro-interview/Cargo.toml`.