Implemented a focused fix in `lib/crates/fabro-agent/src/local_sandbox.rs`.

What changed:
- Added a Unix-only helper to normalize newly created local sandbox files to `0o644`.
- Applied it after:
  - `write_file()`
  - `upload_file_from_local()`
  - `download_file_to_local()`

Why:
- `LocalSandbox::upload_file_from_local()` previously used `tokio::fs::copy()`, which could preserve a source file’s restrictive mode like `0o600`.
- That made file permissions inconsistent compared with other sandbox implementations that already default to `0o644`.

Tests added:
- `write_file_sets_default_permissions`
- `upload_file_from_local_sets_default_permissions`
- `download_file_to_local_sets_default_permissions`

Validation:
- `git diff --check` passed.
- I couldn’t run `cargo test` because this environment doesn’t have the Rust toolchain installed (`cargo: not found`).