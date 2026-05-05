use std::collections::{BTreeMap, HashMap};

use chrono::{TimeZone, Utc};
use fabro_store::{RunProjection, SerializableProjection, StageId};
use fabro_types::graph::Graph;
use fabro_types::run::RunSpec;
use fabro_types::{
    BilledModelUsage, Checkpoint, RunStatus, SandboxRecord, StageCompletion, StageOutcome,
    StartRecord, TerminalStatus, WorkflowSettings, first_event_seq, fixtures,
};
use serde_json::json;

fn sample_run_spec() -> RunSpec {
    RunSpec {
        run_id:           fixtures::RUN_1,
        settings:         WorkflowSettings::default(),
        graph:            Graph::new("ship"),
        workflow_slug:    Some("demo".to_string()),
        source_directory: Some("/tmp/project".to_string()),
        labels:           HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance:       None,
        manifest_blob:    None,
        definition_blob:  None,
        git:              Some(fabro_types::GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          None,
            dirty:        fabro_types::DirtyStatus::Clean,
            push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
        }),
        fork_source_ref:  None,
        in_place:         false,
    }
}

fn sample_checkpoint() -> Checkpoint {
    Checkpoint {
        timestamp:                  Utc
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .single()
            .expect("timestamp should be representable"),
        current_node:               "build".to_string(),
        completed_nodes:            vec!["build".to_string()],
        node_retries:               HashMap::new(),
        context_values:             HashMap::new(),
        node_outcomes:              HashMap::new(),
        next_node_id:               Some("ship".to_string()),
        git_commit_sha:             Some("abc123".to_string()),
        loop_failure_signatures:    HashMap::new(),
        restart_failure_signatures: HashMap::new(),
        node_visits:                HashMap::from([("build".to_string(), 2usize)]),
    }
}

fn sample_usage() -> BilledModelUsage {
    serde_json::from_value(json!({
        "input": {
            "usage": {
                "model": {
                    "provider": "openai",
                    "model_id": "gpt-5.2"
                },
                "tokens": {
                    "input_tokens": 123,
                    "output_tokens": 45
                }
            },
            "facts": {
                "provider": "open_ai"
            }
        },
        "total_usd_micros": 168
    }))
    .expect("sample usage should deserialize")
}

#[test]
fn serializable_projection_round_trips_and_trims_bulky_node_fields() {
    let stage_id = StageId::new("build", 2);
    let mut projection = RunProjection::default();
    projection.spec = Some(sample_run_spec());
    projection.start = Some(StartRecord {
        run_id:     fixtures::RUN_1,
        start_time: Utc
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .single()
            .expect("start_time should be representable"),
        run_branch: Some("fabro/run/demo".to_string()),
        base_sha:   Some("deadbeef".to_string()),
    });
    projection.status = Some(RunStatus::Running);
    projection.checkpoint = Some(sample_checkpoint());
    projection.sandbox = Some(SandboxRecord {
        provider:          "local".to_string(),
        working_directory: "/tmp/project".to_string(),
        identifier:        Some("sandbox-1".to_string()),
        repo_cloned:       None,
        clone_origin_url:  None,
        clone_branch:      None,
    });
    projection.pending_interviews = BTreeMap::new();
    let stage = projection.stage_entry(stage_id.node_id(), stage_id.visit(), first_event_seq(2));
    stage.prompt = Some("plan the work".to_string());
    stage.response = Some("done".to_string());
    stage.completion = Some(StageCompletion {
        outcome:        StageOutcome::Succeeded,
        notes:          Some("ok".to_string()),
        failure_reason: None,
        timestamp:      Utc
            .with_ymd_and_hms(2026, 4, 20, 12, 1, 0)
            .single()
            .expect("timestamp should be representable"),
    });
    stage.provider_used = Some(json!({ "provider": "openai", "model": "gpt-5.4" }));
    stage.diff = Some("diff --git a/a b/a".to_string());
    stage.script_invocation = Some(json!({ "command": "cargo test" }));
    stage.script_timing = Some(json!({ "duration_ms": 10 }));
    stage.parallel_results = Some(json!([{ "stage": "fanout@1" }]));
    stage.duration_ms = Some(1234);
    stage.usage = Some(sample_usage());
    stage.stdout = Some("stdout".to_string());
    stage.stderr = Some("stderr".to_string());

    let serialized = serde_json::to_value(SerializableProjection(&projection))
        .expect("projection should serialize");
    let round_tripped: RunProjection =
        serde_json::from_value(serialized).expect("serialized projection should deserialize");
    let node = round_tripped.stage(&stage_id).expect("node should remain");

    assert_eq!(round_tripped.spec().map(RunSpec::id), Some(fixtures::RUN_1));
    assert_eq!(
        round_tripped
            .current_checkpoint()
            .expect("checkpoint should remain")
            .current_node,
        "build"
    );
    assert_eq!(round_tripped.status(), Some(RunStatus::Running));
    assert!(!round_tripped.is_terminal());
    assert_eq!(node.prompt, None);
    assert_eq!(node.response, None);
    assert_eq!(node.diff, None);
    assert_eq!(node.stdout, None);
    assert_eq!(node.stderr, None);
    assert_eq!(node.first_event_seq, first_event_seq(2));
    assert_eq!(
        node.completion
            .as_ref()
            .map(|completion| completion.outcome),
        Some(StageOutcome::Succeeded)
    );
    assert_eq!(
        node.provider_used,
        Some(json!({ "provider": "openai", "model": "gpt-5.4" }))
    );
    assert_eq!(
        node.script_invocation,
        Some(json!({ "command": "cargo test" }))
    );
    assert_eq!(node.script_timing, Some(json!({ "duration_ms": 10 })));
    assert_eq!(
        node.parallel_results,
        Some(json!([{ "stage": "fanout@1" }]))
    );
    assert_eq!(node.duration_ms, Some(1234));
    assert_eq!(node.usage, Some(sample_usage()));
}

#[test]
fn projection_query_methods_expose_common_state() {
    let mut projection = RunProjection::default();
    projection.spec = Some(sample_run_spec());
    projection.status = Some(RunStatus::Archived {
        prior: TerminalStatus::Dead,
    });
    projection.checkpoint = Some(sample_checkpoint());
    projection.pending_interviews = BTreeMap::from([(
        "q-1".to_string(),
        fabro_store::PendingInterviewRecord::default(),
    )]);

    assert_eq!(
        projection.spec().map(RunSpec::workflow_slug),
        Some(Some("demo"))
    );
    assert_eq!(
        projection.status(),
        Some(RunStatus::Archived {
            prior: TerminalStatus::Dead,
        })
    );
    assert!(projection.is_terminal());
    assert_eq!(
        projection
            .current_checkpoint()
            .map(|checkpoint| checkpoint.current_node.as_str()),
        Some("build")
    );
    assert!(projection.pending_interviews().contains_key("q-1"));
}
