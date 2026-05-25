# Startup Migrations

Fabro keeps temporary compatibility migrations close to the crate that owns the data being rewritten.

## Layout

- Put migration implementations in a crate-local `migrations/` directory.
- Name files `YYYYMMDDSS_descriptive_name.rs`, where `SS` is a same-day sequence.
- Add a crate-local `src/migrations.rs` registry that imports migration files with explicit `#[path = "../migrations/..."]` modules and exposes a single `run_migrations(...)` entrypoint for that crate.
- Keep migration APIs `pub(crate)` unless another crate is the real owner of orchestration.

## Behavior

- Prefer idempotent, state-driven migrations over an applied-migrations ledger for file rewrites.
- Always write a backup before mutating an existing user-owned file.
- Include migration metadata in code: `ID` or filename, short name, and removal deadline/comment.
- Do not log secret values or copied file contents. Log counts, key names when needed for operator action, backup paths, and removal deadlines.
- Preserve the old behavior unless the migration must fail to avoid a worse startup error.

## Tests

- Keep behavior tests next to the migration module or the startup path that invokes it.
- Test the no-op path, backup/rewrite path, conflict path, and idempotent second-run behavior when applicable.
- For migrations that move secrets, assert source precedence and that existing vault values are not overwritten.
