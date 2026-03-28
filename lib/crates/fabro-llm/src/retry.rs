use crate::error::SdkError;
use crate::types::RetryPolicy;
use std::future::Future;
use std::time::Duration;
use tokio::time;
use tracing::warn;

/// Retry a fallible async operation according to the given policy (Section 6.6).
///
/// - Only retries if the error is retryable.
/// - Respects Retry-After from the error if less than `max_delay`.
/// - If Retry-After exceeds `max_delay`, does NOT retry.
///
/// # Errors
///
/// Returns the last `SdkError` if all retries are exhausted or the error is non-retryable.
pub async fn retry<F, Fut, T>(policy: &RetryPolicy, mut operation: F) -> Result<T, SdkError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SdkError>>,
{
    let mut attempt = 0u32;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                if !err.retryable() || attempt >= policy.max_retries {
                    return Err(err);
                }

                // Check Retry-After
                let delay = if let Some(retry_after) = err.retry_after() {
                    let retry_after_dur = Duration::from_secs_f64(retry_after);
                    if retry_after_dur > policy.backoff.max_delay {
                        return Err(err);
                    }
                    retry_after_dur
                } else {
                    // Convert from 0-indexed (fabro-llm convention) to 1-indexed (BackoffPolicy)
                    policy.backoff.delay_for_attempt(attempt + 1)
                };

                warn!(
                    attempt = attempt,
                    delay_secs = delay.as_secs_f64(),
                    error = %err,
                    "LLM request failed, retrying"
                );

                if let Some(ref on_retry) = policy.on_retry {
                    on_retry(&err, attempt, delay);
                }

                time::sleep(delay).await;

                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RetryPolicy;
    use fabro_util::backoff::BackoffPolicy;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn fast_backoff() -> BackoffPolicy {
        BackoffPolicy {
            initial_delay: Duration::from_micros(1),
            factor: 2.0,
            max_delay: Duration::from_secs(60),
            jitter: false,
        }
    }

    #[tokio::test]
    async fn retry_succeeds_first_try() {
        let policy = RetryPolicy {
            max_retries: 2,
            backoff: BackoffPolicy {
                jitter: false,
                ..BackoffPolicy::default()
            },
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Ok::<_, SdkError>(42)
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_succeeds_after_retries() {
        let policy = RetryPolicy {
            max_retries: 3,
            backoff: fast_backoff(),
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                let count = cc.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(SdkError::Provider {
                        kind: crate::error::ProviderErrorKind::Server,
                        detail: Box::new(crate::error::ProviderErrorDetail {
                            status_code: Some(500),
                            ..crate::error::ProviderErrorDetail::new("error", "test")
                        }),
                    })
                } else {
                    Ok(99)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 99);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_gives_up_after_max_retries() {
        let policy = RetryPolicy {
            max_retries: 2,
            backoff: fast_backoff(),
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(SdkError::Provider {
                    kind: crate::error::ProviderErrorKind::Server,
                    detail: Box::new(crate::error::ProviderErrorDetail {
                        status_code: Some(500),
                        ..crate::error::ProviderErrorDetail::new("error", "test")
                    }),
                })
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn retry_does_not_retry_non_retryable() {
        let policy = RetryPolicy {
            max_retries: 3,
            backoff: fast_backoff(),
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(SdkError::Provider {
                    kind: crate::error::ProviderErrorKind::Authentication,
                    detail: Box::new(crate::error::ProviderErrorDetail {
                        status_code: Some(401),
                        ..crate::error::ProviderErrorDetail::new("bad key", "test")
                    }),
                })
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_skips_when_retry_after_exceeds_max_delay() {
        let policy = RetryPolicy {
            max_retries: 3,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_micros(1),
                factor: 2.0,
                max_delay: Duration::from_secs(5),
                jitter: false,
            },
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(SdkError::Provider {
                    kind: crate::error::ProviderErrorKind::RateLimit,
                    detail: Box::new(crate::error::ProviderErrorDetail {
                        status_code: Some(429),
                        retry_after: Some(100.0), // Way beyond max_delay
                        ..crate::error::ProviderErrorDetail::new("rate limited", "test")
                    }),
                })
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_uses_retry_after_when_within_limit() {
        let policy = RetryPolicy {
            max_retries: 1,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_secs(10), // high, but retry_after is low
                factor: 2.0,
                max_delay: Duration::from_secs(60),
                jitter: false,
            },
            ..Default::default()
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let start = tokio::time::Instant::now();
        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                let count = cc.fetch_add(1, Ordering::SeqCst);
                if count < 1 {
                    Err(SdkError::Provider {
                        kind: crate::error::ProviderErrorKind::RateLimit,
                        detail: Box::new(crate::error::ProviderErrorDetail {
                            status_code: Some(429),
                            retry_after: Some(0.01),
                            ..crate::error::ProviderErrorDetail::new("rate limited", "test")
                        }),
                    })
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        let elapsed = start.elapsed();
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
        // Should have waited ~0.01s, not ~10s
        assert!(elapsed.as_secs_f64() < 1.0);
    }

    #[tokio::test]
    async fn retry_invokes_on_retry_callback() {
        let retry_attempts = Arc::new(AtomicU32::new(0));
        let retry_attempts_clone = retry_attempts.clone();

        let policy = RetryPolicy {
            max_retries: 2,
            backoff: fast_backoff(),
            on_retry: Some(Arc::new(move |_err, _attempt, _delay| {
                retry_attempts_clone.fetch_add(1, Ordering::SeqCst);
            })),
        };

        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry(&policy, || {
            let cc = cc.clone();
            async move {
                let count = cc.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(SdkError::Provider {
                        kind: crate::error::ProviderErrorKind::Server,
                        detail: Box::new(crate::error::ProviderErrorDetail {
                            status_code: Some(500),
                            ..crate::error::ProviderErrorDetail::new("error", "test")
                        }),
                    })
                } else {
                    Ok(99)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 99);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
        // on_retry should have been called twice (before each retry)
        assert_eq!(retry_attempts.load(Ordering::SeqCst), 2);
    }
}
