pub use fabro_util::backoff::BackoffPolicy;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff:      BackoffPolicy,
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            backoff:      BackoffPolicy::default(),
        }
    }

    pub fn with_max_attempts(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            backoff: BackoffPolicy::default(),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_none_is_single_attempt() {
        let p = RetryPolicy::none();
        assert_eq!(p.max_attempts, 1);
    }
}
