use std::any::{TypeId, type_name};

use fabro_api::types::StageHandler as ApiStageHandler;
use fabro_types::StageHandler;
use serde_json::json;

#[test]
fn stage_handler_reuses_canonical_type() {
    assert_same_type::<ApiStageHandler, StageHandler>();
}

#[test]
fn stage_handler_serializes_openapi_wire_values() {
    let cases = [
        (StageHandler::Start, "start"),
        (StageHandler::Exit, "exit"),
        (StageHandler::Agent, "agent"),
        (StageHandler::Prompt, "prompt"),
        (StageHandler::Command, "command"),
        (StageHandler::Human, "human"),
        (StageHandler::Conditional, "conditional"),
        (StageHandler::Parallel, "parallel"),
        (StageHandler::ParallelFanIn, "parallel.fan_in"),
        (StageHandler::StackManagerLoop, "stack.manager_loop"),
        (StageHandler::Wait, "wait"),
    ];

    for (handler, wire) in cases {
        assert_eq!(serde_json::to_value(handler).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<ApiStageHandler>(json!(wire)).unwrap(),
            handler
        );
    }
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
