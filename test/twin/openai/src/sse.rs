use async_stream::stream;
use axum::{
    body::Body,
    http::{HeaderValue, Response, StatusCode, header},
};
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};

use crate::engine::{failures::TransportOptions, plan::ResponsePlan};

pub fn responses_sse_response(plan: &ResponsePlan, transport: TransportOptions) -> Response<Body> {
    let mut events = Vec::new();
    let reasoning_item_id = format!("rs_{}", plan.id);
    let message_item_id = format!("msg_{}", plan.id);
    let mut next_output_index = 0;
    let streamed_text = plan.structured_output.as_ref().map(Value::to_string);

    events.push(sse_event(
        "response.created",
        &json!({
            "type": "response.created",
            "response": {
                "id": plan.id,
                "object": "response",
                "created": plan.created,
                "model": plan.model,
                "status": "in_progress",
                "output": [],
            },
        }),
    ));
    events.push(sse_event(
        "response.in_progress",
        &json!({
            "type": "response.in_progress",
            "response": {
                "id": plan.id,
                "object": "response",
                "created": plan.created,
                "model": plan.model,
                "status": "in_progress",
                "output": [],
            },
        }),
    ));

    events.push(sse_event(
        "response.output_item.added",
        &json!({
            "type": "response.output_item.added",
            "item": {
                "id": reasoning_item_id,
                "type": "reasoning",
                "summary": [],
            },
            "output_index": next_output_index,
        }),
    ));
    for reasoning in &plan.reasoning {
        events.push(sse_event(
            "response.reasoning.delta",
            &json!({
                "type": "response.reasoning.delta",
                "delta": reasoning,
                "item_id": reasoning_item_id,
                "output_index": next_output_index,
            }),
        ));
    }
    events.push(sse_event(
        "response.output_item.done",
        &json!({
            "type": "response.output_item.done",
            "item": {
                "id": reasoning_item_id,
                "type": "reasoning",
                "summary": [],
            },
            "output_index": next_output_index,
        }),
    ));
    next_output_index += 1;

    if !plan.response_text.is_empty() || streamed_text.is_some() {
        events.push(sse_event(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "item": {
                    "id": message_item_id,
                    "type": "message",
                    "status": "in_progress",
                    "content": [],
                    "role": "assistant",
                },
                "output_index": next_output_index,
            }),
        ));

        let message_text = streamed_text
            .as_deref()
            .unwrap_or(plan.response_text.as_str());

        if !message_text.is_empty() {
            events.push(sse_event(
                "response.content_part.added",
                &json!({
                    "type": "response.content_part.added",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "part": {
                        "type": "output_text",
                        "text": "",
                    },
                }),
            ));
            events.push(sse_event(
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "delta": message_text,
                }),
            ));
            events.push(sse_event(
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "text": message_text,
                }),
            ));
            events.push(sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "part": {
                        "type": "output_text",
                        "text": message_text,
                    },
                }),
            ));
        }

        events.push(sse_event(
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done",
                "item": {
                    "id": message_item_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                },
                "output_index": next_output_index,
            }),
        ));
        next_output_index += 1;
    }

    for tool_call in &plan.tool_calls {
        let item_id = format!("fc_{}", tool_call.id);
        events.push(sse_event(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "item": {
                    "id": item_id,
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": "",
                },
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            "response.function_call_arguments.delta",
            &json!({
                "type": "response.function_call_arguments.delta",
                "item_id": item_id,
                "delta": tool_call.arguments.to_string(),
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            "response.function_call_arguments.done",
            &json!({
                "type": "response.function_call_arguments.done",
                "item_id": item_id,
                "arguments": tool_call.arguments.to_string(),
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done",
                "item": {
                    "id": item_id,
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": tool_call.arguments.to_string(),
                },
                "output_index": next_output_index,
            }),
        ));
        next_output_index += 1;
    }

    if !transport.malformed_sse {
        events.push(sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": {
                    "id": plan.id,
                    "object": "response",
                    "created": plan.created,
                    "model": plan.model,
                    "status": "completed",
                },
            }),
        ));
    }

    stream_response(events, transport)
}

pub fn chat_sse_response(plan: &ResponsePlan, transport: TransportOptions) -> Response<Body> {
    let mut events = Vec::new();
    let content = plan.chat_content();
    events.push(chat_chunk(&json!({
        "id": format!("chatcmpl_{}", plan.id),
        "object": "chat.completion.chunk",
        "created": plan.created,
        "model": plan.model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant"
            },
            "finish_reason": Value::Null,
        }]
    })));

    if !content.is_empty() {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "content": content
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    for reasoning in &plan.reasoning {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning": reasoning
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    if !plan.tool_calls.is_empty() {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": plan.tool_calls.iter().map(|tool_call| json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": tool_call.arguments.to_string(),
                        }
                    })).collect::<Vec<_>>()
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    if !transport.malformed_sse {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": if plan.tool_calls.is_empty() { "stop" } else { "tool_calls" },
            }]
        })));
        events.push("data: [DONE]\n\n".to_owned());
    }

    stream_response(events, transport)
}

fn stream_response(events: Vec<String>, transport: TransportOptions) -> Response<Body> {
    let limit = transport.close_after_chunks.unwrap_or(events.len());
    let malformed_sse = transport.malformed_sse;
    let inter_event_delay_ms = transport.inter_event_delay_ms;

    let body = Body::from_stream(stream! {
        for (index, event) in events.into_iter().enumerate() {
            if index >= limit {
                break;
            }

            if inter_event_delay_ms > 0 {
                sleep(Duration::from_millis(inter_event_delay_ms)).await;
            }

            yield Ok::<_, std::convert::Infallible>(event.into_bytes());
        }

        if malformed_sse {
            yield Ok::<_, std::convert::Infallible>(b"event: malformed\ndata: {".to_vec());
        }
    });

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn chat_chunk(data: &Value) -> String {
    format!("data: {data}\n\n")
}
