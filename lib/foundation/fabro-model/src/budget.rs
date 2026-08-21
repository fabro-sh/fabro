//! Run-level LLM spend budget tracking.
//!
//! A [`RunBudget`] is shared (via `Arc`) between the workflow engine and the
//! agent session. Every site that records LLM usage charges it here and acts
//! on the returned [`BudgetCharge`]: a tripped limit halts the run with
//! `BudgetExhausted`, and a newly crossed warning threshold emits a one-shot
//! run notice.

use std::fmt;
use std::sync::Mutex;

use crate::billing::UsdMicros;

/// Numerator/denominator of the warning threshold: warn when spend reaches
/// 4/5 (80%) of a limit.
const WARN_NUMERATOR: i64 = 4;
const WARN_DENOMINATOR: i64 = 5;

/// A budget dimension's limit and the spend measured against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetUsage {
    Cost { limit: UsdMicros, spent: UsdMicros },
    Tokens { limit: u64, spent: u64 },
}

impl BudgetUsage {
    /// Message for a run halted by this limit.
    #[must_use]
    pub fn exceeded_message(&self) -> String {
        match self {
            Self::Cost { limit, spent } => format!(
                "run cost {} exceeded run.limits.max_cost {}",
                format_usd(*spent),
                format_usd(*limit)
            ),
            Self::Tokens { limit, spent } => {
                format!("run used {spent} tokens, exceeding run.limits.max_tokens {limit}")
            }
        }
    }

    /// Message for the one-shot 80% warning notice.
    #[must_use]
    pub fn warning_message(&self) -> String {
        match self {
            Self::Cost { limit, spent } => format!(
                "run cost {} reached 80% of run.limits.max_cost {}",
                format_usd(*spent),
                format_usd(*limit)
            ),
            Self::Tokens { limit, spent } => {
                format!("run used {spent} tokens, 80% of run.limits.max_tokens {limit}")
            }
        }
    }
}

impl fmt::Display for BudgetUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cost { limit, spent } => {
                write!(f, "cost {} of {}", format_usd(*spent), format_usd(*limit))
            }
            Self::Tokens { limit, spent } => write!(f, "{spent} of {limit} tokens"),
        }
    }
}

fn format_usd(value: UsdMicros) -> String {
    format!("${:.2}", value.0 as f64 / 1_000_000.0)
}

/// Thread-safe accumulator for a run's LLM spend against optional limits.
///
/// Token spend compares against `TokenCounts::total_tokens()` (all five
/// buckets). Cost spend stays `None` until a cost is observed — a response
/// with no provider-reported cost and no catalog price does not advance it,
/// which is why `max_tokens` is the backstop for unpriced models.
///
/// Charging and reporting are split so the charging site does not need
/// access to the run's event stream: [`RunBudget::charge`] returns only the
/// exceeded limit (the caller must halt), while newly crossed warning
/// thresholds queue internally until a site that can emit run notices drains
/// them with [`RunBudget::take_pending_warnings`].
#[derive(Debug)]
pub struct RunBudget {
    max_cost:   Option<UsdMicros>,
    max_tokens: Option<u64>,
    state:      Mutex<BudgetState>,
}

#[derive(Debug, Default)]
struct BudgetState {
    tokens_spent:     u64,
    cost_spent:       Option<UsdMicros>,
    cost_warned:      bool,
    tokens_warned:    bool,
    pending_warnings: Vec<BudgetUsage>,
}

impl RunBudget {
    #[must_use]
    pub fn new(max_cost: Option<UsdMicros>, max_tokens: Option<u64>) -> Self {
        Self {
            max_cost,
            max_tokens,
            state: Mutex::new(BudgetState::default()),
        }
    }

    /// Whether no limit is configured. An unlimited budget never trips.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.max_cost.is_none() && self.max_tokens.is_none()
    }

    /// Charge usage totals to the budget. Returns the limit the accumulated
    /// spend now exceeds, if any — the caller must halt the run. When both
    /// limits are exceeded, the cost limit is reported.
    ///
    /// `tokens` is a `total_tokens()`-style sum; `cost` is `None` when the
    /// response carried no cost data. Newly crossed warning thresholds are
    /// queued for [`Self::take_pending_warnings`].
    pub fn charge(&self, tokens: i64, cost: Option<UsdMicros>) -> Option<BudgetUsage> {
        let mut state = self.state.lock().expect("run budget lock poisoned");
        state.tokens_spent = state
            .tokens_spent
            .saturating_add(u64::try_from(tokens).unwrap_or(0));
        UsdMicros::accumulate(&mut state.cost_spent, cost);

        let exceeded = exceeded_locked(self, &state);

        if let (Some(limit), Some(spent)) = (self.max_cost, state.cost_spent) {
            if !state.cost_warned && spent <= limit && crossed_warn_threshold(spent.0, limit.0) {
                state.cost_warned = true;
                state
                    .pending_warnings
                    .push(BudgetUsage::Cost { limit, spent });
            }
        }
        if let Some(limit) = self.max_tokens {
            let crossed = crossed_warn_threshold(
                i64::try_from(state.tokens_spent).unwrap_or(i64::MAX),
                i64::try_from(limit).unwrap_or(i64::MAX),
            );
            if !state.tokens_warned && state.tokens_spent <= limit && crossed {
                state.tokens_warned = true;
                let spent = state.tokens_spent;
                state
                    .pending_warnings
                    .push(BudgetUsage::Tokens { limit, spent });
            }
        }

        exceeded
    }

    /// Drain warning thresholds crossed since the last call. Each warning is
    /// returned exactly once for the budget's lifetime; the caller emits
    /// them as run notices.
    #[must_use]
    pub fn take_pending_warnings(&self) -> Vec<BudgetUsage> {
        std::mem::take(
            &mut self
                .state
                .lock()
                .expect("run budget lock poisoned")
                .pending_warnings,
        )
    }

    /// The currently exceeded limit, if any. Pre-flight check: lets a caller
    /// skip an LLM request when the budget is already spent.
    #[must_use]
    pub fn exceeded(&self) -> Option<BudgetUsage> {
        let state = self.state.lock().expect("run budget lock poisoned");
        exceeded_locked(self, &state)
    }

    #[must_use]
    pub fn tokens_spent(&self) -> u64 {
        self.state
            .lock()
            .expect("run budget lock poisoned")
            .tokens_spent
    }

    #[must_use]
    pub fn cost_spent(&self) -> Option<UsdMicros> {
        self.state
            .lock()
            .expect("run budget lock poisoned")
            .cost_spent
    }
}

fn exceeded_locked(budget: &RunBudget, state: &BudgetState) -> Option<BudgetUsage> {
    if let (Some(limit), Some(spent)) = (budget.max_cost, state.cost_spent) {
        if spent > limit {
            return Some(BudgetUsage::Cost { limit, spent });
        }
    }
    if let Some(limit) = budget.max_tokens {
        if state.tokens_spent > limit {
            return Some(BudgetUsage::Tokens {
                limit,
                spent: state.tokens_spent,
            });
        }
    }
    None
}

fn crossed_warn_threshold(spent: i64, limit: i64) -> bool {
    spent.saturating_mul(WARN_DENOMINATOR) >= limit.saturating_mul(WARN_NUMERATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(value: f64) -> UsdMicros {
        UsdMicros::from_usd(value)
    }

    #[test]
    fn unlimited_budget_never_trips() {
        let budget = RunBudget::new(None, None);
        assert!(budget.is_unlimited());
        assert_eq!(budget.charge(1_000_000_000, Some(usd(10_000.0))), None);
        assert!(budget.take_pending_warnings().is_empty());
        assert!(budget.exceeded().is_none());
    }

    #[test]
    fn cost_limit_trips_when_spend_passes_it() {
        let budget = RunBudget::new(Some(usd(25.0)), None);
        assert!(budget.charge(100, Some(usd(24.0))).is_none());
        let exceeded = budget.charge(100, Some(usd(1.13)));
        assert_eq!(
            exceeded,
            Some(BudgetUsage::Cost {
                limit: usd(25.0),
                spent: usd(25.13),
            })
        );
        assert!(budget.exceeded().is_some());
    }

    #[test]
    fn spend_equal_to_the_limit_does_not_trip() {
        let budget = RunBudget::new(Some(usd(25.0)), Some(1_000));
        assert!(budget.charge(1_000, Some(usd(25.0))).is_none());
        assert!(budget.exceeded().is_none());
    }

    #[test]
    fn token_limit_trips_without_cost_data() {
        let budget = RunBudget::new(Some(usd(25.0)), Some(1_000));
        let exceeded = budget.charge(1_500, None);
        assert_eq!(
            exceeded,
            Some(BudgetUsage::Tokens {
                limit: 1_000,
                spent: 1_500,
            })
        );
    }

    #[test]
    fn cost_exceeded_wins_over_tokens_exceeded() {
        let budget = RunBudget::new(Some(usd(1.0)), Some(100));
        let exceeded = budget.charge(200, Some(usd(2.0)));
        assert!(matches!(exceeded, Some(BudgetUsage::Cost { .. })));
    }

    #[test]
    fn warnings_fire_once_per_dimension() {
        let budget = RunBudget::new(Some(usd(10.0)), Some(1_000));

        assert!(budget.charge(100, Some(usd(1.0))).is_none());
        assert!(budget.take_pending_warnings().is_empty());

        assert!(budget.charge(750, Some(usd(7.5))).is_none());
        assert_eq!(budget.take_pending_warnings(), vec![
            BudgetUsage::Cost {
                limit: usd(10.0),
                spent: usd(8.5),
            },
            BudgetUsage::Tokens {
                limit: 1_000,
                spent: 850,
            },
        ]);

        assert!(budget.charge(50, Some(usd(0.5))).is_none());
        assert!(budget.take_pending_warnings().is_empty());
    }

    #[test]
    fn a_charge_that_jumps_past_the_limit_reports_exceeded_not_warning() {
        let budget = RunBudget::new(Some(usd(10.0)), None);
        let exceeded = budget.charge(100, Some(usd(11.0)));
        assert!(matches!(exceeded, Some(BudgetUsage::Cost { .. })));
        assert!(budget.take_pending_warnings().is_empty());
    }

    #[test]
    fn missing_cost_does_not_advance_cost_spend() {
        let budget = RunBudget::new(Some(usd(1.0)), None);
        assert!(budget.charge(1_000_000, None).is_none());
        assert_eq!(budget.cost_spent(), None);
    }

    #[test]
    fn negative_token_totals_are_ignored() {
        let budget = RunBudget::new(None, Some(1_000));
        assert!(budget.charge(-500, None).is_none());
        assert_eq!(budget.tokens_spent(), 0);
    }

    #[test]
    fn seeding_past_the_limit_trips_immediately() {
        let budget = RunBudget::new(Some(usd(25.0)), None);
        assert!(budget.charge(5_000, Some(usd(26.0))).is_some());
        assert!(budget.exceeded().is_some());
    }

    #[test]
    fn messages_name_the_config_keys() {
        let cost = BudgetUsage::Cost {
            limit: usd(25.0),
            spent: usd(25.13),
        };
        assert_eq!(
            cost.exceeded_message(),
            "run cost $25.13 exceeded run.limits.max_cost $25.00"
        );
        let tokens = BudgetUsage::Tokens {
            limit: 1_000,
            spent: 850,
        };
        assert_eq!(
            tokens.warning_message(),
            "run used 850 tokens, 80% of run.limits.max_tokens 1000"
        );
    }
}
