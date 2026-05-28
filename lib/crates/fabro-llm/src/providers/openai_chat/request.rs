//! Build a Chat Completions request body from a unified
//! [`Request`](crate::types::Request).

use fabro_model::Catalog;

use super::hooks::ChatHooks;
use super::translate::{
    translate_messages, translate_response_format, translate_tool_choice, translate_tools,
};
use super::wire::ApiRequest;
use crate::providers::common::api_model_id;
use crate::types::Request;

/// Build the API request body from a unified `Request`.
///
/// Returns a `serde_json::Value` so that `provider_options.<provider_name>`
/// fields can be merged into the request before sending, and so that
/// [`ChatHooks::mutate_request`] can apply provider-specific shape
/// translations.
pub(crate) fn build_chat_request_with_catalog(
    request: &Request,
    stream: Option<bool>,
    provider_name: &str,
    catalog: Option<&Catalog>,
    hooks: ChatHooks,
) -> serde_json::Value {
    let chat_messages = translate_messages(&request.messages);
    let tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_choice = request.tool_choice.as_ref().map(translate_tool_choice);
    let response_format = request
        .response_format
        .as_ref()
        .map(translate_response_format);

    let api_request = ApiRequest {
        model: api_model_id(catalog, &request.model),
        messages: chat_messages,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        top_p: request.top_p,
        stop: request.stop_sequences.clone(),
        tools,
        tool_choice,
        response_format,
        stream,
    };

    let mut body = serde_json::to_value(&api_request).unwrap_or_default();
    merge_provider_options(&mut body, request.provider_options.as_ref(), provider_name);
    if let Some(mutate) = hooks.mutate_request {
        mutate(&mut body, request);
    }
    body
}

/// Merge `provider_options.<provider_name>` fields into the serialized API
/// request body.
///
/// The provider name is configurable (e.g. "groq", "together",
/// "openai-compatible"), allowing each instance to have its own namespace in
/// `provider_options`.
pub(crate) fn merge_provider_options(
    body: &mut serde_json::Value,
    provider_options: Option<&serde_json::Value>,
    provider_name: &str,
) {
    let Some(opts) = provider_options.and_then(|opts| opts.get(provider_name)) else {
        return;
    };
    let Some(body_map) = body.as_object_mut() else {
        return;
    };
    let Some(opts_map) = opts.as_object() else {
        return;
    };

    for (key, value) in opts_map {
        body_map.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
pub(crate) fn build_chat_request(
    request: &Request,
    stream: Option<bool>,
    provider_name: &str,
) -> serde_json::Value {
    build_chat_request_with_catalog(request, stream, provider_name, None, ChatHooks::NONE)
}

#[cfg(test)]
mod tests {
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;
    use crate::providers::openai_chat::wire::ApiRequest;
    use crate::types::Message;

    fn minimal_request() -> Request {
        Request {
            model:            "llama-3.1-70b".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         None,
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    #[test]
    fn api_request_stream_field_serialization() {
        let req = ApiRequest {
            model:           "test".into(),
            messages:        vec![],
            temperature:     None,
            max_tokens:      None,
            top_p:           None,
            stop:            None,
            tools:           None,
            tool_choice:     None,
            response_format: None,
            stream:          Some(true),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["stream"], true);

        // When stream is None, it should be omitted.
        let req_no_stream = ApiRequest {
            model:           "test".into(),
            messages:        vec![],
            temperature:     None,
            max_tokens:      None,
            top_p:           None,
            stop:            None,
            tools:           None,
            tool_choice:     None,
            response_format: None,
            stream:          None,
        };
        let json_no_stream = serde_json::to_value(&req_no_stream).unwrap();
        assert!(json_no_stream.get("stream").is_none());
    }

    #[test]
    fn provider_options_none_produces_standard_body() {
        let request = minimal_request();
        let body = build_chat_request(&request, None, "groq");
        assert_eq!(body["model"], "llama-3.1-70b");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn catalog_api_id_is_used_for_provider_request_body() {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
api_id = "acme/model-large"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog = fabro_model::Catalog::from_builtin_with_overrides(&settings).unwrap();
        let mut request = minimal_request();
        request.model = "acme-large".to_string();

        let body = build_chat_request_with_catalog(
            &request,
            None,
            "acme",
            Some(&catalog),
            ChatHooks::NONE,
        );

        assert_eq!(request.model, "acme-large");
        assert_eq!(body["model"], "acme/model-large");
    }

    #[test]
    fn merge_provider_options_with_non_object_value() {
        let mut body = serde_json::json!({"model": "test"});
        let opts = serde_json::json!({"groq": "not-an-object"});
        merge_provider_options(&mut body, Some(&opts), "groq");
        // Should not crash and body should be unchanged
        assert_eq!(body["model"], "test");
    }
}
