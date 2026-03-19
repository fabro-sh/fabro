All tests pass, clippy is clean, formatting is correct.

## Summary

**Reviewed findings from three parallel agents (Code Reuse, Code Quality, Efficiency).**

**One fix applied:** Extracted a `canonical_provider()` helper in `catalog.rs` and applied it consistently to `list_models()` and `closest_model()` — both were missing the alias normalization that was added to `default_model_for_provider()`. This meant `default_model_for_provider("open_ai")` worked correctly but `list_models(Some("open_ai"))` returned empty results. Now all three provider-filtering functions in the catalog normalize aliases consistently.

**Findings skipped (not worth fixing):**
- Double `Provider::default_from_env()` call in `run_from_branch` — functionally correct since env vars are stable; the two calls happen only when both CLI args are absent
- Redundant parse roundtrip when callers already hold `Provider` — benign overhead on a non-hot path
- Stringly-typed provider in `resolve_model_provider` — pre-existing design issue, not introduced by this diff
- Repeated `provider.and_then(default_model_for_provider).unwrap_or_else(default_model_from_env).id` pattern — only 3 occurrences with slightly different shapes; not worth a helper yet