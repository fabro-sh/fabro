//! Proves the `CompletionMessage` / `CompletionMessageRole` /
//! `CompletionContentPart` OpenAPI schemas are served by the canonical
//! `fabro_types::{Message, Role, ContentPart}` via build.rs
//! `with_replacement`, and that the canonical serde output matches the
//! wire shape the spec describes.

use std::any::{TypeId, type_name};

use fabro_api::types::{ContentPart as ApiContentPart, Message as ApiMessage, Role as ApiRole};
use fabro_types::{ContentPart, Message, Role, ToolCall, ToolResult};
use serde_json::json;

#[test]
fn completion_message_reuses_domain_types() {
    assert_same_type::<ApiMessage, Message>();
    assert_same_type::<ApiRole, Role>();
    assert_same_type::<ApiContentPart, ContentPart>();
}

#[test]
fn role_json_matches_openapi_enum() {
    for (role, wire) in [
        (Role::System, "system"),
        (Role::User, "user"),
        (Role::Assistant, "assistant"),
        (Role::Tool, "tool"),
        (Role::Developer, "developer"),
    ] {
        assert_eq!(serde_json::to_value(role).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<Role>(json!(wire)).unwrap(),
            role,
            "round trip for {wire}"
        );
    }
}

#[test]
fn message_json_matches_openapi_shape() {
    // Optional fields are omitted, not serialized as null.
    assert_eq!(
        serde_json::to_value(Message::user("hello")).unwrap(),
        json!({
            "role": "user",
            "content": [{"kind": "text", "data": "hello"}]
        })
    );

    // Populated optionals appear under the spec's property names.
    let mut message = Message::tool_result("call_1", json!("ok"), false);
    message.name = Some("checker".to_string());
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "role": "tool",
            "content": [{
                "kind": "tool_result",
                "data": {
                    "tool_call_id": "call_1",
                    "content": "ok",
                    "is_error": false
                }
            }],
            "name": "checker",
            "tool_call_id": "call_1"
        })
    );
}

#[test]
fn message_accepts_explicit_nulls_for_optionals() {
    // The previously generated API type serialized absent optionals as
    // explicit nulls; inbound payloads in that older shape must keep
    // parsing.
    let message: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [{"kind": "text", "data": "hi"}],
        "name": null,
        "tool_call_id": null
    }))
    .unwrap();
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(message.name, None);
    assert_eq!(message.tool_call_id, None);
}

#[test]
fn content_part_json_matches_openapi_envelope() {
    // The spec describes a `{kind, data}` envelope; every variant must
    // serialize into it.
    assert_eq!(
        serde_json::to_value(ContentPart::text("hi")).unwrap(),
        json!({"kind": "text", "data": "hi"})
    );

    assert_eq!(
        serde_json::to_value(ContentPart::ToolCall(ToolCall::new(
            "call_1",
            "write_workflow_file",
            json!({"file_name": "workflow.fabro"}),
        )))
        .unwrap(),
        json!({
            "kind": "tool_call",
            "data": {
                "id": "call_1",
                "name": "write_workflow_file",
                "type": "function",
                "arguments": {"file_name": "workflow.fabro"},
                "raw_arguments": null
            }
        })
    );

    assert_eq!(
        serde_json::to_value(ContentPart::ToolResult(ToolResult::success(
            "call_1",
            json!("done"),
        )))
        .unwrap(),
        json!({
            "kind": "tool_result",
            "data": {
                "tool_call_id": "call_1",
                "content": "done",
                "is_error": false
            }
        })
    );
}

#[test]
fn content_part_preserves_unknown_kinds() {
    // The spec leaves `kind` open-ended; unknown kinds must round-trip
    // (previously the handler conversion silently dropped them).
    let wire = json!({"kind": "mystery", "data": {"x": 1}});
    let part: ContentPart = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(part, ContentPart::Other {
        kind: "mystery".to_string(),
        data: json!({"x": 1}),
    });
    assert_eq!(serde_json::to_value(part).unwrap(), wire);
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
