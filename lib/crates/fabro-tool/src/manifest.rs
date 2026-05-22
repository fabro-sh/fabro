use serde_json::Value;

use super::common::{ToolError, ToolResult};

pub fn json_to_toml_value(key: &str, value: &Value) -> ToolResult<toml::Value> {
    match value {
        Value::Null => Err(ToolError::message(format!(
            "input `{key}` cannot be null; use a string, boolean, or number"
        ))),
        Value::Bool(value) => Ok(toml::Value::Boolean(*value)),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Ok(toml::Value::Integer(integer))
            } else if let Some(float) = value.as_f64() {
                Ok(toml::Value::Float(float))
            } else {
                Err(ToolError::message(format!(
                    "input `{key}` contains a number outside TOML's supported range"
                )))
            }
        }
        Value::String(value) => Ok(toml::Value::String(value.clone())),
        Value::Array(_) => Err(ToolError::message(format!(
            "input `{key}` does not support array values; use a string, boolean, or number",
        ))),
        Value::Object(_) => Err(ToolError::message(format!(
            "input `{key}` does not support object values; use a string, boolean, or number",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn json_inputs_convert_scalar_values_to_toml_values() {
        let cases = [
            (json!("hello"), toml::Value::String("hello".to_string())),
            (json!(true), toml::Value::Boolean(true)),
            (json!(42), toml::Value::Integer(42)),
            (json!(0.5), toml::Value::Float(0.5)),
        ];

        for (json, expected) in cases {
            assert_eq!(json_to_toml_value("input", &json).unwrap(), expected);
        }
    }

    #[test]
    fn json_input_arrays_and_objects_are_rejected() {
        let array_err = json_to_toml_value("matrix", &json!(["a", 1])).unwrap_err();
        assert_eq!(
            array_err.as_str(),
            "input `matrix` does not support array values; use a string, boolean, or number",
        );

        let object_err = json_to_toml_value("settings", &json!({ "enabled": true })).unwrap_err();
        assert_eq!(
            object_err.as_str(),
            "input `settings` does not support object values; use a string, boolean, or number",
        );
    }

    #[test]
    fn json_input_null_is_rejected_with_key_name() {
        let err = json_to_toml_value("goal", &Value::Null).unwrap_err();

        assert_eq!(
            err.as_str(),
            "input `goal` cannot be null; use a string, boolean, or number",
        );
    }
}
