All tests pass and clippy is clean.

## Summary

**The code is already clean — no changes needed.**

All three review agents found that the implementation is well-structured:

- **Code Reuse**: The extraction is well-scoped. Shared functions moved to `provider_auth.rs`, install-only helpers (`prompt_select`, `prompt_multiselect`) correctly left in `install.rs`. All flagged patterns (repeated `home_dir().join(".fabro")`, `Styles::detect_stderr()`) are pre-existing across the codebase and not introduced by this change.

- **Code Quality**: The most notable finding (`set_var` in `validate_api_key`) is pre-existing behavior that was simply moved from `install.rs` — not introduced by this PR. The `run_openai_oauth_or_api_key` abstraction boundary is reasonable: the caller decides "try OAuth at all?" while the function handles "OAuth failed, fall back." No parameter sprawl, no stringly-typed code, no copy-paste issues.

- **Efficiency**: No unnecessary work, no hot-path bloat, no memory issues. The `Client::from_env()` initializing all providers is the standard pattern used throughout the codebase and acceptable for an interactive CLI command.