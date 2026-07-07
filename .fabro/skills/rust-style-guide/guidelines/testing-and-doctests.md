# Testing and Doctests

## Rule

Use balanced behavior-focused testing: put unit tests near focused logic, integration tests around public behavior and workflows, and skip doctests by default.

## Why

Unit tests give fast feedback around dense logic and invariants. Integration tests protect the behavior callers actually depend on. Doctests add maintenance cost and should not become default coverage just because a public item has documentation.

## Do

- Test behavior, invariants, and observable state changes instead of private implementation steps.
- For each nontrivial source file, default to a bottom-of-file `#[cfg(test)] mod tests` covering that file's behavior and private helpers. Integration tests complement these module tests; they do not replace them.
- Put unit tests in the same module or a nearby test module when they exercise focused domain logic, parsing, validation, or small transformations.
- Put integration tests under `tests/` when they exercise public APIs, CLI behavior, cross-crate behavior, I/O boundaries, or multi-step workflows.
- Use module-private tests when they make hard-to-reach invariants clear; prefer public behavior when practical.
- Name tests as behavior descriptions, such as `rejects_zero_limit` or `loads_profile_from_env_override`.
- Use fallible tests returning `Result<(), Error>` when setup or assertions naturally use `?`.
- Keep setup helpers small, explicit, and named after domain concepts.
- Prefer real values and temp files or directories where practical; use fakes or mocks only at external, slow, or nondeterministic boundaries.
- For reusable libraries, expose narrow seams for file, network, time, randomness, subprocess, or OS behavior when edge cases must be tested.
- Put regression tests at the level where the bug was observable.
- Keep assertions specific about behavior, errors, and state changes.

## Avoid

- Do not add doctests by default.
- Do not use rustdoc examples as a substitute for normal tests.
- Do not test every private helper through brittle implementation details.
- Do not write tests that only mirror the implementation.
- Do not use bare `unwrap` in tests when `?` or `expect` would make failures clearer.
- Do not add sleeps or timing-dependent tests; use controlled clocks, explicit events, or boundary timeouts.
- Do not assert only that code "does not panic" when behavior can be checked.
- Do not introduce broad test-only public APIs.
- Do not make helpers `pub` only so integration tests can reach them; use module-local tests or expose a real domain API.
- Do not hide test-only controls in normal library APIs; gate them behind `cfg(test)` or a deliberate `test-util` feature.
- Do not skip meaningful integration coverage just because unit tests pass.

## Example

Keep unit tests close to focused logic:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limit(u32);

impl Limit {
    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitError {
    Invalid,
    Zero,
}

pub fn parse_limit(value: &str) -> Result<Limit, LimitError> {
    let value = value.parse().map_err(|_| LimitError::Invalid)?;

    if value == 0 {
        return Err(LimitError::Zero);
    }

    Ok(Limit(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_limit() {
        let error = parse_limit("0").expect_err("zero limit should be rejected");
        assert_eq!(error, LimitError::Zero);
    }

    #[test]
    fn parses_positive_limit() -> Result<(), LimitError> {
        let limit = parse_limit("25")?;
        assert_eq!(limit.get(), 25);
        Ok(())
    }
}
```

Use integration tests for public workflows:

```rust
#[test]
fn creates_user_workflow() -> anyhow::Result<()> {
    let app = TestApp::start()?;

    let response = app.create_user("ada@example.com")?;

    assert_eq!(response.status(), 201);
    assert!(app.user_exists("ada@example.com")?);
    Ok(())
}
```

## Exceptions

- Add doctests only when a project explicitly opts into maintaining public rustdoc examples.
- Use `no_run` or `ignore` for rustdoc examples only when the documentation page's rules apply.
- Use module-private tests for parsers, validators, state machines, or algorithmic code with dense edge cases.
- Use `#[cfg(test)]` helpers when they keep production APIs clean and do not hide the behavior under test.
