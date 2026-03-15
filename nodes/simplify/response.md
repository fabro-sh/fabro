Everything passes. Here's a summary of what was fixed:

### Fixes Applied

1. **Restored trailing newlines** in 7 files that had their final `\n` stripped during the rename:
   - `apps/fabro-web/app/layouts/app-shell.tsx`
   - `apps/fabro-web/app/lib/config.server.ts`
   - `apps/fabro-web/app/routes/start.tsx`
   - `docker/demo-server.toml`
   - `lib/crates/fabro-api/src/demo/mod.rs`
   - `lib/crates/fabro-api/tests/openapi_conformance.rs`
   - `lib/crates/fabro-config/src/server.rs`

2. **Renamed stale test function names** in `lib/crates/fabro-config/src/server.rs`:
   - `parse_feature_flags` → `parse_features`
   - `parse_feature_flags_defaults` → `parse_features_defaults`

No code reuse or efficiency issues were found — the original implementation was a clean mechanical rename.