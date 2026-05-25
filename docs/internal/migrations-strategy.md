# Fabro Migrations Strategy

Fabro uses temporary migrations for compatibility rewrites of user-owned files and startup data. These are not SQL schema migrations. They exist so an upgraded binary can safely read data written by an older Fabro release, rewrite it into the current shape once, and then continue with the normal runtime path.

Migrations are product-facing compatibility code. Treat them like startup and storage code: conservative, idempotent, observable, and easy to remove after the compatibility window closes.

## Architecture

Each crate owns the migrations for the data it owns.

```text
lib/crates/<crate>/
  migrations/
    YYYYMMDDSS_descriptive_name.rs
  src/migrations.rs
```

The crate-local `src/migrations.rs` module is the registry. It imports numbered migration files with explicit `#[path = "../migrations/..."]` modules, orders them deliberately, and exposes the crate's migration entrypoint.

Examples:

- `fabro-config` owns settings-file migrations.
- `fabro-server` owns server startup migrations for `server.env` and vault files.

Keep migration APIs `pub(crate)` unless another crate genuinely orchestrates the migration.

## Naming And Metadata

Migration files use:

```text
YYYYMMDDSS_descriptive_name.rs
```

- `YYYYMMDD` is the date the migration is introduced.
- `SS` is a same-day sequence number starting at `01`.
- The descriptive name should say what is being rewritten, not only the old feature name.

Each migration should include:

- A module-level comment explaining why it exists and when to delete it.
- `REMOVAL_DEADLINE` when the migration is temporary compatibility code.
- A report type with at least enough fields to log changed/skipped counts and backup paths.

Do not bury migration ordering in filename globbing or directory iteration. The registry module should make ordering explicit.

## When To Add A Migration

Add a migration when all of these are true:

- A supported previous release may have written data in an old shape.
- The current release can infer the new shape without asking the operator.
- Failing immediately would create unnecessary upgrade breakage.
- The migration can be made idempotent and safe to retry.

Do not add a migration for:

- New defaults that normal config resolution can provide.
- Ambiguous rewrites where multiple new states are plausible.
- Data cleanup that can happen lazily in the normal write path.
- Permanent fallback behavior. If the old shape remains a supported input indefinitely, model that as normal parsing/resolution, not a temporary migration.

## Runner Design

The runner should make the migration boundary obvious.

For parse-recovery migrations, run only after normal parsing fails:

```rust
match content.parse::<SettingsLayer>() {
    Ok(layer) => layer,
    Err(err) => match migrations::run_migrations(path, &content)? {
        Some(report) => report.layer,
        None => return Err(Error::parse_file("Failed to parse settings file", path, err)),
    },
}
```

For startup storage migrations, run before the runtime component consumes the data:

```rust
let mut vault = load_startup_vault(vault_path)?;
let report = migrations::run_migrations(&mut vault, server_env_path, &env_entries)?;
```

Prefer one migration entrypoint per crate or subsystem. If the data has different phases, such as raw-file rewrites before `Vault::load` and loaded-vault rewrites after it, name those phases explicitly instead of hiding them behind a broad helper.

## Idempotence And State

File migrations should be state-driven. Fabro does not keep an applied-migrations ledger for these compatibility rewrites.

This is intentional:

- Operators can restore or edit files manually.
- Startup may be interrupted after one file changes and before another does.
- Re-running should converge on the same current state.

Every migration must handle:

- Missing files as no-op unless the owning API already treats them as errors.
- Already-migrated files as no-op.
- Partially migrated state as either a safe no-op or a clear error.
- Existing current-shape values as authoritative.

Never overwrite a current-shape value with a legacy value. For secrets, the vault wins over process env and `server.env`.

## File Safety

Before rewriting an existing user-owned file:

1. Parse and validate the full target state in memory.
2. Write a backup beside the original file.
3. Preserve private permissions for secret-bearing files.
4. Write the replacement atomically when the local helper supports it.

Use structured parsers and local helpers rather than ad hoc string rewriting:

- TOML settings: `toml_edit` so comments and unrelated fields survive where practical.
- `server.env`: `fabro_config::envfile`.
- Vault JSON: serde JSON values or the vault API, depending on whether the legacy shape can be loaded.

If a migration removes entries from a file after writing another store, write the destination first, then back up and rewrite the source. Document this order in tests.

## Errors And Recovery

Choose the error policy deliberately.

Use warn-and-continue only when the normal path may still succeed and compatibility is best-effort. The legacy vault-entry migration does this because an unreadable legacy shape should not block loading an otherwise usable vault file.

Return an error when the migration found data it must move or rewrite and cannot do so safely. This gives operators a precise migration failure instead of a later, misleading startup error.

Error messages should name:

- The operation.
- The affected path.
- The key or setting name when useful.

They must not include secret values or full file contents.

## Observability

Migration logging is for operators and developers diagnosing upgrade behavior.

Use structured tracing fields:

```rust
warn!(
    migrated_entries = report.migrated_entries,
    skipped_entries = report.skipped_entries,
    backup_path = %backup_path,
    removal_deadline = migrations::REMOVAL_DEADLINE,
    "Migrated legacy settings file"
);
```

Safe fields:

- Migration name or deadline.
- Changed, migrated, removed, skipped, and preserved counts.
- Backup path.
- Secret or setting key names when an operator must act.
- Error value.

Forbidden fields:

- Secret values.
- Raw file contents.
- Serialized before/after data.

If logging is not configured yet and the operator needs to see the message, emit the same concise warning to stderr. Keep this rare and scoped to startup/config loading.

Do not emit workflow run events for process startup migrations. Events are for workflow run state; migrations are startup/storage diagnostics.

## Secrets

Secret migrations must follow `docs/internal/server-secrets-strategy.md`.

Rules:

- Classify secret names through `fabro-static`, not local string lists.
- Existing vault values are authoritative.
- Process env may be copied into the vault, but it cannot be cleaned up by Fabro.
- `server.env` entries may be removed only after the vault contains the intended value and a backup has been written.
- Conflicts should preserve both values and warn by key name only.
- Do not add runtime env fallback paths for optional integration secrets.

## Tests

Migration tests should cover behavior, not implementation details.

Required scenarios for most migrations:

- No-op when the old shape is absent.
- Successful rewrite creates a backup and produces the current shape.
- Running the migration a second time is a no-op.
- Existing current-shape values are not overwritten.
- Ambiguous or unsupported old shapes return a clear error.
- Write failures leave either the original file unchanged or a backup sufficient for recovery.

Additional scenarios for secret migrations:

- Source precedence.
- Existing vault value wins.
- Matching legacy source entry is cleaned up.
- Conflicting legacy source entry is preserved and warned.
- Secret type is preserved or assigned correctly.
- Logs and errors do not contain secret values.

Keep tests next to the migration module when they exercise pure rewrite behavior. Put tests on the startup path when the important contract is orchestration order or integration with validation.

## Removing A Migration

Temporary migrations should not become permanent parsing policy.

When the removal deadline passes:

1. Confirm supported upgrade windows no longer need the migration.
2. Remove the migration file and its registry entry.
3. Remove tests that only cover the legacy shape.
4. Remove user-facing compatibility docs.
5. Keep current-shape tests that still protect normal behavior.

If the old shape must remain supported after the deadline, move it into normal parsing/resolution and update this strategy doc's assumptions in the same change.

## Checklist

Before merging a new migration:

- The owning crate has a `migrations/` file with a dated sequence name.
- The crate registry orders the migration explicitly.
- The migration is idempotent.
- Existing current-shape data wins over legacy data.
- Backups are written before mutating existing user-owned files.
- Secret-bearing files keep private permissions.
- Logs include counts and backup paths, never secret values or raw file contents.
- Tests cover no-op, success, conflict/unsupported input, and retry behavior.
- The migration has a removal deadline or a written reason why it is permanent.
