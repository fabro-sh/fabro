/// Condition expression evaluator for edge guards (spec Section 10).
///
/// Grammar: `ConditionExpr ::= Clause ('&&' Clause)*`, `Clause ::= Key Op Literal`,
/// `Op ::= '=' | '!='`.
use crate::context::Context;
use crate::error::AttractorError;
use crate::outcome::Outcome;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Clause {
    key: String,
    op: Op,
    value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Eq,
    NotEq,
    Truthy,
}

fn parse_clauses(expr: &str) -> Result<Vec<Clause>, AttractorError> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Ok(Vec::new());
    }

    expr.split("&&")
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let part = part.trim();
            if let Some(pos) = part.find("!=") {
                let key = part[..pos].trim().to_string();
                let value = part[pos + 2..].trim().to_string();
                if key.is_empty() {
                    return Err(AttractorError::Parse(format!(
                        "empty key in condition clause: {part:?}"
                    )));
                }
                Ok(Clause {
                    key,
                    op: Op::NotEq,
                    value,
                })
            } else if let Some(pos) = part.find('=') {
                let key = part[..pos].trim().to_string();
                let value = part[pos + 1..].trim().to_string();
                if key.is_empty() {
                    return Err(AttractorError::Parse(format!(
                        "empty key in condition clause: {part:?}"
                    )));
                }
                Ok(Clause {
                    key,
                    op: Op::Eq,
                    value,
                })
            } else {
                // Bare key: truthiness check
                let key = part.to_string();
                if key.is_empty() {
                    return Err(AttractorError::Parse(format!(
                        "empty key in condition clause: {part:?}"
                    )));
                }
                Ok(Clause {
                    key,
                    op: Op::Truthy,
                    value: String::new(),
                })
            }
        })
        .collect()
}

/// Parse and validate a condition expression.
///
/// # Errors
///
/// Returns an error if the expression contains invalid syntax.
pub fn parse_condition(expr: &str) -> Result<(), AttractorError> {
    parse_clauses(expr)?;
    Ok(())
}

fn resolve_key(key: &str, outcome: &Outcome, context: &Context) -> String {
    if key == "outcome" {
        return outcome.status.to_string();
    }
    if key == "preferred_label" {
        return outcome
            .preferred_label
            .as_deref()
            .unwrap_or("")
            .to_string();
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

fn json_value_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Evaluate a condition expression against an outcome and context.
/// Empty conditions always return true.
#[must_use]
pub fn evaluate_condition(expr: &str, outcome: &Outcome, context: &Context) -> bool {
    let Ok(clauses) = parse_clauses(expr) else {
        return false;
    };

    if clauses.is_empty() {
        return true;
    }

    clauses.iter().all(|clause| {
        let resolved = resolve_key(&clause.key, outcome, context);
        match clause.op {
            Op::Eq => resolved == clause.value,
            Op::NotEq => resolved != clause.value,
            Op::Truthy => !resolved.is_empty() && resolved != "false" && resolved != "0",
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::StageStatus;

    fn make_outcome(status: StageStatus) -> Outcome {
        Outcome {
            status,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: std::collections::HashMap::new(),
            notes: None,
            failure_reason: None,
            usage: None,
            files_touched: Vec::new(),
        }
    }

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
        assert!(!evaluate_condition(
            "outcome!=success",
            &outcome,
            &context
        ));
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
    fn parse_condition_validates() {
        assert!(parse_condition("outcome=success").is_ok());
        assert!(parse_condition("outcome=success && context.x=y").is_ok());
        assert!(parse_condition("").is_ok());
    }

    #[test]
    fn parse_condition_accepts_bare_key() {
        assert!(parse_condition("some_flag").is_ok());
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
}
