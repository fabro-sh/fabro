use fabro_types::StageHandler;
use serde_json::json;

#[test]
fn stage_handler_serializes_canonical_wire_values() {
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
            serde_json::from_value::<StageHandler>(json!(wire)).unwrap(),
            handler
        );
    }
}

#[test]
fn stage_handler_maps_current_handler_types_and_defaults_to_agent() {
    assert_eq!(
        StageHandler::from_handler_type(Some("agent")),
        StageHandler::Agent
    );
    assert_eq!(
        StageHandler::from_handler_type(Some("prompt")),
        StageHandler::Prompt
    );
    assert_eq!(
        StageHandler::from_handler_type(Some("tool")),
        StageHandler::Command
    );
    assert_eq!(
        StageHandler::from_handler_type(Some("parallel.fan_in")),
        StageHandler::ParallelFanIn
    );
    assert_eq!(
        StageHandler::from_handler_type(Some("unknown")),
        StageHandler::Agent
    );
    assert_eq!(StageHandler::from_handler_type(None), StageHandler::Agent);
}
