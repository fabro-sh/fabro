# Constructors and Builders

## Rule

Use `new` and `try_new` for required fields, add builders when optional configuration makes call sites clearer, and reserve typestate builders for important invariants.

## Why

Simple constructors keep invariants close to the type. Builders are useful when names and defaults matter, but they add API surface. Typestate can prevent invalid states at compile time, but it is too much machinery for ordinary configuration.

## Do

- Use `new` for infallible construction from required values.
- Use `try_new` when construction validates caller input or can fail; reserve `parse` for `FromStr`-backed textual parsing.
- Keep validation inside the constructor or `build` method.
- Use `Default` only when there is an obvious, useful default value.
- Use a builder when a type has several optional fields, many defaults, or call sites would otherwise pass booleans and `None` values.
- Prefer consuming builder setters like `fn timeout(mut self, value: Duration) -> Self` for owned configuration builders.
- Use `with_*` for derived variants or optional modifications, not as a substitute for a clear primary constructor.
- Use typestate builders only when the compile-time ordering protects an important invariant or prevents a dangerous operation; for workflow state machines, follow [typestate and state machines](typestate-and-state-machines.md).

## Avoid

- Do not add a builder for every struct by habit.
- Do not make fields public just to avoid writing a constructor.
- Do not write a `new` function that panics or unwraps on caller-provided input.
- Do not use long constructors with boolean flags or repeated `None` arguments.
- Do not encode ordinary optional configuration with typestate.
- Do not use `Default` when the value would be surprising, invalid, or environment-dependent.

## Public API Notes

For public libraries, constructors and builders are part of the stable API. If a type is likely to gain optional settings over time, prefer a builder before adding many constructor parameters.

Adding a required constructor parameter is usually a breaking change. Adding an optional builder method is usually easier to evolve.

## Example

```rust
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    max_attempts: u32,
    backoff:      Duration,
}

impl RetryPolicy {
    pub fn try_new(max_attempts: u32, backoff: Duration) -> Result<Self, RetryPolicyError> {
        if max_attempts == 0 {
            return Err(RetryPolicyError::NoAttempts);
        }

        Ok(Self {
            max_attempts,
            backoff,
        })
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn backoff(&self) -> Duration {
        self.backoff
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryPolicyError {
    NoAttempts,
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    timeout:      Duration,
    retry_policy: RetryPolicy,
    user_agent:   Option<String>,
}

impl ClientOptions {
    pub fn new(timeout: Duration, retry_policy: RetryPolicy) -> Self {
        Self {
            timeout,
            retry_policy,
            user_agent: None,
        }
    }

    pub fn builder() -> ClientOptionsBuilder {
        ClientOptionsBuilder::default()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Clone, Debug)]
#[must_use]
pub struct ClientOptionsBuilder {
    timeout:      Duration,
    retry_policy: RetryPolicy,
    user_agent:   Option<String>,
}

impl Default for ClientOptionsBuilder {
    fn default() -> Self {
        Self {
            timeout:      Duration::from_secs(30),
            retry_policy: RetryPolicy::try_new(3, Duration::from_millis(200))
                .expect("default retry policy is valid"),
            user_agent:   None,
        }
    }
}

impl ClientOptionsBuilder {
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    pub fn build(self) -> ClientOptions {
        ClientOptions {
            timeout:      self.timeout,
            retry_policy: self.retry_policy,
            user_agent:   self.user_agent,
        }
    }
}
```

## Exceptions

- Use public fields and struct literals for plain data types with no invariants.
- Use `&mut self` builder methods when matching an existing API style or when callers need to reuse the builder.
- Use generated builder crates only when the project already depends on them or has enough builder-heavy types to justify the dependency.
- Use typestate for important protocols, state machines, or safety boundaries where invalid ordering should not compile.
