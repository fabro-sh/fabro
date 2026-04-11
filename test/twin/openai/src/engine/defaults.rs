use serde_json::{Value, json};

use super::plan::ResponsePlan;
use crate::openai::models::{ResponseFormat, ResponsesRequest, normalize_whitespace};

pub fn build_default_response_plan(
    response_number: u64,
    request: &ResponsesRequest,
) -> ResponsePlan {
    let normalized_text = request.extract_user_text();
    let response_text = format!("deterministic: {normalized_text}");
    let input_tokens = normalized_text.split_whitespace().count() as u64;

    let structured_output = request.response_format().and_then(|format| match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({
            "message": response_text,
            "model": request.model,
        })),
        ResponseFormat::JsonSchema(schema) => {
            Some(generate_json_from_schema(&schema, &response_text))
        }
    });
    let reasoning = if request.reasoning.is_some() {
        vec![format!("reasoning: {normalized_text}")]
    } else {
        Vec::new()
    };

    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model: request.model.clone(),
        response_text,
        structured_output,
        reasoning,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens: 5,
    }
}

pub fn build_default_chat_plan(
    response_number: u64,
    model: String,
    input_text: &str,
    response_format: Option<ResponseFormat>,
    reasoning_requested: bool,
) -> ResponsePlan {
    let normalized_text = normalize_whitespace(input_text);
    let response_text = format!("deterministic: {normalized_text}");
    let structured_output = response_format.and_then(|format| match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({
            "message": response_text,
            "model": model,
        })),
        ResponseFormat::JsonSchema(schema) => {
            Some(generate_json_from_schema(&schema, &response_text))
        }
    });
    let reasoning = if reasoning_requested {
        vec![format!("reasoning: {normalized_text}")]
    } else {
        Vec::new()
    };
    let input_tokens = normalized_text.split_whitespace().count() as u64;

    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model,
        response_text,
        structured_output,
        reasoning,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens: 5,
    }
}

fn generate_json_from_schema(schema: &Value, response_text: &str) -> Value {
    let schema = schema.get("schema").unwrap_or(schema);

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();

            let mut object = serde_json::Map::new();
            for (name, property_schema) in properties {
                object.insert(
                    name,
                    primitive_value_for_schema(&property_schema, response_text),
                );
            }
            Value::Object(object)
        }
        _ => json!({ "message": response_text }),
    }
}

fn primitive_value_for_schema(schema: &Value, response_text: &str) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => Value::String(response_text.to_owned()),
        Some("integer") => json!(1),
        Some("number") => json!(1.0),
        Some("boolean") => json!(true),
        Some("object") => generate_json_from_schema(schema, response_text),
        _ => Value::Null,
    }
}
