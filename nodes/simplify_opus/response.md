All 390 tests pass, 0 failures.

## Summary

**Review findings:**
- **Code Reuse**: Clean — the one-line `json!({"effort": effort})` duplication between Anthropic and OpenAI providers is coincidental API symmetry, not shared domain logic. Not worth extracting.
- **Efficiency**: Clean — the `.as_ref().map()` short-circuits to `None` in the common case, negligible next to HTTP I/O.
- **Code Quality**: One actionable finding — copy-paste `Request` construction in tests.

**Fix applied:** Introduced a `make_base_request()` test helper and refactored 3 tests + `make_request_with_format` to use it via struct update syntax (`..make_base_request()`), eliminating ~40 lines of duplicated test boilerplate. The tests now clearly express only what differs from the default (e.g., `reasoning_effort: Some("medium".to_string())` or custom `messages`).