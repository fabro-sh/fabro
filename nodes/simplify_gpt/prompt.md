Goal: # Plan: JSONL analytics event file format

## Context

Currently each CLI invocation writes a single `Track` event as a standalone JSON file (`~/.fabro/tmp/fabro-event-{uuid}.json`) and spawns a detached subprocess to send it. We want to switch to JSONL format (one JSON event per line) so a single file can contain multiple events. Filenames keep a UUID for uniqueness. This enables callers to batch multiple events into one file/subprocess.

The panic sender (`__send_panic`) is unaffected — it stays single-JSON-per-file.

## Changes

### 1. `lib/crates/fabro-util/src/telemetry/sender.rs` — rewrite

**Writer — rename `send()` to `emit()`, accept multiple events:**
- `pub fn emit(tracks: &[Track])` (was `pub fn send(track: Track)`)
- Early return if `SEGMENT_WRITE_KEY` is `None` or `tracks` is empty
- Generate a UUID for the filename: `fabro-events-{uuid}.jsonl`
- Serialize each `Track` as a compact JSON line (`serde_json::to_string`), join with `\n`
- Pass the bytes to `spawn_fabro_subcommand("__send_analytics", &filename, &json)` as before

No file locking needed — each invocation writes its own uniquely-named file.

**Reader — rename `send_to_segment()` to `upload()`:**
- Read file contents as string
- Parse each non-empty line as `serde_json::Value`, inject `"type": "track"`, collect into batch array
- Skip malformed lines with `tracing::warn!`
- If no valid events, return `Ok(())`
- POST to `https://api.segment.io/v1/batch` with payload `{"batch": [...]}`
- Keep Basic auth the same

Extract a pure `fn build_segment_batch(content: &str) -> Option<Value>` for testability.

**Constants:**
- Change `SEGMENT_API_URL` from `.../v1/track` to `.../v1/batch`

### 2. `lib/crates/fabro-cli/src/main.rs`

**`send_telemetry_event()` (~line 428):** Change call from `sender::send(track)` to `sender::emit(&[track])`.

**`SendAnalytics` handler (~line 910):** Change call from `sender::send_to_segment(&path)` to `sender::upload(&path)`.

### 3. `lib/crates/fabro-util/src/telemetry/spawn.rs` — no changes

`spawn_fabro_subcommand` is generic (takes raw bytes). It continues to work for both JSONL analytics files and single-JSON panic files.

### 4. No changes to these files
- `event.rs` — `Track` struct unchanged
- `panic.rs` — stays single-JSON-per-file
- `mod.rs`, `anonymous_id.rs`, `context.rs`, `git.rs`, `sanitize.rs` — unchanged

## Implementation order (red/green TDD)

Write each test first (red), then implement just enough to make it pass (green).

### Step 1: `build_segment_batch` — pure function, no I/O

1. **Red:** Write test `build_segment_batch_empty_content` — empty string returns `None`
2. **Green:** Add `fn build_segment_batch(content: &str) -> Option<Value>` stub returning `None`
3. **Red:** Write test `build_segment_batch_single_event` — one JSONL line produces `{"batch": [{"type": "track", ...}]}`
4. **Green:** Implement line parsing, `"type": "track"` injection, batch wrapping
5. **Red:** Write test `build_segment_batch_multiple_events` — two lines produce batch of 2
6. **Green:** Should already pass
7. **Red:** Write test `build_segment_batch_skips_malformed_lines` — one good + one bad line produces batch of 1
8. **Green:** Add `continue` on parse error

### Step 2: `emit()` — writer side

9. **Red:** Update existing `send_noops_without_write_key` to use `emit(&[track])` signature
10. **Green:** Rename `send` to `emit`, change signature to `&[Track]`, serialize as JSONL (one JSON line per track, joined with `\n`), generate `fabro-events-{uuid}.jsonl` filename

### Step 3: `upload()` — reader side

11. **Red:** Write test `upload_noops_without_write_key` — same pattern as existing `send_panic_noops_without_dsn`
12. **Green:** Rename `send_to_segment` to `upload`, change internals to read file as string, call `build_segment_batch`, POST to `/v1/batch`

### Step 4: Wire up call sites in `main.rs`

13. Update `send_telemetry_event()` to call `sender::emit(&[track])`
14. Update `SendAnalytics` handler to call `sender::upload(&path)`

### Step 5: Final checks

```bash
cargo fmt --check --all
cargo clippy --workspace -- -D warnings
cargo test -p fabro-util
cargo test --workspace
```


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: success
  - Model: claude-opus-4-6, 38.6k tokens in / 4.6k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/sender.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 21.3k tokens in / 7.9k out
  - Files: /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/sender.rs
- **simplify_gemini**: success
  - Model: claude-opus-4-6, 26.4k tokens in / 10.3k out


# Simplify: Code Review and Cleanup

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run git diff (or git diff HEAD if there are staged changes) to see what changed. If there are no git changes, review the most recently modified files that the user mentioned or that you edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message. Pass each agent the full diff so it has the complete context.

### Agent 1: Code Reuse Review

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use Grep to find similar patterns elsewhere in the codebase — common locations are utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality. Suggest the existing function to use instead.
3. Flag any inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar patterns are common candidates.

Note: This is a greenfield app, so focus on maximizing simplicity and don't worry about changing things to achieve it.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. Parameter sprawl: adding new parameters to a function instead of generalizing or restructuring existing ones
3. Copy-paste with slight variation: near-duplicate code blocks that should be unified with a shared abstraction
4. Leaky abstractions: exposing internal details that should be encapsulated, or breaking existing abstraction boundaries
5. Stringly-typed code: using raw strings where constants, enums (string unions), or branded types already exist in the codebase

Note: This is a greenfield app, so be aggressive in optimizing quality.

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns
2. Missed concurrency: independent operations run sequentially when they could run in parallel
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths
4. Unnecessary existence checks: pre-checking file/resource existence before operating (TOCTOU anti-pattern) — operate directly and handle the error
5. Memory: unbounded data structures, missing cleanup, event listener leaks
6. Overly broad operations: reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate their findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue with the finding, just skip it.

When done, briefly summarize what was fixed (or confirm the code was already clean).