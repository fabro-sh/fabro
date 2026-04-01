Reviewed and cleaned up the `fabro doctor` storage-dir work.

What I fixed:
- Reused `load_user_settings_with_globals(globals)` so doctor uses the same effective settings resolution as other commands.
- Split storage-dir handling into:
  - `probe_storage_dir(&Path)` for filesystem probing
  - `check_storage_dir(&StorageDirStatus)` for pure result rendering
- Made the storage-dir check always report all 3 detail lines:
  - `Exists: ...`
  - `Readable: ...`
  - `Writable: ...`
- Added the missing unit coverage:
  - existing dir probe
  - missing dir probe
  - pass
  - missing dir error
  - not writable error
- Added the missing focused integration test:
  - `storage_dir_shown_in_output`
- Kept existing snapshots green.

Validation:
- `cargo nextest run -p fabro-cli -- doctor` ✅
- `cargo clippy --workspace -- -D warnings` ✅

No further issues stood out after cleanup.