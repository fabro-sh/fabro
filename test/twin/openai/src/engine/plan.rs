use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ResponsePlan {
    pub id:                String,
    pub created:           u64,
    pub model:             String,
    pub response_text:     String,
    pub structured_output: Option<Value>,
    pub reasoning:         Vec<String>,
    pub tool_calls:        Vec<ToolCallPlan>,
    pub input_tokens:      u64,
    pub output_tokens:     u64,
}

#[derive(Clone, Debug)]
pub struct ToolCallPlan {
    pub id:            String,
    pub name:          String,
    pub arguments:     Value,
    pub raw_arguments: Option<String>,
}

impl ResponsePlan {
    pub fn tool_call_arguments_text(tool_call: &ToolCallPlan) -> String {
        tool_call
            .raw_arguments
            .clone()
            .unwrap_or_else(|| tool_call.arguments.to_string())
    }

    fn responses_tool_call_item(tool_call: &ToolCallPlan) -> Value {
        json!({
            "id": format!("fc_{}", tool_call.id),
            "type": "function_call",
            "call_id": tool_call.id,
            "name": tool_call.name,
            "arguments": Self::tool_call_arguments_text(tool_call),
        })
    }

    pub fn chat_content(&self) -> String {
        self.structured_output
            .as_ref()
            .map_or_else(|| self.response_text.clone(), ToString::to_string)
    }

    pub fn responses_json(&self) -> Value {
        let mut content_items = Vec::new();

        if !self.response_text.is_empty() {
            content_items.push(json!({
                "type": "output_text",
                "text": self.response_text,
            }));
        }

        if let Some(structured_output) = &self.structured_output {
            content_items.push(json!({
                "type": "output_json",
                "json": structured_output,
            }));
        }

        let mut output = Vec::new();

        if !content_items.is_empty() {
            output.push(json!({
                "id": format!("msg_{}", self.id),
                "type": "message",
                "role": "assistant",
                "content": content_items,
            }));
        }

        for tool_call in &self.tool_calls {
            output.push(Self::responses_tool_call_item(tool_call));
        }

        json!({
            "id": self.id,
            "object": "response",
            "created": self.created,
            "model": self.model,
            "status": "completed",
            "reasoning": self.reasoning,
            "output": output,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
            }
        })
    }

    pub fn chat_completions_json(&self) -> Value {
        json!({
            "id": format!("chatcmpl_{}", self.id),
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "finish_reason": if self.tool_calls.is_empty() { "stop" } else { "tool_calls" },
                "message": {
                    "role": "assistant",
                    "content": self.chat_content(),
                    "reasoning": self.reasoning,
                    "tool_calls": self.tool_calls.iter().map(|tool_call| json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": Self::tool_call_arguments_text(tool_call),
                        }
                    })).collect::<Vec<_>>(),
                }
            }],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
            }
        })
    }
}
