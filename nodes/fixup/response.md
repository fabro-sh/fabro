This is a pre-existing test failure unrelated to our changes. The fix I made was simply adding `use std::sync::Arc;` to the `mtls_e2e` module in `lib/crates/fabro-server/tests/it/api.rs` to resolve the compilation error from the verify step.

To summarize the fix:
- **File:** `lib/crates/fabro-server/tests/it/api.rs`
- **Change:** Added `use std::sync::Arc;` import to the `mtls_e2e` module (line 17), which was missing but needed for `Arc::new()` calls on lines 389-390.
- **Result:** `cargo clippy --workspace -- -D warnings` passes cleanly, and all doctor tests (28/28) pass.