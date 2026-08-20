//! Retry for the first repository clone in a clone-based sandbox.
//!
//! Clone-based providers can mint a GitHub App installation token and clone
//! with it immediately. GitHub can reject that first clone before the token is
//! available to the git endpoint. On a private repository, the rejection can
//! arrive as `Repository not found.` or an authentication failure.
//!
//! Only a token minted during the current clone operation makes those messages
//! safe to retry. Static PATs and pre-minted installation tokens fail fast.
//!
//! Retries reuse the same token on purpose. Replication of a given token only
//! makes progress, so each attempt strictly improves the odds, while re-minting
//! would restart the replication clock.

use std::future::Future;
use std::time::Duration;

use fabro_types::SandboxProviderKind;
use fabro_util::backoff::BackoffPolicy;
use tokio::time;

/// Total clone attempts, including the first.
const MAX_ATTEMPTS: u32 = 3;

/// Why a failed clone attempt is worth repeating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum CloneRetryReason {
    /// A freshly minted installation token has not reached the GitHub edge
    /// cache site serving this clone yet.
    TokenReplication,
    /// The clone failed on infrastructure, unrelated to credentials.
    TransientInfra,
}

/// What a clone failure message tells us about retry safety.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloneMessageClass {
    Retry(CloneRetryReason),
    Permanent,
    Unknown,
}

impl CloneMessageClass {
    pub(crate) fn retry_reason(self) -> Option<CloneRetryReason> {
        match self {
            Self::Retry(reason) => Some(reason),
            Self::Permanent | Self::Unknown => None,
        }
    }
}

/// Message fragments that mean the clone failed on infrastructure.
///
/// These are safe to retry whether or not the clone was authenticated.
const TRANSIENT_HINTS: &[&str] = &[
    "could not resolve host",
    "temporary failure in name resolution",
    "connection refused",
    "connection reset",
    "connection timed out",
    "timed out",
    "network is unreachable",
    "no route to host",
    "tls handshake",
    "early eof",
    "rpc failed",
    "unexpected disconnect",
    "the remote end hung up unexpectedly",
    "index-pack failed",
    "service unavailable",
    "gateway timeout",
    "too many requests",
    "rate limit",
];

/// Message fragments GitHub uses when a token is not yet visible.
///
/// Only meaningful when the clone carried credentials. The same lag surfaces as
/// 404 or as an auth failure depending on which endpoint answers first.
const TOKEN_REPLICATION_HINTS: &[&str] = &[
    "repository not found",
    "authentication failed",
    "invalid username or password",
    "bad credentials",
];

/// Classify a failed clone by its rendered message.
///
/// `token_was_freshly_minted` gates the token-replication reading. A static
/// credential cannot become valid during backoff, so auth failures for it are
/// permanent.
pub(crate) fn classify_message(message: &str, token_was_freshly_minted: bool) -> CloneMessageClass {
    let lower = message.to_ascii_lowercase();

    if TRANSIENT_HINTS.iter().any(|hint| lower.contains(hint)) {
        return CloneMessageClass::Retry(CloneRetryReason::TransientInfra);
    }
    if TOKEN_REPLICATION_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return if token_was_freshly_minted {
            CloneMessageClass::Retry(CloneRetryReason::TokenReplication)
        } else {
            CloneMessageClass::Permanent
        };
    }
    let permanent = lower.contains("could not read username")
        || lower.contains("terminal prompts disabled")
        || lower.contains("permission denied")
        || (lower.contains("permission to") && lower.contains("denied"))
        || (lower.contains("destination path") && lower.contains("already exists"))
        || (lower.contains("remote branch") && lower.contains("not found"));
    if permanent {
        return CloneMessageClass::Permanent;
    }
    CloneMessageClass::Unknown
}

/// Backoff between clone attempts: 3s, then 9s.
///
/// GitHub's guidance for token replication is to wait a few seconds and retry
/// with the same token. Sub-second delays land inside the same replication
/// window and spend an attempt for nothing.
fn backoff() -> BackoffPolicy {
    BackoffPolicy {
        initial_delay: Duration::from_secs(3),
        factor:        3.0,
        max_delay:     Duration::from_secs(10),
        jitter:        false,
    }
}

/// Run a clone, repeating it while the failure looks transient.
///
/// `attempt` receives the 1-based attempt number. `classify` decides whether an
/// error is worth repeating; `None` returns it to the caller untouched. When a
/// deadline is present, a retry starts only when its backoff fits before that
/// deadline. The final error is returned as-is.
pub(crate) async fn retry_clone<T, E, Attempt, Fut, Classify>(
    provider: SandboxProviderKind,
    deadline: Option<time::Instant>,
    mut attempt: Attempt,
    classify: Classify,
) -> Result<T, E>
where
    Attempt: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, E>>,
    Classify: Fn(&E) -> Option<CloneRetryReason>,
{
    let backoff = backoff();

    for attempt_number in 1..MAX_ATTEMPTS {
        match attempt(attempt_number).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let Some(reason) = classify(&err) else {
                    return Err(err);
                };
                let delay = backoff.delay_for_attempt(attempt_number);
                if deadline.is_some_and(|deadline| {
                    delay >= deadline.saturating_duration_since(time::Instant::now())
                }) {
                    return Err(err);
                }
                // The failure text can carry git stderr, so log the category
                // rather than the message. The caller still reports the full
                // error if the attempts run out.
                tracing::warn!(
                    provider = %provider,
                    attempt = attempt_number,
                    max_attempts = MAX_ATTEMPTS,
                    reason = %reason,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "Git clone failed, retrying"
                );
                time::sleep(delay).await;
            }
        }
    }

    attempt(MAX_ATTEMPTS).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records the attempt numbers a closure was called with.
    #[derive(Default)]
    struct Attempts(Mutex<Vec<u32>>);

    impl Attempts {
        fn record(&self, attempt: u32) {
            self.0.lock().expect("attempt log mutex").push(attempt);
        }

        fn recorded(&self) -> Vec<u32> {
            self.0.lock().expect("attempt log mutex").clone()
        }
    }

    /// A classifier that treats every failure as worth repeating.
    const ALWAYS_RETRY: fn(&String) -> Option<CloneRetryReason> =
        |_| Some(CloneRetryReason::TokenReplication);

    #[test]
    fn private_repo_not_found_after_a_successful_mint_is_a_replication_lag() {
        assert_eq!(
            classify_message("repository not found: Repository not found.", true),
            CloneMessageClass::Retry(CloneRetryReason::TokenReplication)
        );
    }

    #[test]
    fn not_found_without_a_fresh_token_is_permanent() {
        assert_eq!(
            classify_message("repository not found: Repository not found.", false),
            CloneMessageClass::Permanent
        );
    }

    #[test]
    fn auth_failure_with_a_fresh_token_is_a_replication_lag() {
        assert_eq!(
            classify_message(
                "fatal: Authentication failed for 'https://github.com/owner/repo'",
                true
            ),
            CloneMessageClass::Retry(CloneRetryReason::TokenReplication)
        );
        assert_eq!(
            classify_message(
                "fatal: Authentication failed for 'https://github.com/owner/repo'",
                false
            ),
            CloneMessageClass::Permanent
        );
    }

    #[test]
    fn infra_failures_retry_without_credentials() {
        for message in [
            "fatal: unable to access: Could not resolve host: github.com",
            "error: RPC failed; curl 56 recv failure",
            "fatal: early EOF",
            "Operation timed out",
        ] {
            assert_eq!(
                classify_message(message, false),
                CloneMessageClass::Retry(CloneRetryReason::TransientInfra),
                "expected {message:?} to be transient"
            );
        }
    }

    #[test]
    fn genuine_failures_are_not_retried() {
        for message in [
            "fatal: could not read Username for 'https://github.com'",
            "remote: Permission to owner/repo.git denied",
            "fatal: destination path 'repo' already exists",
        ] {
            assert_eq!(
                classify_message(message, true),
                CloneMessageClass::Permanent,
                "expected {message:?} to fail fast"
            );
        }
    }

    #[test]
    fn unrecognized_failures_remain_unknown() {
        assert_eq!(
            classify_message("git clone stopped for an unexpected reason", true),
            CloneMessageClass::Unknown
        );
    }

    #[test]
    fn backoff_waits_seconds_not_milliseconds() {
        let backoff = backoff();
        assert_eq!(backoff.delay_for_attempt(1), Duration::from_secs(3));
        assert_eq!(backoff.delay_for_attempt(2), Duration::from_secs(9));
    }

    #[tokio::test(start_paused = true)]
    async fn first_success_runs_one_attempt() {
        let attempts = Attempts::default();

        let result = retry_clone(
            SandboxProviderKind::Docker,
            None,
            |attempt| {
                attempts.record(attempt);
                async move { Ok::<_, String>(attempt) }
            },
            ALWAYS_RETRY,
        )
        .await;

        assert_eq!(result, Ok(1));
        assert_eq!(attempts.recorded(), vec![1]);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_until_a_later_attempt_succeeds() {
        let attempts = Attempts::default();

        let result = retry_clone(
            SandboxProviderKind::Docker,
            None,
            |attempt| {
                attempts.record(attempt);
                async move {
                    if attempt < 3 {
                        Err("Repository not found.".to_string())
                    } else {
                        Ok(attempt)
                    }
                }
            },
            ALWAYS_RETRY,
        )
        .await;

        assert_eq!(result, Ok(3));
        assert_eq!(attempts.recorded(), vec![1, 2, 3]);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_attempts_return_the_final_error() {
        let attempts = Attempts::default();

        let result = retry_clone(
            SandboxProviderKind::Docker,
            None,
            |attempt| {
                attempts.record(attempt);
                async move { Err::<(), _>(format!("Repository not found. (attempt {attempt})")) }
            },
            ALWAYS_RETRY,
        )
        .await;

        assert_eq!(
            result,
            Err("Repository not found. (attempt 3)".to_string()),
            "the caller should see the last failure, not the first"
        );
        assert_eq!(attempts.recorded(), vec![1, 2, 3]);
    }

    #[tokio::test(start_paused = true)]
    async fn unretryable_failure_stops_immediately() {
        let attempts = Attempts::default();

        let result = retry_clone(
            SandboxProviderKind::Docker,
            None,
            |attempt| {
                attempts.record(attempt);
                async move { Err::<(), _>("permission denied".to_string()) }
            },
            |_: &String| None,
        )
        .await;

        assert_eq!(result, Err("permission denied".to_string()));
        assert_eq!(
            attempts.recorded(),
            vec![1],
            "a deterministic failure should not wait out the backoff"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_stops_retry_when_backoff_does_not_fit() {
        let attempts = Attempts::default();
        let deadline = time::Instant::now() + Duration::from_secs(2);

        let result = retry_clone(
            SandboxProviderKind::Docker,
            Some(deadline),
            |attempt| {
                attempts.record(attempt);
                async move { Err::<(), _>("temporary failure".to_string()) }
            },
            ALWAYS_RETRY,
        )
        .await;

        assert_eq!(result, Err("temporary failure".to_string()));
        assert_eq!(attempts.recorded(), vec![1]);
        assert_eq!(time::Instant::now() + Duration::from_secs(2), deadline);
    }
}
