use std::any::{TypeId, type_name};

use fabro_api::types::{
    ActivatedSkill as ApiActivatedSkill, AgentMcpToolSummary as ApiAgentMcpToolSummary,
    AgentSkillActivationSource as ApiAgentSkillActivationSource,
    AgentSkillSummary as ApiAgentSkillSummary, McpServerProjection as ApiMcpServerProjection,
    McpServerStatus as ApiMcpServerStatus, PermissionLevel as ApiPermissionLevel,
    SkillsProjection as ApiSkillsProjection, StageContextWindow as ApiStageContextWindow,
    StageContextWindowBreakdownItem as ApiStageContextWindowBreakdownItem,
    StageContextWindowCategory as ApiStageContextWindowCategory,
    StageContextWindowCountMethod as ApiStageContextWindowCountMethod,
    StageContextWindowProjection as ApiStageContextWindowProjection,
    StageContextWindowStaleness as ApiStageContextWindowStaleness,
    StageContextWindowUnavailableReason as ApiStageContextWindowUnavailableReason,
    StageContextWindowWarning as ApiStageContextWindowWarning,
    StageProjection as ApiStageProjection, SubAgentProjection as ApiSubAgentProjection,
    SubAgentStatus as ApiSubAgentStatus, TodoListProjection as ApiTodoListProjection,
};
use fabro_types::{
    ActivatedSkill, AgentMcpToolSummary, AgentSkillActivationSource, AgentSkillSummary,
    McpServerProjection, McpServerStatus, PermissionLevel, SkillsProjection, StageContextWindow,
    StageContextWindowBreakdownItem, StageContextWindowCategory, StageContextWindowCountMethod,
    StageContextWindowProjection, StageContextWindowStaleness, StageContextWindowUnavailableReason,
    StageContextWindowWarning, StageProjection, SubAgentProjection, SubAgentStatus, TodoListKind,
    TodoListProjection,
};
use serde_json::json;

#[test]
fn stage_projection_reuses_canonical_type() {
    assert_same_type::<ApiStageProjection, StageProjection>();
}

#[test]
fn stage_projection_reuses_nested_agent_state_types() {
    assert_same_type::<ApiTodoListProjection, TodoListProjection>();
    assert_same_type::<ApiSubAgentProjection, SubAgentProjection>();
    assert_same_type::<ApiSubAgentStatus, SubAgentStatus>();
    assert_same_type::<ApiSkillsProjection, SkillsProjection>();
    assert_same_type::<ApiActivatedSkill, ActivatedSkill>();
    assert_same_type::<ApiAgentSkillSummary, AgentSkillSummary>();
    assert_same_type::<ApiAgentSkillActivationSource, AgentSkillActivationSource>();
    assert_same_type::<ApiMcpServerProjection, McpServerProjection>();
    assert_same_type::<ApiMcpServerStatus, McpServerStatus>();
    assert_same_type::<ApiAgentMcpToolSummary, AgentMcpToolSummary>();
    assert_same_type::<ApiPermissionLevel, PermissionLevel>();
    assert_same_type::<ApiStageContextWindow, StageContextWindow>();
    assert_same_type::<ApiStageContextWindowProjection, StageContextWindowProjection>();
    assert_same_type::<ApiStageContextWindowBreakdownItem, StageContextWindowBreakdownItem>();
    assert_same_type::<ApiStageContextWindowCategory, StageContextWindowCategory>();
    assert_same_type::<ApiStageContextWindowCountMethod, StageContextWindowCountMethod>();
    assert_same_type::<ApiStageContextWindowStaleness, StageContextWindowStaleness>();
    assert_same_type::<ApiStageContextWindowUnavailableReason, StageContextWindowUnavailableReason>(
    );
    assert_same_type::<ApiStageContextWindowWarning, StageContextWindowWarning>();
}

#[test]
fn stage_projection_round_trips_representative_json() {
    let value = json!({
        "first_event_seq": 1,
        "prompt": "build it",
        "response": "done",
        "completion": {
            "outcome": "succeeded",
            "notes": null,
            "failure_reason": null,
            "timestamp": "2026-04-29T12:34:56Z"
        },
        "provider_used": {
            "mode": "prompt",
            "provider": "openai",
            "model": "gpt-5.2",
            "reasoning_effort": "high",
            "speed": "fast"
        },
        "diff": "diff --git a/file b/file",
        "script_invocation": { "command": "cargo test" },
        "script_timing": { "duration_ms": 42 },
        "parallel_results": [{ "branch": 0, "status": "succeeded" }],
        "output": "ok",
        "termination": "exited",
        "started_at": "2026-04-29T12:34:00Z",
        "timing": {
            "wall_time_ms": 56000,
            "inference_time_ms": 0,
            "tool_time_ms": 0,
            "active_time_ms": 0
        },
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "todos": {
            "kind": "openai_plan",
            "list_id": "openai_plan:ses_root",
            "items": [
                {
                    "id": "todo-1",
                    "status": "in_progress",
                    "order": 0,
                    "subject": "Write tests",
                    "active_form": "Writing tests"
                }
            ]
        },
        "subagents": [
            {
                "agent_id": "sub-1",
                "depth": 1,
                "task": "Investigate failing test",
                "status": {
                    "kind": "completed",
                    "success": true,
                    "turns_used": 3
                }
            }
        ],
        "skills": {
            "available": [
                {
                    "name": "rust",
                    "description": "Rust workflow help"
                }
            ],
            "activated": [
                {
                    "name": "rust",
                    "source": "slash"
                }
            ]
        },
        "permission_level": "read-only",
        "mcp_servers": [
            {
                "server_name": "filesystem",
                "tool_count": 1,
                "status": {
                    "kind": "ready",
                    "tools": [
                        {
                            "name": "read_file",
                            "original_name": "read_file"
                        }
                    ]
                }
            }
        ],
        "context_window": {
            "provider": "openai",
            "model": "gpt-5.4",
            "context_window_tokens": 400000,
            "input_tokens": 123456,
            "usage_percent": 30.864,
            "count_method": "provider_api_scaled_breakdown",
            "staleness": "live",
            "generated_at": "2026-05-23T12:34:56Z",
            "event_seq": 42,
            "breakdown": [
                {
                    "category": "system_prompt",
                    "tokens": 30000,
                    "usage_percent": 7.5
                }
            ],
            "warnings": []
        },
        "state": "succeeded"
    });

    let state: StageProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(state).unwrap(), value);
}

#[test]
fn stage_context_window_response_round_trips_representative_json() {
    let value = json!({
        "stage_id": "implement@1",
        "available": true,
        "unavailable_reason": null,
        "provider": "openai",
        "model": "gpt-5.4",
        "context_window_tokens": 400000,
        "input_tokens": 123456,
        "usage_percent": 30.864,
        "count_method": "provider_api_scaled_breakdown",
        "staleness": "live",
        "generated_at": "2026-05-23T12:34:56Z",
        "event_seq": 42,
        "breakdown": [
            {
                "category": "system_prompt",
                "tokens": 30000,
                "usage_percent": 7.5
            }
        ],
        "warnings": [
            {
                "code": "local_token_estimate",
                "message": "input token count is a local estimate"
            }
        ]
    });

    let response: StageContextWindow = serde_json::from_value(value.clone()).unwrap();
    let api_response: ApiStageContextWindow = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(api_response, response);
    assert_eq!(serde_json::to_value(response).unwrap(), value);
}

#[test]
fn permission_level_matches_openapi_json_shape() {
    let permission_json = serde_json::to_value(PermissionLevel::ReadOnly).unwrap();
    assert_eq!(permission_json, json!("read-only"));
    let api_permission: ApiPermissionLevel = serde_json::from_value(permission_json).unwrap();
    assert_eq!(api_permission, PermissionLevel::ReadOnly);
}

#[test]
fn nested_agent_state_types_match_openapi_json_shape() {
    let todo_list = TodoListProjection::new(TodoListKind::OpenAiPlan, "openai_plan:ses_root");
    let todo_json = serde_json::to_value(&todo_list).unwrap();
    assert_eq!(
        todo_json,
        json!({
            "kind": "openai_plan",
            "list_id": "openai_plan:ses_root",
            "items": []
        })
    );
    let api_todo_list: ApiTodoListProjection = serde_json::from_value(todo_json).unwrap();
    assert_eq!(api_todo_list, todo_list);

    let subagent = SubAgentProjection {
        agent_id: "sub-1".to_string(),
        depth:    1,
        task:     "Investigate failing test".to_string(),
        status:   SubAgentStatus::Completed {
            success:    true,
            turns_used: 3,
        },
    };
    let subagent_json = serde_json::to_value(&subagent).unwrap();
    assert_eq!(
        subagent_json,
        json!({
            "agent_id": "sub-1",
            "depth": 1,
            "task": "Investigate failing test",
            "status": {
                "kind": "completed",
                "success": true,
                "turns_used": 3
            }
        })
    );
    let api_subagent: ApiSubAgentProjection = serde_json::from_value(subagent_json).unwrap();
    assert_eq!(api_subagent, subagent);

    let skill = AgentSkillSummary {
        name:        "rust".to_string(),
        description: "Rust workflow help".to_string(),
    };
    let skill_json = serde_json::to_value(&skill).unwrap();
    assert_eq!(
        skill_json,
        json!({
            "name": "rust",
            "description": "Rust workflow help"
        })
    );
    let api_skill: ApiAgentSkillSummary = serde_json::from_value(skill_json).unwrap();
    assert_eq!(api_skill, skill);

    let source_json = serde_json::to_value(AgentSkillActivationSource::Slash).unwrap();
    assert_eq!(source_json, json!("slash"));
    let api_source: ApiAgentSkillActivationSource = serde_json::from_value(source_json).unwrap();
    assert_eq!(api_source, AgentSkillActivationSource::Slash);

    let activated = ActivatedSkill {
        name:   "rust".to_string(),
        source: AgentSkillActivationSource::Slash,
    };
    let skills = SkillsProjection {
        available: vec![skill],
        activated: vec![activated],
    };
    let skills_json = serde_json::to_value(&skills).unwrap();
    assert_eq!(
        skills_json,
        json!({
            "available": [
                {
                    "name": "rust",
                    "description": "Rust workflow help"
                }
            ],
            "activated": [
                {
                    "name": "rust",
                    "source": "slash"
                }
            ]
        })
    );
    let api_skills: ApiSkillsProjection = serde_json::from_value(skills_json).unwrap();
    assert_eq!(api_skills, skills);

    let tool = AgentMcpToolSummary {
        name:          "read_file".to_string(),
        original_name: "read_file".to_string(),
    };
    let tool_json = serde_json::to_value(&tool).unwrap();
    assert_eq!(
        tool_json,
        json!({
            "name": "read_file",
            "original_name": "read_file"
        })
    );
    let api_tool: ApiAgentMcpToolSummary = serde_json::from_value(tool_json).unwrap();
    assert_eq!(api_tool, tool);

    let mcp_server = McpServerProjection {
        server_name: "filesystem".to_string(),
        tool_count:  1,
        status:      McpServerStatus::Ready { tools: vec![tool] },
    };
    let mcp_json = serde_json::to_value(&mcp_server).unwrap();
    assert_eq!(
        mcp_json,
        json!({
            "server_name": "filesystem",
            "tool_count": 1,
            "status": {
                "kind": "ready",
                "tools": [
                    {
                        "name": "read_file",
                        "original_name": "read_file"
                    }
                ]
            }
        })
    );
    let api_mcp: ApiMcpServerProjection = serde_json::from_value(mcp_json).unwrap();
    assert_eq!(api_mcp, mcp_server);
    assert_eq!(mcp_server.tool_count, 1);
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
