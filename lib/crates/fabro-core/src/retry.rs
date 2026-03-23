use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial_delay: Duration,
    pub factor: f64,
    pub max_delay: Duration,
    pub jitter: bool,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            factor: 2.0,
            max_delay: Duration::from_secs(60),
            jitter: false,
        }
    }
}

impl BackoffPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let multiplier = self.factor.powi(attempt.saturating_sub(1) as i32);
        let delay = self.initial_delay.mul_f64(multiplier);
        if delay > self.max_delay {
            self.max_delay
        } else {
            delay
        }
    }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffPolicy,
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            backoff: BackoffPolicy::default(),
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
    fn backoff_delay_first_attempt() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(100),
            factor: 2.0,
            max_delay: Duration::from_secs(10),
            jitter: false,
        };
        assert_eq!(b.delay_for_attempt(1), Duration::from_millis(100));
    }

    #[test]
    fn backoff_delay_exponential() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(100),
            factor: 2.0,
            max_delay: Duration::from_secs(10),
            jitter: false,
        };
        assert_eq!(b.delay_for_attempt(2), Duration::from_millis(200));
        assert_eq!(b.delay_for_attempt(3), Duration::from_millis(400));
        assert_eq!(b.delay_for_attempt(4), Duration::from_millis(800));
    }

    #[test]
    fn backoff_delay_capped_at_max() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(100),
            factor: 2.0,
            max_delay: Duration::from_millis(300),
            jitter: false,
        };
        assert_eq!(b.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(b.delay_for_attempt(2), Duration::from_millis(200));
        assert_eq!(b.delay_for_attempt(3), Duration::from_millis(300)); // capped
        assert_eq!(b.delay_for_attempt(4), Duration::from_millis(300)); // still capped
    }

    #[test]
    fn retry_policy_none_is_single_attempt() {
        let p = RetryPolicy::none();
        assert_eq!(p.max_attempts, 1);
    }
}
