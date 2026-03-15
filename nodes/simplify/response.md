All clean. Here's a summary of what was fixed:

**Fixed:**
1. **Removed dead branch**: The `if all_excludes.is_empty()` check could never be true since `EXCLUDE_DIRS` always contains 14 entries. Removed the unreachable `format!("{GIT_REMOTE} add -A")` branch and kept only the pathspec-based command.
2. **Removed pointless `sort()` + `dedup()`**: The built-in excludes (`**/node_modules/**` format) and user globs use different formats, so lexicographic dedup would never match anything. Duplicate pathspecs are harmless for git anyway. Removed both calls.

**Skipped (not worth addressing):**
- Moving `EXCLUDE_DIRS` to a shared module — this was an explicit design choice in the plan, and the current placement is reasonable.
- `LazyLock` for the formatted patterns — this is a cold path (once per workflow stage), trivial allocation cost.
- The trailing newline formatting issue was auto-fixed by `cargo fmt`.