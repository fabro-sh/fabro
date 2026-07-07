# Panics, unwrap, expect, and assertions

## Rule

Return `Result` for recoverable failures; panic only for violated invariants or impossible states, and prefer `expect` with an invariant-focused message over bare `unwrap`.

## Why

Panics unwind by default; a panicking Tokio task surfaces as a `JoinError` at the join point, and under `panic = "abort"` the process dies. Either way, panics give callers no structured recovery path for expected failures, so they are appropriate when the program has reached a state that means the code is wrong, not when input, I/O, network, parsing, or configuration can fail normally.

## Do

- Return `Result` for user input, file I/O, network calls, parsing, validation, configuration, and external service failures.
- Use `expect` when failure would prove a hard-coded constant, static fixture, or internal invariant is wrong.
- Write `expect` messages that state the invariant, such as `DEFAULT_PORT should be a valid u16`.
- Use `assert!`, `assert_eq!`, and `assert_ne!` for tests and internal invariants.
- Use `debug_assert!` only for checks that are helpful in debug builds but not required for release correctness.
- Use `unreachable!` only after the code has already ruled out the state by construction.
- Add `# Panics` rustdoc when a public function can panic.
- In tests, prefer `expect` when the setup failure message will help diagnose the failed test.

## Avoid

- Do not use `unwrap` or `expect` for recoverable runtime failures.
- Do not use bare `unwrap` outside tests; the workspace denies `clippy::unwrap_used` (with `allow-unwrap-in-tests`), so use `expect` with an invariant message in production code.
- Do not use panics for normal validation failures.
- Do not write `expect("should work")`, `expect("failed")`, or messages that just repeat the error.
- Do not use `unreachable!` for states reachable from external input.
- Do not rely on `debug_assert!` for memory safety, security, validation, or release behavior.
- Do not leave `todo!()` or `unimplemented!()` in committed production paths.
- Do not hide fallible startup work behind panics when a clean diagnostic can be returned.

## Library vs Application

Libraries should be strict: return errors for caller-controlled failures and document any public panic behavior. Applications may fail fast during startup for violated build-time or configuration invariants, but ordinary operator mistakes should still become clean errors.

## Example

Use `Result` for runtime input:

```rust
pub fn parse_port(raw: &str) -> Result<u16, std::num::ParseIntError> {
    raw.parse()
}
```

Bad: panic on operator input.

```rust
let port = std::env::var("PORT").unwrap().parse::<u16>().unwrap();
```

Good: return a diagnostic path.

```rust
use anyhow::Context;

let port = std::env::var("PORT")
    .context("PORT is required")?
    .parse::<u16>()
    .context("PORT should be a valid u16")?;
```

Use `expect` when a checked-in invariant is wrong:

```rust
const DEFAULT_PORT: &str = "8080";

pub fn default_port() -> u16 {
    DEFAULT_PORT
        .parse()
        .expect("DEFAULT_PORT should be a valid u16")
}
```

Use assertions for internal invariants:

```rust
fn split_parsed_record(fields: &[String]) -> (&str, &str) {
    assert!(
        fields.len() == 2,
        "record parser should produce exactly two fields"
    );

    (fields[0].as_str(), fields[1].as_str())
}
```

## Exceptions

- Use `unwrap` in short tests when the failure location is obvious and `expect` would add noise.
- Use panics in examples or prototypes only when the surrounding context is intentionally disposable.
- Use `panic!` for impossible internal states when returning an error would imply callers can recover.
