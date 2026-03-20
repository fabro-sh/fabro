All clean. Here's a summary of what was fixed:

### Fixes applied

1. **Simplified `build_json_output`** (Code Reuse): Replaced verbose hand-rolled `serde_json::Map::new()` with `serde_json::json!{}` macro, which is already used elsewhere in the codebase for known shapes. Reduces the function from 20 lines to 11.

2. **Replaced dead `_ => "Unknown"` branch with `unreachable!()`** (Code Quality): The poll loop only breaks when `is_terminal()` returns true, which is exactly `Succeeded | Failed | Dead`. The wildcard arm was dead code masquerading as a fallback. `unreachable!()` makes the invariant explicit.

3. **Capped sleep duration at remaining deadline** (Efficiency): Previously, `thread::sleep(interval)` could sleep well past the deadline (e.g., `--timeout 5 --interval 10000` would sleep 10s). Now the sleep is `interval.min(dl - now)`, so it wakes up promptly when the deadline arrives.

4. **Used `to_writer_pretty` instead of `to_string_pretty`** (Efficiency): Avoids allocating an intermediate `String` by writing JSON directly to the locked stdout handle.