use crate::outcome::StageUsage;

/// Compute the dollar cost for a stage's token usage, if pricing is available.
#[must_use]
pub fn compute_stage_cost(usage: &StageUsage) -> Option<f64> {
    let info = fabro_model::get_model_info(&usage.model)?;
    let input_rate = info.costs.input_cost_per_mtok?;
    let output_rate = info.costs.output_cost_per_mtok?;
    let multiplier = if usage.speed.as_deref() == Some("fast") {
        6.0
    } else {
        1.0
    };
    Some(
        (usage.input_tokens as f64 * input_rate / 1_000_000.0
            + usage.output_tokens as f64 * output_rate / 1_000_000.0)
            * multiplier,
    )
}

/// Format a dollar cost for display (e.g. `"$1.23"`).
#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

#[cfg(test)]
mod tests {
    use super::{compute_stage_cost, format_cost};
    use crate::outcome::StageUsage;

    #[test]
    fn format_cost_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
    }

    #[test]
    fn format_cost_normal() {
        assert_eq!(format_cost(1.5), "$1.50");
    }

    #[test]
    fn format_cost_rounds() {
        assert_eq!(format_cost(123.456), "$123.46");
    }

    #[test]
    fn compute_stage_cost_known_model() {
        let usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        let cost = compute_stage_cost(&usage);
        assert!(cost.is_some());
        assert!(cost.unwrap() > 0.0);
    }

    #[test]
    fn compute_stage_cost_unknown_model() {
        let usage = StageUsage {
            model: "nonexistent-model-xyz".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        assert_eq!(compute_stage_cost(&usage), None);
    }

    #[test]
    fn compute_stage_cost_fast_mode_6x_multiplier() {
        let standard_usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        let fast_usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: Some("fast".into()),
            cost: None,
        };
        let standard_cost = compute_stage_cost(&standard_usage).unwrap();
        let fast_cost = compute_stage_cost(&fast_usage).unwrap();
        assert!(
            (fast_cost - standard_cost * 6.0).abs() < 1e-10,
            "fast mode should be 6x standard cost: standard={standard_cost}, fast={fast_cost}"
        );
    }
}
