//! Request encoding: canonical `Request` → Chat Completions body.

use super::translate;
use super::wire::ApiRequest;
use crate::codec::{CodecCtx, EncodedRequest};

/// Build the Chat Completions request for `ctx.request`. `stream` toggles the
/// `stream` body field. The body is assembled as a `serde_json::Value` so
/// `provider_options.<provider_name>` fields can be merged in before sending.
///
/// Infallible for this dialect — the `Codec::encode` `Result` is wrapped by the
/// trait impl.
pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> EncodedRequest {
    let request = ctx.request;
    let chat_messages = translate::translate_messages(&request.messages);
    let tools = request
        .tools
        .as_ref()
        .map(|t| translate::translate_tools(t));
    let tool_choice = request
        .tool_choice
        .as_ref()
        .map(translate::translate_tool_choice);
    let response_format = request
        .response_format
        .as_ref()
        .map(translate::translate_response_format);

    let api_request = ApiRequest {
        model: ctx.deployment_id.to_string(),
        messages: chat_messages,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        top_p: request.top_p,
        stop: request.stop_sequences.clone(),
        tools,
        tool_choice,
        response_format,
        stream: stream.then_some(true),
    };

    let mut body = serde_json::to_value(&api_request).unwrap_or_default();
    merge_provider_options(
        &mut body,
        request.provider_options.as_ref(),
        ctx.provider_name,
    );

    EncodedRequest {
        body,
        endpoint: "/chat/completions".to_string(),
        headers: Vec::new(),
    }
}

/// Merge `provider_options.<provider_name>` fields into the serialized API
/// request body.
///
/// The provider name is configurable (e.g. "groq", "together", "kimi"),
/// allowing each instance to have its own namespace in `provider_options`.
pub(super) fn merge_provider_options(
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
mod tests {
    use super::super::wire::ApiRequest;
    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{Message, Request};

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

    /// Encode `request` through the codec with `deployment_id == request.model`
    /// (the no-catalog case) and return the body.
    fn encode_body(request: &Request, provider_name: &str, stream: bool) -> serde_json::Value {
        let params = CodecParams::default();
        let deployment_id = request.model.clone();
        let ctx = CodecCtx {
            request,
            provider_name,
            deployment_id: &deployment_id,
            model: None,
            params: &params,
        };
        encode(&ctx, stream).body
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
    fn encode_uses_deployment_id_as_model() {
        let request = minimal_request();
        let params = CodecParams::default();
        let deployment_id = "acme/model-large".to_string();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "acme",
            deployment_id: &deployment_id,
            model:         None,
            params:        &params,
        };
        let body = encode(&ctx, false).body;
        assert_eq!(body["model"], "acme/model-large");
    }

    #[test]
    fn provider_options_none_produces_standard_body() {
        let request = minimal_request();
        let body = encode_body(&request, "groq", false);
        assert_eq!(body["model"], "llama-3.1-70b");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn provider_options_matching_name_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5,
                "presence_penalty": 0.3
            }
        }));
        let body = encode_body(&request, "groq", false);
        assert_eq!(body["frequency_penalty"], 0.5);
        assert_eq!(body["presence_penalty"], 0.3);
    }

    #[test]
    fn provider_options_different_name_ignored() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));
        let body = encode_body(&request, "groq", false);
        assert!(body.get("repetition_penalty").is_none());
    }

    #[test]
    fn provider_options_uses_adapter_name() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));
        let body = encode_body(&request, "together", false);
        assert_eq!(body["repetition_penalty"], 1.2);
    }

    #[test]
    fn provider_options_preserves_standard_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.7);
        request.max_tokens = Some(200);
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5
            }
        }));
        let body = encode_body(&request, "groq", true);
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["stream"], true);
        assert_eq!(body["frequency_penalty"], 0.5);
    }

    #[test]
    fn provider_options_can_override_model() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "model": "custom-model"
            }
        }));
        let body = encode_body(&request, "groq", false);
        assert_eq!(body["model"], "custom-model");
    }

    #[test]
    fn merge_provider_options_with_non_object_value() {
        let mut body = serde_json::json!({"model": "test"});
        let opts = serde_json::json!({"groq": "not-an-object"});
        merge_provider_options(&mut body, Some(&opts), "groq");
        assert_eq!(body["model"], "test");
    }
}
