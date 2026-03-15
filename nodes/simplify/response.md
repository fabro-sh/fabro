Clean diff. The `rewind.rs` change reverted back to the working literal string since the `concat!` macro approach was broken.

## Summary

**Fixed:**
1. **Compilation error in `rewind.rs`**: The `concat!("refs/heads/", crate::git::META_BRANCH_PREFIX)` doesn't compile because `concat!` only accepts literals, not `const` values. Reverted to the string literal `"refs/heads/fabro/meta/"` which is correct and works.

2. **Missing trailing newlines**: All three files (`rewind.rs`, `engine.rs`, `git.rs`) had their final newlines stripped. Restored them, then ran `cargo fmt` to ensure consistency.

**Confirmed clean:**
- The `MetadataStore::branch_name()` change to use `format!("{META_BRANCH_PREFIX}{run_id}")` is correct and uses the constant properly.
- The doc comment update in `engine.rs` is correct.
- No remaining hardcoded `refs/fabro/` strings exist in the codebase.
- All 179 tests pass, clippy clean with `-D warnings`, formatting clean.