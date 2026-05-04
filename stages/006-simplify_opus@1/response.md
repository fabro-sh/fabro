All clean. Let me summarize the fixes made:

## Summary of Fixes

### Issues Fixed

1. **Redundant allocation in `outcome.rs`** (`fabro-workflow`): `billed_model_usage_from_llm` was creating two separate allocations for the same provider value — `ProviderId::from(provider)` and a separate `provider.to_string()`. Fixed by reusing `model.provider.as_str()` for the string reference and `model.provider` for the equality comparison, eliminating the redundant `provider_str` variable.

2. **Hacky fallback in `models.rs`** (`fabro-server`): The `test_model` handler was doing `Provider::from_str(info.provider.as_str()).unwrap_or(Provider::Anthropic)` — silently falling back to Anthropic for unknown providers, which is incorrect. Fixed by destructuring the `(provider_enum, issue)` tuple directly from the `auth_issues` find result and using `*provider_enum` (already the correct `Provider` enum value). Also replaced the roundabout `provider.to_string() == info.provider.as_str()` comparison with the direct `info.provider == <&str>::from(*provider)` which uses `ProviderId`'s `PartialEq<&str>` impl.

3. **Unnecessary `Provider` import in `models.rs`**: Removed the now-unused `Provider` import from the handler's import block.

4. **Unused `Provider` import in `server.rs`**: Removed the `Provider` import from the top-level `server.rs` module imports since it was no longer referenced by any handler.

5. **Unnecessary allocations via `provider.to_string()`**: Several call sites were converting `Provider` enum to `String` just to pass as `&str`, when `<&str>::from(provider)` (via `IntoStaticStr`) provides a `&'static str` without allocation:
   - `fabro-agent/src/cli.rs`: `&provider.to_string()` → `<&str>::from(provider)` 
   - `fabro-model/src/catalog.rs`: `provider.to_string().as_str()` → `<&str>::from(provider)` (two sites: `default_from_env` and `default_for_configured`)
   - `fabro-workflow/src/operations/start.rs`: `&provider.to_string()` → `<&str>::from(provider)`
   - `fabro-agent/tests/it/guardrails.rs`: Removed intermediate `let provider_str: &str = provider.into()` variable, inlining `<&str>::from(provider)` directly

6. **Redundant parse-then-stringify in `run_materialization.rs`**: Was parsing a `&str` into a `Provider` enum then immediately converting back to `String` via `.to_string()` to pass to `default_for_provider(&str)`. Fixed to validate the parse but pass the original `&str` directly, avoiding the round-trip allocation.

7. **Missing trailing newline in `fabro-model/Cargo.toml`**: The file was missing a final newline, causing a `\ No newline at end of file` in the diff.

8. **Formatting**: Ran `cargo fmt` to fix trailing newlines in several source files.