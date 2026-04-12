/// Abstraction over environment variable lookup.
///
/// Production code uses [`SystemEnv`] which delegates to [`std::env::var`].
/// Tests inject a [`TestEnv`] backed by a `HashMap` so they never mutate
/// process-global state.
pub trait Env: Send + Sync {
    fn var(&self, key: &str) -> Result<String, std::env::VarError>;
}

/// Reads real process environment variables.
#[derive(Clone, Debug)]
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        std::env::var(key)
    }
}

/// In-memory environment double — no process-global mutation.
///
/// Intended for use in tests across the workspace. Unconditionally compiled
/// because it is trivial and has no external dependencies.
#[derive(Clone, Debug)]
pub struct TestEnv(pub std::collections::HashMap<String, String>);

impl Env for TestEnv {
    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        self.0
            .get(key)
            .cloned()
            .ok_or(std::env::VarError::NotPresent)
    }
}
