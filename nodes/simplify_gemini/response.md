## Summary

**One issue found and fixed** from the Code Quality review (Agent 2):

**Duplicated error-handling boilerplate → extracted `clone_repo()` helper method**

The `initialize()` method had 3–4 separate error sites that each copy-pasted the same ~10 lines: format error message (with `github_app.is_none()` conditional), emit `GitCloneFailed`, emit `InitializeFailed`, return `Err`. The agent extracted all clone logic into a private `clone_repo(&self, sandbox, params) -> Result<(), String>` method that:
- Handles credential resolution, git clone, and push credential setup internally
- Emits `GitCloneStarted` / `GitCloneCompleted` events internally  
- Returns a plain `Err(String)` on failure

The caller in `initialize()` now has a single error-handling site:
```rust
if let Err(err) = self.clone_repo(&sandbox, clone_params).await {
    self.emit(SandboxEvent::GitCloneFailed { url: ..., error: err.clone() });
    self.emit(SandboxEvent::InitializeFailed { ... });
    return Err(err);
}
```

This also fixed a pre-existing inconsistency where one failure path (`parse_github_owner_repo`) emitted `GitCloneFailed` but missed `InitializeFailed`.

**Other agents' findings**: Code Reuse (Agent 1) and Efficiency (Agent 3) found no actionable issues. The refactoring is a net efficiency improvement (removes redundant `detect_repo_info` call from `initialize()`), and no new code duplicates existing utilities.