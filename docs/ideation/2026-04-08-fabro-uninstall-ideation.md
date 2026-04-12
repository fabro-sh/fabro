---
date: 2026-04-08
topic: fabro-uninstall
focus: add `fabro uninstall` command — the opposite of install
---

# Ideation: `fabro uninstall` Command

## Codebase Context

- `fabro install` creates `~/.fabro/` with settings.toml, certs/, storage/ (secrets, server state, logs, scratch, store, artifacts), skills/, workflows/, logs/, tmp/
- `install.sh` modifies shell configs (.zshrc/.bashrc/.config/fish) adding PATH entries with `# fabro` sentinel comment
- `Home` (fabro-util) and `Storage` (fabro-config) structs are canonical path registries
- `system prune` provides the established dry-run/--yes/size-reporting UX pattern
- `server stop` handles graceful SIGTERM→SIGKILL with socket cleanup
- Binary installed to `~/.fabro/bin/fabro` by install.sh; brew/cargo installs go elsewhere

## Ranked Ideas

### 1. Core Mirror Uninstall
**Description:** `fabro uninstall` removes `~/.fabro/` entirely, using `Home` and `Storage` structs as the canonical path registry. Compose existing deletion logic rather than writing new teardown code.
**Rationale:** Home/Storage structs enumerate every path, so future paths added to install automatically appear in uninstall — zero maintenance drift.
**Downsides:** None significant — table stakes.
**Confidence:** 95%
**Complexity:** Low
**Status:** Unexplored

### 2. Server Shutdown First
**Description:** Before removing any files, detect a running server and gracefully stop it, reusing `server stop` logic (SIGTERM→SIGKILL). Only then proceed with file deletion.
**Rationale:** Deleting files under a running server creates orphaned processes, dangling sockets, and corrupt state.
**Downsides:** None — correctness requirement.
**Confidence:** 95%
**Complexity:** Low
**Status:** Unexplored

### 3. Shell Config Cleanup
**Description:** Remove PATH lines from `.zshrc`/`.bashrc`/`.config/fish` that `install.sh` added, using the `# fabro` sentinel comment as grep target.
**Rationale:** Stale PATH entries are the #1 complaint after CLI uninstalls. Sentinel comment makes surgical removal safe.
**Downsides:** Shell config surgery requires careful testing across shells.
**Confidence:** 85%
**Complexity:** Medium
**Status:** Unexplored

### 4. Dry-Run by Default
**Description:** Follow `system prune` pattern: list everything that would be removed with sizes, require `--yes` to confirm. Support `--json`.
**Rationale:** Already proven UX in the codebase. Prevents "oops" moments.
**Downsides:** None.
**Confidence:** 95%
**Complexity:** Low
**Status:** Unexplored

### 5. Binary Self-Removal
**Description:** If binary lives inside `~/.fabro/bin/` (install.sh path), delete it as final step. For brew/cargo, print a hint instead.
**Rationale:** Users expect "uninstall" to mean "gone." Simple conditional based on `current_exe()` path.
**Downsides:** Self-deleting binary must be last step. Detection heuristic could be wrong if user moved the binary.
**Confidence:** 75%
**Complexity:** Low
**Status:** Unexplored

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Selective component uninstall | YAGNI — users can back up manually |
| 2 | GitHub App deregistration | Scope creep — remote API during teardown is fragile |
| 3 | Export before destroy | Gold-plating — `cp -r ~/.fabro` suffices |
| 4 | Telemetry farewell event | Marginal value, unwelcome phoning home |
| 5 | Install manifest | Over-engineering for small deterministic footprint |
| 6 | Per-repo cascade cleanup | Filesystem scanning is slow and presumptuous |
| 7 | Secure secrets shredding | Security theater on modern SSDs |
| 8 | Reframe as `system reset` | Poor discoverability |
| 9 | Doctor extension | Mixing diagnostic and destructive ops |
| 10 | Install --undo flag | Terrible discoverability |
| 11 | Teardown facet trait | Premature abstraction for ~5 steps |
| 12 | Event-sourced uninstall | Requires rewriting install first |
| 13 | Restoration script | Worse version of export (already cut) |
| 14 | Server-mediated uninstall | Circular dependency |
| 15 | Manifest + Selective | Both halves cut |
| 16 | Doctor-informed uninstall | Coupling for no benefit |

## Session Log
- 2026-04-08: Initial ideation — ~40 generated across 5 agents, deduped to 22, 5 survived. All 5 accepted as facets of one command. Proceeding to plan.
