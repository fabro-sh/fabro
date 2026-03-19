Here's a summary of all changes made:

### `lib/crates/fabro-util/src/telemetry/sender.rs` — rewritten

**Writer side:**
- Renamed `send(track: Track)` → `emit(tracks: &[Track])` — accepts a slice of events
- Early returns if `SEGMENT_WRITE_KEY` is `None` or `tracks` is empty
- `spawn_sender` serializes each `Track` as a compact JSON line, joins with `\n`, generates a `fabro-events-{uuid}.jsonl` filename

**Reader side:**
- Renamed `send_to_segment()` → `upload()` — reads file as string, delegates to `build_segment_batch`, POSTs to `/v1/batch`
- New pure function `build_segment_batch(content: &str) -> Option<Value>` — parses each non-empty JSONL line, injects `"type": "track"`, wraps in `{"batch": [...]}`, skips malformed lines with `tracing::warn!`

**Constants:**
- `SEGMENT_API_URL` changed from `.../v1/track` to `.../v1/batch`
- Added `use uuid::Uuid` (removed unused `Track.message_id` dependency for filename)

**Tests (9 total, all passing):**
1. `build_segment_batch_empty_content` — empty string returns `None`
2. `build_segment_batch_single_event` — one line produces batch with `"type": "track"`
3. `build_segment_batch_multiple_events` — two lines produce batch of 2
4. `build_segment_batch_skips_malformed_lines` — bad JSON lines are skipped
5. `build_segment_batch_all_malformed` — all bad lines returns `None`
6. `build_segment_batch_skips_blank_lines` — blank lines are ignored
7. `emit_noops_without_write_key` — no-ops without compile-time key
8. `emit_noops_with_empty_tracks` — no-ops on empty slice
9. `upload_noops_without_write_key` — errors without compile-time key

### `lib/crates/fabro-cli/src/main.rs` — two call sites updated

- Line 429: `sender::send(track)` → `sender::emit(&[track])`
- Line 911: `sender::send_to_segment(&path)` → `sender::upload(&path)`

### Unchanged files (as planned)
- `event.rs`, `panic.rs`, `mod.rs`, `spawn.rs`, `anonymous_id.rs`, `context.rs`, `git.rs`, `sanitize.rs` — no changes needed