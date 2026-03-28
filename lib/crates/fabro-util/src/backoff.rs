use std::time::Duration;

use rand::Rng;

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
        let multiplier = self
            .factor
            .powi(i32::try_from(attempt.saturating_sub(1)).unwrap());
        let base_delay = self.initial_delay.mul_f64(multiplier);
        let capped = if base_delay > self.max_delay {
            self.max_delay
        } else {
            base_delay
        };
        if self.jitter {
            // Apply jitter: random factor in [0.5, 1.5)
            let jitter_factor = rand::thread_rng().gen_range(0.5..1.5);
            capped.mul_f64(jitter_factor)
        } else {
            capped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_first_attempt() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(100),
            factor: 2.0,
            max_delay: Duration::from_secs(10),
            jitter: false,
        };
        assert_eq!(b.delay_for_attempt(1), Duration::from_millis(100));
    }

    #[test]
    fn delay_exponential() {
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
    fn delay_capped_at_max() {
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
    fn delay_with_jitter_within_range() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(1000),
            factor: 1.0,
            max_delay: Duration::from_secs(10),
            jitter: true,
        };
        let base = Duration::from_millis(1000);
        let min = base.mul_f64(0.5);
        let max = base.mul_f64(1.5);

        for _ in 0..100 {
            let delay = b.delay_for_attempt(1);
            assert!(
                delay >= min && delay <= max,
                "delay {:?} out of range [{:?}, {:?}]",
                delay,
                min,
                max,
            );
        }
    }

    #[test]
    fn delay_linear_factor() {
        let b = BackoffPolicy {
            initial_delay: Duration::from_millis(500),
            factor: 1.0,
            max_delay: Duration::from_secs(60),
            jitter: false,
        };
        assert_eq!(b.delay_for_attempt(1), Duration::from_millis(500));
        assert_eq!(b.delay_for_attempt(2), Duration::from_millis(500));
        assert_eq!(b.delay_for_attempt(3), Duration::from_millis(500));
    }
}
