Everything looks clean. Here's a summary of what was fixed:

## Summary

**Three issues fixed:**

1. **Inconsistent `GitCloneFailed` event boundary** — `GitCloneStarted` and `GitCloneCompleted` were emitted inside `clone_repo()`, but `GitCloneFailed` was emitted by the caller in `initialize()`. Moved `GitCloneFailed` into `clone_repo()` so all clone lifecycle events are co-located. The caller now only emits `InitializeFailed` (the higher-level event). Updated the doc comment accordingly.

2. **`origin_url` only set when clone token exists** — Unlike `ExeSandbox` which sets `origin_url` unconditionally after clone, `DaytonaSandbox` only set it inside `if let Some(token)`. This meant `origin_url()` returned `None` for public repos even after successful clone. Fixed by moving `origin_url.set()` outside the token-gated block.

3. **Clippy warning: `match_result_ok`** — `sandbox.process().await.ok()` matched with `if let Some(ps)` should use `if let Ok(ps)` directly. Fixed.

**Reviewed and confirmed clean (no changes needed):**
- `GitCloneParams` struct is new and doesn't duplicate any existing type
- The `detect_repo_info` + `ssh_url_to_https` pattern at call sites (run.rs and test) is appropriate — these are different contexts (production vs test) with different sources for cwd
- No unnecessary `.clone()` calls — the string clones are needed for the event system
- No redundant computations after the simplification agents' changes (auth URL is only built once, for push credentials)