# Reduce Cargo Target APFS Churn Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make day-to-day Cargo builds and cleanup less painful on macOS/APFS by reducing the number and size of debug artifacts written under `target/`.

**Architecture:** Keep the change local to Cargo profile configuration first, because the observed slow path was filesystem metadata deletion in `target/debug/deps`, not Rust compilation logic. Prefer reversible profile tuning over moving build output or disabling incremental compilation until measurements prove those stronger options are needed.

**Tech Stack:** Rust Cargo profiles, macOS/APFS, `cargo clean`, `cargo nextest`, pinned nightly formatting/lint commands.

---

## Background

On 2026-04-28, `cargo clean` in this repo spent several minutes deleting `target/debug/deps`. A process sample showed the active work was mostly `unlink(2)`, with smaller time in `lstat` and directory traversal. Activity Monitor reported very low write throughput because this workload is metadata-bound: many small file deletions and APFS journal updates, not large sequential writes.

The repo already has:

```toml
[profile.dev.package."*"]
debug = false # Disable debug info for all dependencies
opt-level = 1  # Shrinks monomorphized generics, reducing test binary size
```

That reduces dependency debug info, but workspace crates can still emit full debug info. The sampled clippy command included `-C debuginfo=2 -C split-debuginfo=unpacked` for a Fabro crate, which can create more debug artifacts and cleanup work on macOS.

## Proposed First Change

Add lower-debug Cargo profile settings for local dev and tests:

```toml
[profile.dev]
debug = "line-tables-only"
split-debuginfo = "off"

[profile.test]
debug = "line-tables-only"
split-debuginfo = "off"
```

Expected impact:

- Keeps source file and line information for backtraces.
- Usually keeps breakpoints and source stepping usable.
- Reduces or removes rich local-variable inspection in `lldb` and IDE debuggers.
- Reduces debug metadata and split-debug filesystem artifacts.
- Does not change debug assertions, overflow checks, runtime behavior, or optimization level.
- Causes one rebuild after the profile settings change.

## Task 1: Baseline Current Target Cost

**Files:**
- Read: `Cargo.toml`
- No code changes

- [ ] **Step 1: Confirm no `cargo clean` is running**

Run:

```bash
ps -axo pid,ppid,stat,etime,pcpu,command | rg -i 'cargo clean' || true
```

Expected: no active `cargo clean` process, or wait for the existing cleanup to finish.

- [ ] **Step 2: Build the workspace using current settings**

Run:

```bash
cargo build --workspace
```

Expected: build succeeds.

- [ ] **Step 3: Record current target size**

Run:

```bash
du -sh target target/debug target/debug/deps 2>/dev/null
```

Expected: record the three size values in this file under "Results".

- [ ] **Step 4: Record current entry pressure**

Run:

```bash
find target/debug/deps -maxdepth 1 -mindepth 1 | wc -l
```

Expected: record the entry count in this file under "Results".

## Task 2: Apply Lower-Debug Profile Settings

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add profile settings**

Edit `Cargo.toml` so the release profile section is followed by:

```toml
[profile.dev]
debug = "line-tables-only"
split-debuginfo = "off"

[profile.test]
debug = "line-tables-only"
split-debuginfo = "off"
```

Keep the existing dependency-specific dev profile below it:

```toml
[profile.dev.package."*"]
debug = false # Disable debug info for all dependencies
opt-level = 1  # Shrinks monomorphized generics, reducing test binary size
```

- [ ] **Step 2: Format if Cargo.toml style changes**

No formatter is required for this simple TOML change. Keep comments and spacing consistent with the surrounding file.

## Task 3: Verify Builds and Tests Still Work

**Files:**
- Read: `Cargo.toml`
- No additional code changes

- [ ] **Step 1: Build the workspace**

Run:

```bash
cargo build --workspace
```

Expected: build succeeds. The first build after changing profile settings may rebuild many crates.

- [ ] **Step 2: Run the core test suite**

Run:

```bash
cargo nextest run --workspace
```

Expected: tests pass. If macOS reports `Too many open files (os error 24)`, rerun with:

```bash
ulimit -n 4096 && cargo nextest run --workspace
```

- [ ] **Step 3: Run clippy with the pinned nightly toolchain**

Run:

```bash
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: clippy passes.

## Task 4: Measure Impact

**Files:**
- Read: `Cargo.toml`
- Update: this plan's "Results" section

- [ ] **Step 1: Record new target size**

Run:

```bash
du -sh target target/debug target/debug/deps 2>/dev/null
```

Expected: compare against Task 1.

- [ ] **Step 2: Record new entry pressure**

Run:

```bash
find target/debug/deps -maxdepth 1 -mindepth 1 | wc -l
```

Expected: compare against Task 1. The size reduction may be more meaningful than entry-count reduction, depending on how much split debug info was previously generated.

- [ ] **Step 3: Time a clean operation when convenient**

Run only when a full cleanup is acceptable:

```bash
time cargo clean
```

Expected: compare wall-clock time against the prior observed multi-minute cleanup. Activity Monitor may still show low MB/s because deletion remains metadata-bound.

## Task 5: Decide Whether to Stop or Tune Further

**Files:**
- Modify only if needed: `Cargo.toml`
- Optional local-only config: `.cargo/config.toml`

- [ ] **Step 1: Stop if the result is acceptable**

If `target/` size and cleanup time are acceptable, keep the lower-debug profile settings and do not add more tuning.

- [ ] **Step 2: Consider disabling incremental only if cleanup is still too costly**

Use this stronger setting only if the cleanup pain still outweighs slower edit-compile cycles:

```toml
[profile.dev]
debug = "line-tables-only"
split-debuginfo = "off"
incremental = false

[profile.test]
debug = "line-tables-only"
split-debuginfo = "off"
incremental = false
```

Expected tradeoff: fewer incremental artifacts, but slower repeated local builds.

- [ ] **Step 3: Consider a local target directory only if repo-local `target/` remains disruptive**

For a personal-only setup, use `.cargo/config.toml` if it is intentionally untracked, or a global Cargo config outside the repo:

```toml
[build]
target-dir = "/Users/bhelmkamp/.cache/cargo-targets/fabro"
```

Expected tradeoff: build output is isolated from the repo and easier to exclude from Spotlight/backups, but all developers do not automatically share the same behavior unless the config is committed.

## Rollback

To restore full debug info for normal dev/test builds, remove these settings:

```toml
[profile.dev]
debug = "line-tables-only"
split-debuginfo = "off"

[profile.test]
debug = "line-tables-only"
split-debuginfo = "off"
```

For a one-off debugger session without changing `Cargo.toml`, run:

```bash
CARGO_PROFILE_DEV_DEBUG=2 CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=unpacked cargo build
```

## Results

Fill this in when the plan is executed:

```text
Before:
- target size:
- target/debug size:
- target/debug/deps size:
- target/debug/deps entries:
- cargo clean wall-clock time:

After:
- target size:
- target/debug size:
- target/debug/deps size:
- target/debug/deps entries:
- cargo clean wall-clock time:
```
