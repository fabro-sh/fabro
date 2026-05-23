use std::any::{TypeId, type_name};

use fabro_api::types::AgentSessionActivatedProps as ApiAgentSessionActivatedProps;
use fabro_types::run_event::AgentSessionActivatedProps;
use fabro_types::{PermissionLevel, ReasoningEffort, SessionCapability, Speed};
use serde_json::json;

#[test]
fn agent_session_activated_props_reuses_canonical_type() {
    assert_same_type::<ApiAgentSessionActivatedProps, AgentSessionActivatedProps>();
}

#[test]
fn agent_session_activated_props_matches_openapi_json_shape() {
    let value = json!({
        "thread_id": "thread-1",
        "provider": "openai",
        "model": "gpt-5.4",
        "reasoning_effort": "high",
        "speed": "fast",
        "permission_level": "read-only",
        "capabilities": ["steer"],
        "visit": 1
    });

    let props: AgentSessionActivatedProps = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(props.permission_level, Some(PermissionLevel::ReadOnly));
    assert_eq!(props.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(props.speed, Some(Speed::Fast));
    assert_eq!(props.capabilities, vec![SessionCapability::Steer]);
    assert_eq!(serde_json::to_value(&props).unwrap(), value);

    let api_props: ApiAgentSessionActivatedProps = serde_json::from_value(value).unwrap();
    assert_eq!(api_props, props);
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
