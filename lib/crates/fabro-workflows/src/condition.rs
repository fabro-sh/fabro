/// Condition expression evaluator for edge guards (spec Section 10).
///
/// The parser lives in `fabro_graphviz::condition`; this module re-exports
/// `parse_condition` and provides runtime evaluation against `Outcome`/`Context`.
use fabro_graphviz::condition::{Clause, ConditionExpr, Op};

use crate::context::keys;
use crate::context::Context;
use crate::outcome::Outcome;

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

fn resolve_key(key: &str, outcome: &Outcome, context: &Context) -> String {
    if key == keys::OUTCOME {
        return outcome.status.to_string();
    }
    if key == keys::PREFERRED_LABEL {
        return outcome.preferred_label.as_deref().unwrap_or("").to_string();
    }
    if let Some(path) = key.strip_prefix("context.") {
        if let Some(val) = context.get(key) {
            return json_value_to_string(&val);
        }
        if let Some(val) = context.get(path) {
            return json_value_to_string(&val);
        }
        return String::new();
    }
    context
        .get(key)
        .map_or_else(String::new, |val| json_value_to_string(&val))
}

fn resolve_key_value(key: &str, outcome: &Outcome, context: &Context) -> serde_json::Value {
    if key == keys::OUTCOME {
        return serde_json::Value::String(outcome.status.to_string());
    }
    if key == keys::PREFERRED_LABEL {
        return outcome
            .preferred_label
            .as_deref()
            .map_or(serde_json::Value::Null, |s| {
                serde_json::Value::String(s.to_string())
            });
    }
    if let Some(path) = key.strip_prefix("context.") {
        if let Some(val) = context.get(key) {
            return val;
        }
        if let Some(val) = context.get(path) {
            return val;
        }
        return serde_json::Value::Null;
    }
    context.get(key).unwrap_or(serde_json::Value::Null)
}

fn json_value_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn is_truthy(s: &str) -> bool {
    !s.is_empty() && s != "false" && s != "0"
}

fn eval_expr(expr: &ConditionExpr, outcome: &Outcome, context: &Context) -> bool {
    match expr {
        ConditionExpr::And(children) => {
            if children.is_empty() {
                return true;
            }
            children.iter().all(|c| eval_expr(c, outcome, context))
        }
        ConditionExpr::Or(children) => children.iter().any(|c| eval_expr(c, outcome, context)),
        ConditionExpr::Not(inner) => !eval_expr(inner, outcome, context),
        ConditionExpr::Clause(clause) => eval_clause(clause, outcome, context),
    }
}

fn eval_clause(clause: &Clause, outcome: &Outcome, context: &Context) -> bool {
    match &clause.op {
        Op::Truthy => {
            let resolved = resolve_key(&clause.key, outcome, context);
            is_truthy(&resolved)
        }
        Op::Eq => {
            let resolved = resolve_key(&clause.key, outcome, context);
            resolved == clause.value
        }
        Op::NotEq => {
            let resolved = resolve_key(&clause.key, outcome, context);
            resolved != clause.value
        }
        Op::Gt | Op::Lt | Op::Gte | Op::Lte => {
            let resolved = resolve_key(&clause.key, outcome, context);
            let lhs: f64 = match resolved.parse() {
                Ok(v) => v,
                Err(_) => return false,
            };
            let rhs: f64 = match clause.value.parse() {
                Ok(v) => v,
                Err(_) => return false,
            };
            match &clause.op {
                Op::Gt => lhs > rhs,
                Op::Lt => lhs < rhs,
                Op::Gte => lhs >= rhs,
                Op::Lte => lhs <= rhs,
                _ => unreachable!(),
            }
        }
        Op::Contains => {
            let raw = resolve_key_value(&clause.key, outcome, context);
            match &raw {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .any(|elem| json_value_to_string(elem) == clause.value),
                _ => {
                    let s = json_value_to_string(&raw);
                    s.contains(&clause.value)
                }
            }
        }
        Op::Matches => {
            let resolved = resolve_key(&clause.key, outcome, context);
            // Regex was validated at parse time, so unwrap is safe
            regex::Regex::new(&clause.value)
                .map(|re| re.is_match(&resolved))
                .unwrap_or(false)
        }
    }
}

/// Evaluate a condition expression against an outcome and context.
/// Empty conditions always return true.
#[must_use]
pub fn evaluate_condition(expr: &str, outcome: &Outcome, context: &Context) -> bool {
    use fabro_graphviz::condition::parse_condition_expr;
    let Ok(parsed) = parse_condition_expr(expr) else {
        return false;
    };
    eval_expr(&parsed, outcome, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::StageStatus;

    fn make_outcome(status: StageStatus) -> Outcome {
        Outcome {
            status,
            ..Outcome::success()
        }
    }

    // -----------------------------------------------------------------------
    // Phase 0: Existing behavior preserved
    // -----------------------------------------------------------------------

    #[test]
    fn empty_condition_is_true() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition("", &outcome, &context));
        assert!(evaluate_condition("  ", &outcome, &context));
    }

    #[test]
    fn outcome_equals_success() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition("outcome=success", &outcome, &context));
        assert!(!evaluate_condition("outcome=fail", &outcome, &context));
    }

    #[test]
    fn outcome_not_equals() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition("outcome!=fail", &outcome, &context));
        assert!(!evaluate_condition("outcome!=success", &outcome, &context));
    }

    #[test]
    fn preferred_label_match() {
        let mut outcome = make_outcome(StageStatus::Success);
        outcome.preferred_label = Some("Fix".to_string());
        let context = Context::new();
        assert!(evaluate_condition(
            "preferred_label=Fix",
            &outcome,
            &context
        ));
        assert!(!evaluate_condition(
            "preferred_label=Approve",
            &outcome,
            &context
        ));
    }

    #[test]
    fn context_key_with_prefix() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("tests_passed", serde_json::json!("true"));
        assert!(evaluate_condition(
            "context.tests_passed=true",
            &outcome,
            &context
        ));
    }

    #[test]
    fn bare_key_context_lookup() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("custom_key", serde_json::json!("custom_value"));
        assert!(evaluate_condition(
            "custom_key=custom_value",
            &outcome,
            &context
        ));
    }

    #[test]
    fn missing_key_compares_as_empty() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(!evaluate_condition(
            "missing_key=something",
            &outcome,
            &context
        ));
        assert!(evaluate_condition("missing_key=", &outcome, &context));
    }

    #[test]
    fn multiple_clauses_and() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("tests_passed", serde_json::json!("true"));
        assert!(evaluate_condition(
            "outcome=success && context.tests_passed=true",
            &outcome,
            &context
        ));
        assert!(!evaluate_condition(
            "outcome=fail && context.tests_passed=true",
            &outcome,
            &context
        ));
    }

    #[test]
    fn context_dotted_fallback() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("loop_state", serde_json::json!("exhausted"));
        assert!(evaluate_condition(
            "context.loop_state=exhausted",
            &outcome,
            &context
        ));
    }

    #[test]
    fn bare_key_truthy_when_non_empty() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("my_flag", serde_json::json!("yes"));
        assert!(evaluate_condition("my_flag", &outcome, &context));
    }

    #[test]
    fn bare_key_falsy_when_empty() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(!evaluate_condition("missing_key", &outcome, &context));
    }

    #[test]
    fn bare_key_falsy_when_false_string() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("my_flag", serde_json::json!("false"));
        assert!(!evaluate_condition("my_flag", &outcome, &context));
    }

    #[test]
    fn bare_key_falsy_when_zero_string() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("my_flag", serde_json::json!("0"));
        assert!(!evaluate_condition("my_flag", &outcome, &context));
    }

    #[test]
    fn bare_key_with_and_clause() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("flag", serde_json::json!("yes"));
        assert!(evaluate_condition(
            "outcome=success && flag",
            &outcome,
            &context
        ));
    }

    #[test]
    fn context_failure_class_matches_when_set() {
        let outcome = make_outcome(StageStatus::Fail);
        let context = Context::new();
        context.set(keys::FAILURE_CLASS, serde_json::json!("budget_exhausted"));
        assert!(evaluate_condition(
            "context.failure_class=budget_exhausted",
            &outcome,
            &context
        ));
    }

    #[test]
    fn context_failure_class_not_equals_on_success() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set(keys::FAILURE_CLASS, serde_json::json!(""));
        assert!(evaluate_condition(
            "context.failure_class!=transient_infra",
            &outcome,
            &context
        ));
    }

    #[test]
    fn context_failure_class_combined_with_outcome() {
        let outcome = make_outcome(StageStatus::Fail);
        let context = Context::new();
        context.set(keys::FAILURE_CLASS, serde_json::json!("transient_infra"));
        assert!(evaluate_condition(
            "outcome=fail && context.failure_class=transient_infra",
            &outcome,
            &context
        ));
        assert!(!evaluate_condition(
            "outcome=fail && context.failure_class=deterministic",
            &outcome,
            &context
        ));
    }

    // -----------------------------------------------------------------------
    // Phase 1: Numeric comparisons
    // -----------------------------------------------------------------------

    #[test]
    fn numeric_gt() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("score", serde_json::json!(90));
        assert!(evaluate_condition("context.score > 80", &outcome, &context));
        context.set("score", serde_json::json!(70));
        assert!(!evaluate_condition(
            "context.score > 80",
            &outcome,
            &context
        ));
    }

    #[test]
    fn numeric_gte() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("score", serde_json::json!(80));
        assert!(evaluate_condition(
            "context.score >= 80",
            &outcome,
            &context
        ));
    }

    #[test]
    fn numeric_lte() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("score", serde_json::json!(80));
        assert!(evaluate_condition(
            "context.score <= 80",
            &outcome,
            &context
        ));
    }

    #[test]
    fn numeric_lt() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("count", serde_json::json!(3));
        assert!(evaluate_condition("context.count < 5", &outcome, &context));
    }

    #[test]
    fn numeric_float() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("ratio", serde_json::json!(0.75));
        assert!(evaluate_condition(
            "context.ratio > 0.5",
            &outcome,
            &context
        ));
    }

    #[test]
    fn numeric_non_numeric_returns_false() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("score", serde_json::json!("not_a_number"));
        assert!(!evaluate_condition(
            "context.score > 80",
            &outcome,
            &context
        ));
    }

    // -----------------------------------------------------------------------
    // Phase 2: contains operator
    // -----------------------------------------------------------------------

    #[test]
    fn contains_substring() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("message", serde_json::json!("an error occurred"));
        assert!(evaluate_condition(
            "context.message contains error",
            &outcome,
            &context
        ));
        context.set("message", serde_json::json!("all good"));
        assert!(!evaluate_condition(
            "context.message contains error",
            &outcome,
            &context
        ));
    }

    #[test]
    fn contains_case_sensitive() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("message", serde_json::json!("an error occurred"));
        assert!(!evaluate_condition(
            "context.message contains Error",
            &outcome,
            &context
        ));
    }

    #[test]
    fn contains_json_array() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("tags", serde_json::json!(["urgent", "low"]));
        assert!(evaluate_condition(
            "context.tags contains urgent",
            &outcome,
            &context
        ));
        assert!(!evaluate_condition(
            "context.tags contains critical",
            &outcome,
            &context
        ));
    }

    // -----------------------------------------------------------------------
    // Phase 3: matches operator (regex)
    // -----------------------------------------------------------------------

    #[test]
    fn matches_regex() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("version", serde_json::json!("v2.0"));
        assert!(evaluate_condition(
            r"context.version matches ^v\d+",
            &outcome,
            &context
        ));
        context.set("version", serde_json::json!("beta"));
        assert!(!evaluate_condition(
            r"context.version matches ^v\d+",
            &outcome,
            &context
        ));
    }

    // -----------------------------------------------------------------------
    // Phase 4: OR (||)
    // -----------------------------------------------------------------------

    #[test]
    fn or_disjunction() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition(
            "outcome=success || outcome=partial_success",
            &outcome,
            &context
        ));
        let outcome = make_outcome(StageStatus::Fail);
        assert!(!evaluate_condition(
            "outcome=success || outcome=partial_success",
            &outcome,
            &context
        ));
    }

    #[test]
    fn or_precedence_and_binds_tighter() {
        // a=1 && b=2 || c=3  is  (a=1 AND b=2) OR c=3
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("a", serde_json::json!("0"));
        context.set("b", serde_json::json!("2"));
        context.set("c", serde_json::json!("3"));
        // a=1 is false, b=2 is true => AND is false; c=3 is true => OR is true
        assert!(evaluate_condition("a=1 && b=2 || c=3", &outcome, &context));
    }

    #[test]
    fn or_precedence_right_and() {
        // a=1 || b=2 && c=3  is  a=1 OR (b=2 AND c=3)
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("a", serde_json::json!("0"));
        context.set("b", serde_json::json!("2"));
        context.set("c", serde_json::json!("0"));
        // a=1 false; b=2 true, c=3 false => AND false; OR false
        assert!(!evaluate_condition("a=1 || b=2 && c=3", &outcome, &context));
    }

    // -----------------------------------------------------------------------
    // Phase 5: NOT (!)
    // -----------------------------------------------------------------------

    #[test]
    fn not_negation() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition("!outcome=fail", &outcome, &context));
        assert!(!evaluate_condition("!outcome=success", &outcome, &context));
    }

    #[test]
    fn not_missing_key_is_true() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition("!missing_key", &outcome, &context));
    }

    #[test]
    fn not_with_and() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("ready", serde_json::json!("true"));
        assert!(evaluate_condition(
            "!outcome=fail && context.ready=true",
            &outcome,
            &context
        ));
    }

    // -----------------------------------------------------------------------
    // Phase 6: Quoted literal values (spec parse_literal)
    // -----------------------------------------------------------------------

    #[test]
    fn quoted_value_matches_bare_value() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition(
            r#"outcome="success""#,
            &outcome,
            &context
        ));
        assert!(!evaluate_condition(r#"outcome="fail""#, &outcome, &context));
    }

    #[test]
    fn quoted_not_eq_matches() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        assert!(evaluate_condition(r#"outcome!="fail""#, &outcome, &context));
        assert!(!evaluate_condition(
            r#"outcome!="success""#,
            &outcome,
            &context
        ));
    }

    #[test]
    fn quoted_context_value() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("env", serde_json::json!("production"));
        assert!(evaluate_condition(
            r#"context.env="production""#,
            &outcome,
            &context
        ));
    }

    #[test]
    fn quoted_and_bare_equivalent_in_compound() {
        let outcome = make_outcome(StageStatus::Success);
        let context = Context::new();
        context.set("ready", serde_json::json!("true"));
        // Mix bare and quoted in a compound expression
        assert!(evaluate_condition(
            r#"outcome=success && context.ready="true""#,
            &outcome,
            &context
        ));
    }
}
