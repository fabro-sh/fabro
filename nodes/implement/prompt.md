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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.