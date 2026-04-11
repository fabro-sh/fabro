use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::types::{Message, ToolCall, ToolDefinition, ToolResult};

/// Context passed to tool execute handlers (Section 5.2).
#[derive(Clone)]
pub struct ToolContext {
    pub tool_call_id: String,
    pub messages:     Vec<Message>,
    pub abort_signal: Option<CancellationToken>,
}

/// An execute handler for a tool.
pub type ExecuteHandler = Arc<
    dyn Fn(
            serde_json::Value,
            ToolContext,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>
        + Send
        + Sync,
>;

/// A tool with an optional execute handler (Section 5.1, 5.5).
/// "Active" tools have an execute handler and are automatically executed.
/// "Passive" tools have no handler and are returned to the caller.
pub struct Tool {
    pub definition: ToolDefinition,
    pub execute:    Option<ExecuteHandler>,
}

impl Tool {
    /// Create a passive tool (no execute handler).
    ///
    /// # Panics
    ///
    /// Panics if the tool name is invalid (see [`validate_tool_name`]).
    #[must_use]
    pub fn passive(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        if let Err(e) = validate_tool_name(name) {
            panic!("Invalid tool name: {e}");
        }
        Self {
            definition: ToolDefinition {
                name: name.to_string(),
                description: description.to_string(),
                parameters,
            },
            execute:    None,
        }
    }

    /// Create an active tool with an execute handler.
    ///
    /// # Panics
    ///
    /// Panics if the tool name is invalid (see [`validate_tool_name`]).
    pub fn active<F, Fut>(
        name: &str,
        description: &str,
        parameters: serde_json::Value,
        handler: F,
    ) -> Self
    where
        F: Fn(serde_json::Value, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
    {
        if let Err(e) = validate_tool_name(name) {
            panic!("Invalid tool name: {e}");
        }
        Self {
            definition: ToolDefinition {
                name: name.to_string(),
                description: description.to_string(),
                parameters,
            },
            execute:    Some(Arc::new(move |args, ctx| Box::pin(handler(args, ctx)))),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.execute.is_some()
    }
}

/// Validate a tool name: [a-zA-Z][a-zA-Z0-9_]* max 64 chars (Section 5.1).
///
/// # Errors
///
/// Returns a description of the validation failure if the name is empty,
/// too long, starts with a non-letter, or contains invalid characters.
pub fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Tool name cannot be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!("Tool name '{name}' exceeds 64 character limit"));
    }
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if !first.is_ascii_alphabetic() {
            return Err(format!("Tool name '{name}' must start with a letter"));
        }
    }
    for ch in chars {
        if !ch.is_ascii_alphanumeric() && ch != '_' {
            return Err(format!(
                "Tool name '{name}' contains invalid character '{ch}'"
            ));
        }
    }
    Ok(())
}

/// A callback to repair invalid tool call arguments (Section 5.8).
/// Receives the tool call and the validation error message, returns repaired
/// arguments or an error if repair is not possible.
pub type RepairToolCallFn = Arc<
    dyn Fn(
            ToolCall,
            String,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>
        + Send
        + Sync,
>;

/// Validate tool call arguments against the tool's parameter schema.
/// Performs a lightweight structural check: verifies that when the schema
/// specifies `"type": "object"`, the arguments are a JSON object, and that
/// required properties are present.
fn validate_tool_args(args: &serde_json::Value, schema: &serde_json::Value) -> Result<(), String> {
    let schema_type = schema.get("type").and_then(serde_json::Value::as_str);
    if schema_type == Some("object") && !args.is_object() {
        return Err(format!(
            "Expected object arguments, got {}",
            args_type_name(args)
        ));
    }
    if let (Some(obj), Some(required)) = (
        args.as_object(),
        schema.get("required").and_then(serde_json::Value::as_array),
    ) {
        let missing: Vec<&str> = required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|key| !obj.contains_key(*key))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Missing required properties: {}",
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

const fn args_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Execute all tool calls with optional schema validation and repair (Section
/// 5.8).
///
/// Before calling a tool's execute handler, validates the arguments against the
/// tool's parameter schema. If validation fails and a `repair` callback is
/// provided, calls it to attempt repair. If repair succeeds, uses the repaired
/// arguments. If repair fails or is not configured, returns an error
/// `ToolResult`.
pub async fn execute_all_tools_with_repair(
    tools: &[&Tool],
    tool_calls: &[ToolCall],
    messages: &[Message],
    abort_signal: Option<&CancellationToken>,
    repair: Option<&RepairToolCallFn>,
) -> Vec<ToolResult> {
    use futures::future::join_all;

    let futures: Vec<_> = tool_calls
        .iter()
        .map(|call| {
            let tool = tools.iter().find(|t| t.definition.name == call.name).copied();
            let call_id = call.id.clone();
            let call_name = call.name.clone();
            let args = call.arguments.clone();
            let call_clone = call.clone();
            let ctx = ToolContext {
                tool_call_id: call_id.clone(),
                messages: messages.to_vec(),
                abort_signal: abort_signal.cloned(),
            };

            async move {
                let Some(t) = tool else {
                    return ToolResult::error(call_id, format!("Unknown tool: {call_name}"));
                };

                let Some(handler) = &t.execute else {
                    return ToolResult::error(call_id, format!("Unknown tool: {call_name}"));
                };

                let validated_args = match validate_tool_args(&args, &t.definition.parameters) {
                    Ok(()) => args,
                    Err(validation_error) => {
                        debug!(tool = %call_name, "Tool call validation failed");
                        if let Some(repair_fn) = repair {
                            match repair_fn(call_clone, validation_error).await {
                                Ok(repaired) => repaired,
                                Err(repair_error) => {
                                    warn!(tool = %call_name, "Tool call repair failed");
                                    return ToolResult::error(
                                        call_id,
                                        format!("Tool call validation failed and repair failed: {repair_error}"),
                                    );
                                }
                            }
                        } else {
                            return ToolResult::error(
                                call_id,
                                format!("Tool call validation failed: {validation_error}"),
                            );
                        }
                    }
                };

                match handler(validated_args, ctx).await {
                    Ok(result) => ToolResult::success(call_id, result),
                    Err(err_msg) => {
                        warn!(tool = %call_name, "Tool execution returned error");
                        ToolResult::error(call_id, err_msg)
                    }
                }
            }
        })
        .collect();

    join_all(futures).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_tool_name_valid() {
        assert!(validate_tool_name("get_weather").is_ok());
        assert!(validate_tool_name("a").is_ok());
        assert!(validate_tool_name("myTool123").is_ok());
        assert!(validate_tool_name("A_B_C").is_ok());
    }

    #[test]
    fn validate_tool_name_empty() {
        assert!(validate_tool_name("").is_err());
    }

    #[test]
    fn validate_tool_name_starts_with_number() {
        assert!(validate_tool_name("1tool").is_err());
    }

    #[test]
    fn validate_tool_name_starts_with_underscore() {
        assert!(validate_tool_name("_tool").is_err());
    }

    #[test]
    fn validate_tool_name_contains_dash() {
        assert!(validate_tool_name("my-tool").is_err());
    }

    #[test]
    fn validate_tool_name_too_long() {
        let name = "a".repeat(65);
        assert!(validate_tool_name(&name).is_err());
    }

    #[test]
    fn validate_tool_name_max_length_ok() {
        let name = "a".repeat(64);
        assert!(validate_tool_name(&name).is_ok());
    }

    #[test]
    fn passive_tool_is_not_active() {
        let tool = Tool::passive(
            "test",
            "test tool",
            serde_json::json!({"type": "object", "properties": {}}),
        );
        assert!(!tool.is_active());
    }

    #[test]
    fn active_tool_is_active() {
        let tool = Tool::active(
            "test",
            "test tool",
            serde_json::json!({"type": "object", "properties": {}}),
            |_args, _ctx| async { Ok(serde_json::json!("result")) },
        );
        assert!(tool.is_active());
    }

    #[test]
    #[should_panic(expected = "Invalid tool name")]
    fn passive_tool_panics_on_invalid_name() {
        let _ = Tool::passive(
            "1invalid",
            "bad name",
            serde_json::json!({"type": "object"}),
        );
    }

    #[test]
    #[should_panic(expected = "Invalid tool name")]
    fn active_tool_panics_on_invalid_name() {
        Tool::active(
            "my-tool",
            "bad name",
            serde_json::json!({"type": "object"}),
            |_args, _ctx| async { Ok(serde_json::json!("result")) },
        );
    }

    #[test]
    fn validate_tool_args_valid_object() {
        let schema =
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}});
        let args = serde_json::json!({"name": "Alice"});
        assert!(validate_tool_args(&args, &schema).is_ok());
    }

    #[test]
    fn validate_tool_args_non_object_when_object_expected() {
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let args = serde_json::json!("not an object");
        let result = validate_tool_args(&args, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected object"));
    }

    #[test]
    fn validate_tool_args_missing_required_properties() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "number"}},
            "required": ["name", "age"]
        });
        let args = serde_json::json!({"name": "Alice"});
        let result = validate_tool_args(&args, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("age"));
    }

    #[test]
    fn validate_tool_args_no_schema_type_passes() {
        let schema = serde_json::json!({});
        let args = serde_json::json!("anything");
        assert!(validate_tool_args(&args, &schema).is_ok());
    }

    #[tokio::test]
    async fn execute_with_repair_valid_args_no_repair_needed() {
        let tools = [Tool::active(
            "greet",
            "Greet someone",
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            |args, _ctx| async move {
                let name = args["name"].as_str().unwrap_or("world");
                Ok(serde_json::json!(format!("Hello, {}!", name)))
            },
        )];
        let calls = vec![ToolCall::new(
            "call_1",
            "greet",
            serde_json::json!({"name": "Alice"}),
        )];
        let tool_refs: Vec<&Tool> = tools.iter().collect();

        let results = execute_all_tools_with_repair(&tool_refs, &calls, &[], None, None).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);
        assert_eq!(results[0].content, serde_json::json!("Hello, Alice!"));
    }

    #[tokio::test]
    async fn execute_with_repair_invalid_args_no_repair_fn() {
        let tools = [Tool::active(
            "greet",
            "Greet someone",
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
            |args, _ctx| async move {
                let name = args["name"].as_str().unwrap_or("world");
                Ok(serde_json::json!(format!("Hello, {}!", name)))
            },
        )];
        let calls = vec![ToolCall::new("call_1", "greet", serde_json::json!({}))];
        let tool_refs: Vec<&Tool> = tools.iter().collect();

        let results = execute_all_tools_with_repair(&tool_refs, &calls, &[], None, None).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(
            results[0]
                .content
                .as_str()
                .unwrap()
                .contains("validation failed")
        );
    }

    #[tokio::test]
    async fn execute_with_repair_invalid_args_repair_succeeds() {
        let tools = [Tool::active(
            "greet",
            "Greet someone",
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
            |args, _ctx| async move {
                let name = args["name"].as_str().unwrap_or("world");
                Ok(serde_json::json!(format!("Hello, {}!", name)))
            },
        )];
        let calls = vec![ToolCall::new("call_1", "greet", serde_json::json!({}))];
        let tool_refs: Vec<&Tool> = tools.iter().collect();

        let repair: RepairToolCallFn = Arc::new(|_call, _error| {
            Box::pin(async { Ok(serde_json::json!({"name": "Repaired"})) })
        });
        let results =
            execute_all_tools_with_repair(&tool_refs, &calls, &[], None, Some(&repair)).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);
        assert_eq!(results[0].content, serde_json::json!("Hello, Repaired!"));
    }

    #[tokio::test]
    async fn execute_with_repair_invalid_args_repair_fails() {
        let tools = [Tool::active(
            "greet",
            "Greet someone",
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
            |args, _ctx| async move {
                let name = args["name"].as_str().unwrap_or("world");
                Ok(serde_json::json!(format!("Hello, {}!", name)))
            },
        )];
        let calls = vec![ToolCall::new("call_1", "greet", serde_json::json!({}))];
        let tool_refs: Vec<&Tool> = tools.iter().collect();

        let repair: RepairToolCallFn =
            Arc::new(|_call, _error| Box::pin(async { Err("cannot repair".to_string()) }));
        let results =
            execute_all_tools_with_repair(&tool_refs, &calls, &[], None, Some(&repair)).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(
            results[0]
                .content
                .as_str()
                .unwrap()
                .contains("repair failed")
        );
    }

    // --- args_type_name ---

    #[test]
    fn args_type_name_null() {
        assert_eq!(args_type_name(&serde_json::Value::Null), "null");
    }

    #[test]
    fn args_type_name_bool() {
        assert_eq!(args_type_name(&serde_json::json!(true)), "boolean");
    }

    #[test]
    fn args_type_name_number() {
        assert_eq!(args_type_name(&serde_json::json!(42)), "number");
    }

    #[test]
    fn args_type_name_string() {
        assert_eq!(args_type_name(&serde_json::json!("hello")), "string");
    }

    #[test]
    fn args_type_name_array() {
        assert_eq!(args_type_name(&serde_json::json!([1, 2])), "array");
    }

    #[test]
    fn args_type_name_object() {
        assert_eq!(args_type_name(&serde_json::json!({})), "object");
    }
}
