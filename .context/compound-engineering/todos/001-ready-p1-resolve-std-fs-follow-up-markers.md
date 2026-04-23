---
status: ready
priority: p1
issue_id: "001"
tags: [rust, clippy, async-io, std-fs]
dependencies: []
---

## Problem Statement

Several Rust crates still contain `FOLLOW-UP:` markers related to blocking `std::fs` or sync I/O on async paths. The requested work is to execute the implementation plan in `~/.claude/plans/we-ll-feal-with-std-fs-jaunty-feigenbaum.md` and finish the refactors or tighten the remaining sync justifications.

## Findings

- The repo is currently on `main`, and the user explicitly approved proceeding there.
- `docs/solutions/` is not present, so there are no repo learnings to consult for this task.
- The current code matches the plan buckets across `fabro-agent`, `fabro-devcontainer`, `fabro-llm`, and `fabro-workflow`.

## Proposed Solutions

- Execute the plan in bucket order, using targeted failing checks before each production change where feasible.
- Prefer async propagation for truly async paths and `spawn_blocking` only at natural async boundaries.
- Remove or narrow `#[expect(clippy::disallowed_methods)]` annotations once the production sites are fixed.

## Recommended Action

Implement the plan directly, verify each bucket with crate-level tests or lint checks, then run the final formatting, clippy, workspace tests, and `FOLLOW-UP` sweep.

## Acceptance Criteria

- All `FOLLOW-UP:` markers under `lib/crates/` are removed.
- The planned async refactors and `spawn_blocking` boundary changes are implemented.
- Formatting and workspace clippy pass.
- Relevant crate tests pass during incremental verification.

## Work Log

- 2026-04-19: Created execution todo, confirmed branch choice with the user, and started inspecting the planned call sites.

