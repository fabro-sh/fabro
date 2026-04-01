use axum::{Json, extract::rejection::JsonRejection, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Accepts all known OpenAI Responses API fields. Unknown fields are ignored
/// via `#[serde(flatten)]` so the twin stays compatible as the API evolves.
#[derive(Clone, Debug, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: ResponseInput,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub stop: Option<Value>,
    pub previous_response_id: Option<String>,
    pub reasoning: Option<Value>,
    pub text: Option<TextConfig>,
    pub tools: Option<Vec<Value>>,
    pub tool_choice: Option<Value>,
    /// Catch-all for fields the twin doesn't use (temperature, top_p, etc.)
    #[allow(dead_code)]
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl ResponsesRequest {
    pub fn extract_user_text(&self) -> String {
        let text = self.input.extract_text();
        if text.is_empty() {
            "empty input".to_owned()
        } else {
            text
        }
    }

    pub fn response_format(&self) -> Option<ResponseFormat> {
        let format = self.text.as_ref()?.format.as_ref()?;
        response_format_from_kind(
            "text.format.type",
            &format.kind,
            format.json_schema_payload(),
        )
        .ok()
    }

    pub fn tool_choice_mode(&self) -> Option<ToolChoiceMode> {
        tool_choice_mode(self.tool_choice.as_ref(), ToolSurface::Responses)
    }

    pub fn validate(&self) -> Result<(), OpenAiError> {
        if self.model.trim().is_empty() {
            return Err(OpenAiError::invalid_request(
                "model",
                "model must not be empty",
            ));
        }

        if let Some(text) = &self.text {
            let Some(format) = &text.format else {
                return Err(OpenAiError::invalid_request(
                    "text.format",
                    "text.format must be present when text is provided",
                ));
            };

            if let ResponseFormat::JsonSchema(schema) = response_format_from_kind(
                "text.format.type",
                &format.kind,
                format.json_schema_payload(),
            )? {
                validate_json_schema_subset(&schema)?;
            }
        }

        if let Some(ResponseFormat::JsonSchema(schema)) = self.response_format() {
            validate_json_schema_subset(&schema)?;
        }

        validate_tools(self.tools.as_ref(), "tools", ToolSurface::Responses)?;
        validate_tool_choice(
            self.tool_choice.as_ref(),
            "tool_choice",
            ToolSurface::Responses,
        )?;
        validate_tool_choice_requires_tools(self.tool_choice.as_ref(), self.tools.as_ref())?;
        validate_stop(self.stop.as_ref(), "stop")?;
        validate_response_input(&self.input)?;

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema(Value),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolSurface {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
    Items(Vec<InputItem>),
    #[default]
    Empty,
}

impl ResponseInput {
    fn extract_text(&self) -> String {
        match self {
            Self::Text(text) => normalize_whitespace(text),
            Self::Items(items) => {
                let pieces: Vec<String> = items
                    .iter()
                    .flat_map(InputItem::extract_texts_for_fallback)
                    .collect();
                normalize_whitespace(&pieces.join(" "))
            }
            Self::Empty => String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputItem {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: InputContent,
    #[serde(default)]
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
}

impl InputItem {
    fn extract_texts_for_fallback(&self) -> Vec<String> {
        if self.item_type.as_deref() == Some("function_call_output") {
            return self
                .output
                .as_ref()
                .map(|output| vec![normalize_whitespace(output)])
                .unwrap_or_default();
        }

        if self.role.as_deref() == Some("user") {
            return self.content.extract_texts();
        }

        Vec::new()
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(untagged)]
pub enum InputContent {
    Text(String),
    Parts(Vec<ContentPart>),
    #[default]
    Empty,
}

impl InputContent {
    fn extract_texts(&self) -> Vec<String> {
        match self {
            Self::Text(text) => vec![normalize_whitespace(text)],
            Self::Parts(parts) => parts.iter().filter_map(ContentPart::text_value).collect(),
            Self::Empty => Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Parts(parts) => parts.is_empty(),
            Self::Empty => true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
}

impl ContentPart {
    fn text_value(&self) -> Option<String> {
        match self.kind.as_str() {
            "input_text" | "text" => self.text.as_ref().map(|text| normalize_whitespace(text)),
            _ => None,
        }
    }
}

fn validate_response_input(input: &ResponseInput) -> Result<(), OpenAiError> {
    let ResponseInput::Items(items) = input else {
        return Ok(());
    };

    for item in items {
        validate_input_item(item)?;
    }

    Ok(())
}

fn validate_input_item(item: &InputItem) -> Result<(), OpenAiError> {
    match item.item_type.as_deref() {
        Some("function_call_output") => validate_function_call_output_item(item),
        None => validate_message_input_item(item),
        // Accept any other item type — the twin extracts user text for fallback
        // responses and ignores items it doesn't understand.
        Some(_) => Ok(()),
    }
}

fn validate_function_call_output_item(item: &InputItem) -> Result<(), OpenAiError> {
    if item.role.is_some() || !item.content.is_empty() {
        return Err(OpenAiError::invalid_request(
            "input",
            "function_call_output items may not include role or content",
        ));
    }

    if item.call_id.as_deref().is_none_or(str::is_empty) {
        return Err(OpenAiError::invalid_request(
            "input",
            "function_call_output items require call_id",
        ));
    }

    if item.output.as_deref().is_none_or(str::is_empty) {
        return Err(OpenAiError::invalid_request(
            "input",
            "function_call_output items require output",
        ));
    }

    Ok(())
}

fn validate_message_input_item(item: &InputItem) -> Result<(), OpenAiError> {
    if item.role.as_deref().is_none_or(str::is_empty) {
        return Err(OpenAiError::invalid_request(
            "input",
            "message input items require role",
        ));
    }

    if item.output.is_some() || item.call_id.is_some() {
        return Err(OpenAiError::invalid_request(
            "input",
            "message input items may not include function_call_output fields",
        ));
    }

    validate_input_content(&item.content)
}

fn validate_input_content(content: &InputContent) -> Result<(), OpenAiError> {
    match content {
        InputContent::Text(_) => Ok(()),
        InputContent::Parts(parts) if !parts.is_empty() => {
            for part in parts {
                validate_input_content_part(part)?;
            }
            Ok(())
        }
        _ => Err(OpenAiError::invalid_request(
            "input",
            "message input items require supported content",
        )),
    }
}

fn validate_input_content_part(part: &ContentPart) -> Result<(), OpenAiError> {
    match part.kind.as_str() {
        "input_text" | "text" if part.text.as_deref().is_some() => Ok(()),
        "input_image"
            if part
                .image_url
                .as_deref()
                .is_some_and(is_supported_image_reference) =>
        {
            Ok(())
        }
        "input_text" | "text" => Err(OpenAiError::invalid_request(
            "input",
            "text input parts require text",
        )),
        "input_image" => Err(OpenAiError::invalid_request(
            "input",
            "image input parts require a supported image_url",
        )),
        _ => Err(OpenAiError::invalid_request(
            "input",
            "unsupported input content part type",
        )),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextConfig {
    pub format: Option<TextFormat>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextFormat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub json_schema: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub strict: Option<bool>,
}

impl TextFormat {
    fn json_schema_payload(&self) -> Option<Value> {
        self.json_schema.clone().or_else(|| {
            json_schema_config_value(
                self.name.as_ref(),
                self.schema.as_ref(),
                self.description.as_ref(),
                self.strict,
            )
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub param: Value,
    pub code: String,
}

#[derive(Clone, Debug)]
pub struct OpenAiError {
    pub status: StatusCode,
    pub body: ErrorEnvelope,
}

impl OpenAiError {
    pub fn invalid_request(param: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ErrorEnvelope {
                error: ErrorBody {
                    message: message.to_owned(),
                    error_type: "invalid_request_error".to_owned(),
                    param: Value::String(param.to_owned()),
                    code: "invalid_request".to_owned(),
                },
            },
        }
    }

    pub fn into_response(self) -> (StatusCode, Json<ErrorEnvelope>) {
        (self.status, Json(self.body))
    }

    pub fn from_json_rejection(rejection: &JsonRejection) -> Self {
        Self::invalid_request("body", &rejection.body_text())
    }
}

fn validate_json_schema_subset(schema: &Value) -> Result<(), OpenAiError> {
    let schema = schema.get("schema").unwrap_or(schema);
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return Err(OpenAiError::invalid_request(
                    "text.format.json_schema",
                    "json_schema object types must define properties",
                ));
            };

            for property in properties.values() {
                validate_schema_node(property)?;
            }
            Ok(())
        }
        _ => Err(OpenAiError::invalid_request(
            "text.format.json_schema",
            "unsupported json_schema root type",
        )),
    }
}

fn validate_schema_node(node: &Value) -> Result<(), OpenAiError> {
    if node.get("items").is_some() || node.get("anyOf").is_some() || node.get("oneOf").is_some() {
        return Err(OpenAiError::invalid_request(
            "text.format.json_schema",
            "unsupported json_schema construct",
        ));
    }

    match node.get("type").and_then(Value::as_str) {
        Some("string" | "number" | "integer" | "boolean") => Ok(()),
        Some("object") => validate_json_schema_subset(node),
        _ => Err(OpenAiError::invalid_request(
            "text.format.json_schema",
            "unsupported json_schema property type",
        )),
    }
}

pub fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    pub tools: Option<Vec<Value>>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<ChatResponseFormat>,
    pub stop: Option<Value>,
}

impl ChatCompletionsRequest {
    pub fn extract_user_text(&self) -> String {
        let pieces: Vec<String> = self
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .flat_map(ChatMessage::extract_texts)
            .collect();
        let text = normalize_whitespace(&pieces.join(" "));
        if text.is_empty() {
            "empty input".to_owned()
        } else {
            text
        }
    }

    pub fn response_format(&self) -> Option<ResponseFormat> {
        let format = self.response_format.as_ref()?;
        response_format_from_kind(
            "response_format.type",
            &format.kind,
            format.json_schema_payload(),
        )
        .ok()
    }

    pub fn reasoning_requested(&self) -> bool {
        self.messages
            .iter()
            .any(ChatMessage::contains_reasoning_content)
    }

    pub fn tool_choice_mode(&self) -> Option<ToolChoiceMode> {
        tool_choice_mode(self.tool_choice.as_ref(), ToolSurface::ChatCompletions)
    }

    pub fn validate(&self) -> Result<(), OpenAiError> {
        if self.model.trim().is_empty() {
            return Err(OpenAiError::invalid_request(
                "model",
                "model must not be empty",
            ));
        }

        if let Some(format) = &self.response_format {
            if let ResponseFormat::JsonSchema(schema) = response_format_from_kind(
                "response_format.type",
                &format.kind,
                format.json_schema_payload(),
            )? {
                validate_json_schema_subset(&schema)?;
            }
        }

        if let Some(ResponseFormat::JsonSchema(schema)) = self.response_format() {
            validate_json_schema_subset(&schema)?;
        }

        validate_tools(self.tools.as_ref(), "tools", ToolSurface::ChatCompletions)?;
        validate_tool_choice(
            self.tool_choice.as_ref(),
            "tool_choice",
            ToolSurface::ChatCompletions,
        )?;
        validate_tool_choice_requires_tools(self.tool_choice.as_ref(), self.tools.as_ref())?;
        validate_stop(self.stop.as_ref(), "stop")?;
        validate_chat_messages(&self.messages)?;

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
}

impl ChatMessage {
    fn extract_texts(&self) -> Vec<String> {
        match &self.content {
            Value::String(text) => vec![normalize_whitespace(text)],
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .map(normalize_whitespace)
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn contains_reasoning_content(&self) -> bool {
        self.role == "assistant"
            && self.content.as_array().is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind == "reasoning")
                })
            })
    }
}

fn validate_chat_messages(messages: &[ChatMessage]) -> Result<(), OpenAiError> {
    if messages.is_empty() {
        return Err(OpenAiError::invalid_request(
            "messages",
            "messages must not be empty",
        ));
    }

    for message in messages {
        validate_chat_message(message)?;
    }

    Ok(())
}

fn validate_chat_message(message: &ChatMessage) -> Result<(), OpenAiError> {
    if message.role.trim().is_empty() {
        return Err(OpenAiError::invalid_request(
            "messages",
            "message role must not be empty",
        ));
    }

    match &message.content {
        Value::String(_) => Ok(()),
        Value::Array(parts) if !parts.is_empty() => {
            for part in parts {
                validate_chat_message_part(part, &message.role)?;
            }
            Ok(())
        }
        _ => Err(OpenAiError::invalid_request(
            "messages",
            "unsupported message content shape",
        )),
    }
}

fn validate_chat_message_part(part: &Value, role: &str) -> Result<(), OpenAiError> {
    let Some(object) = part.as_object() else {
        return Err(OpenAiError::invalid_request(
            "messages",
            "message content parts must be objects",
        ));
    };

    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Err(OpenAiError::invalid_request(
            "messages",
            "message content part type is required",
        ));
    };

    match kind {
        "text" => {
            if object.get("text").and_then(Value::as_str).is_none() {
                return Err(OpenAiError::invalid_request(
                    "messages",
                    "text-bearing message content parts require text",
                ));
            }
            Ok(())
        }
        "reasoning" => {
            if role != "assistant" {
                return Err(OpenAiError::invalid_request(
                    "messages",
                    "reasoning content parts are only supported on assistant messages",
                ));
            }
            if object.get("text").and_then(Value::as_str).is_none() {
                return Err(OpenAiError::invalid_request(
                    "messages",
                    "text-bearing message content parts require text",
                ));
            }
            Ok(())
        }
        "image_url" => {
            if !object
                .get("image_url")
                .is_some_and(is_valid_chat_image_reference)
            {
                return Err(OpenAiError::invalid_request(
                    "messages",
                    "image_url parts require a supported image_url object",
                ));
            }
            Ok(())
        }
        _ => Err(OpenAiError::invalid_request(
            "messages",
            "unsupported message content part type",
        )),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub json_schema: Option<Value>,
}

impl ChatResponseFormat {
    fn json_schema_payload(&self) -> Option<Value> {
        self.json_schema.clone().or_else(|| self.schema.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolChoiceMode {
    Auto,
    NoTool,
    Required,
    Function(String),
}

fn response_format_from_kind(
    param: &str,
    kind: &str,
    schema: Option<Value>,
) -> Result<ResponseFormat, OpenAiError> {
    match kind {
        "text" => Ok(ResponseFormat::Text),
        "json_object" => Ok(ResponseFormat::JsonObject),
        "json_schema" => schema
            .map(ResponseFormat::JsonSchema)
            .ok_or_else(|| OpenAiError::invalid_request(param, "json_schema requires schema")),
        _ => Err(OpenAiError::invalid_request(
            param,
            "unsupported response format type",
        )),
    }
}

fn validate_tools(
    tools: Option<&Vec<Value>>,
    param: &str,
    surface: ToolSurface,
) -> Result<(), OpenAiError> {
    let Some(tools) = tools else {
        return Ok(());
    };

    for tool in tools {
        let Some(tool_type) = tool.get("type").and_then(Value::as_str) else {
            return Err(OpenAiError::invalid_request(param, "tool type is required"));
        };

        if tool_type != "function" {
            return Err(OpenAiError::invalid_request(
                param,
                "only function tools are supported",
            ));
        }

        if function_tool_name(tool, surface).is_none() {
            return Err(OpenAiError::invalid_request(
                param,
                "function tool name is required",
            ));
        }
    }

    Ok(())
}

fn validate_tool_choice(
    tool_choice: Option<&Value>,
    param: &str,
    surface: ToolSurface,
) -> Result<(), OpenAiError> {
    let Some(tool_choice) = tool_choice else {
        return Ok(());
    };

    match tool_choice {
        Value::String(value) if matches!(value.as_str(), "auto" | "none" | "required") => Ok(()),
        Value::Object(object)
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "function")
                && function_tool_choice_name(tool_choice, surface).is_some() =>
        {
            Ok(())
        }
        _ => Err(OpenAiError::invalid_request(
            param,
            "unsupported tool_choice shape",
        )),
    }
}

fn validate_tool_choice_requires_tools(
    tool_choice: Option<&Value>,
    tools: Option<&Vec<Value>>,
) -> Result<(), OpenAiError> {
    let Some(tool_choice) = tool_choice else {
        return Ok(());
    };

    let requires_tools = match tool_choice {
        Value::String(value) => value == "required",
        Value::Object(_) => true,
        _ => false,
    };

    if requires_tools && tools.is_none_or(Vec::is_empty) {
        return Err(OpenAiError::invalid_request(
            "tool_choice",
            "tool_choice requires tools to be provided",
        ));
    }

    Ok(())
}

fn validate_stop(stop: Option<&Value>, param: &str) -> Result<(), OpenAiError> {
    let Some(stop) = stop else {
        return Ok(());
    };

    match stop {
        Value::String(_) => Ok(()),
        Value::Array(values) if values.iter().all(Value::is_string) => Ok(()),
        _ => Err(OpenAiError::invalid_request(
            param,
            "stop must be a string or array of strings",
        )),
    }
}

fn tool_choice_mode(tool_choice: Option<&Value>, surface: ToolSurface) -> Option<ToolChoiceMode> {
    match tool_choice? {
        Value::String(value) => match value.as_str() {
            "auto" => Some(ToolChoiceMode::Auto),
            "none" => Some(ToolChoiceMode::NoTool),
            "required" => Some(ToolChoiceMode::Required),
            _ => None,
        },
        Value::Object(_) => function_tool_choice_name(tool_choice?, surface)
            .map(|name| ToolChoiceMode::Function(name.to_owned())),
        _ => None,
    }
}

fn function_tool_name(tool: &Value, surface: ToolSurface) -> Option<&str> {
    if surface == ToolSurface::Responses {
        if let Some(name) = tool.get("name").and_then(Value::as_str) {
            return Some(name);
        }
    }

    tool.get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
}

fn function_tool_choice_name(tool_choice: &Value, surface: ToolSurface) -> Option<&str> {
    if surface == ToolSurface::Responses {
        if let Some(name) = tool_choice.get("name").and_then(Value::as_str) {
            return Some(name);
        }
    }

    tool_choice
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
}

fn json_schema_config_value(
    name: Option<&String>,
    schema: Option<&Value>,
    description: Option<&String>,
    strict: Option<bool>,
) -> Option<Value> {
    if name.is_none() && schema.is_none() && description.is_none() && strict.is_none() {
        return None;
    }

    let mut object = Map::new();
    if let Some(name) = name {
        object.insert("name".to_owned(), Value::String(name.clone()));
    }
    if let Some(schema) = schema {
        object.insert("schema".to_owned(), schema.clone());
    }
    if let Some(description) = description {
        object.insert("description".to_owned(), Value::String(description.clone()));
    }
    if let Some(strict) = strict {
        object.insert("strict".to_owned(), Value::Bool(strict));
    }
    Some(Value::Object(object))
}

fn is_supported_image_reference(image_url: &str) -> bool {
    !image_url.trim().is_empty()
        && (image_url.starts_with("http://")
            || image_url.starts_with("https://")
            || image_url.starts_with("data:"))
}

fn is_valid_chat_image_reference(image_url: &Value) -> bool {
    let Some(object) = image_url.as_object() else {
        return false;
    };

    let Some(url) = object.get("url").and_then(Value::as_str) else {
        return false;
    };

    if !is_supported_image_reference(url) {
        return false;
    }

    object.get("detail").is_none_or(Value::is_string)
}
