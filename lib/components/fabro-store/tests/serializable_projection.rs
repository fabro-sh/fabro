use std::collections::{BTreeMap, HashMap};

use chrono::{TimeZone, Utc};
use fabro_store::{RunProjection, SerializableProjection, StageId};
use fabro_types::graph::Graph;
use fabro_types::run::RunSpec;
use fabro_types::{
    Checkpoint, CheckpointRecord, InterviewQuestionRecord, ModelUsage, ParallelBranchResult,
    QuestionType, RunDiff, RunSandbox, RunSandboxInstance, RunSandboxPlan, RunSandboxRuntime,
    RunStatus, SandboxProviderKind, StageCompletion, StageModelUsage, StageOutcome, StartRecord,
    first_event_seq, fixtures, test_support,
};
use serde_json::json;

fn sample_run_spec() -> RunSpec {
    RunSpec {
        graph: Graph::new("ship"),
        workflow_slug: Some("demo".to_string()),
        source_directory: Some("/tmp/project".to_string()),
        labels: HashMap::from([("team".to_string(), "platform".to_string())]),
        git: Some(fabro_types::GitContext {
            origin_url: "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:     "main".to_string(),
            sha:        None,
            dirty:      fabro_types::DirtyStatus::Clean,
        }),
        ..test_support::test_run_spec()
    }
}

fn sample_checkpoint() -> Checkpoint {
    Checkpoint {
        timestamp:      Utc
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .single()
            .expect("timestamp should be representable"),
        current_node:   "build".to_string(),
        git_commit_sha: Some("abc123".to_string()),
    }
}

fn sample_usage() -> ModelUsage {
    serde_json::from_value(json!({
        "model": { "provider": "openai", "model_id": "gpt-5.2" },
        "usage": {
            "tokens": {
                "input": 123,
                "output": 45
            },
            "cost": { "usd_micros": 168, "source": "catalog" }
        }
    }))
    .expect("sample usage should deserialize")
}

#[test]
fn serializable_projection_round_trips_and_trims_bulky_node_fields() {
    let stage_id = StageId::new("build", 2);
    let mut projection = RunProjection::new(
        "Demo".to_string(),
        sample_run_spec(),
        Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .single()
            .unwrap(),
    );
    projection.start = Some(StartRecord {
        start_time: Utc
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .single()
            .expect("start_time should be representable"),
        run_branch: Some("fabro/run/demo".to_string()),
        base_sha:   Some("deadbeef".to_string()),
    });
    projection.status = RunStatus::Running;
    projection.checkpoints.push(CheckpointRecord {
        seq:        7,
        checkpoint: sample_checkpoint(),
        diff:       RunDiff::default(),
    });
    let sandbox_plan = RunSandboxPlan {
        provider: SandboxProviderKind::LOCAL,
        image:    None,
        snapshot: None,
    };
    projection.sandbox = Some(RunSandbox::ready(sandbox_plan, RunSandboxInstance {
        provider:          SandboxProviderKind::LOCAL,
        image:             None,
        snapshot:          None,
        runtime:           RunSandboxRuntime {
            id:                "sandbox-1".to_string(),
            working_directory: "/tmp/project".to_string(),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
        },
        ready_duration_ms: None,
        retained:          None,
    }));
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
    stage.provider_used = Some(StageModelUsage {
        mode:             StageModelUsage::MODE_PROMPT.to_string(),
        provider:         Some("openai".to_string()),
        model:            Some("gpt-5.4".to_string()),
        reasoning_effort: None,
        speed:            None,
    });
    stage.diff = Some("diff --git a/a b/a".to_string());
    stage.script_invocation = Some(json!({ "command": "cargo test" }));
    stage.script_timing = Some(json!({ "duration_ms": 10 }));
    let parallel_results = vec![ParallelBranchResult {
        id:              "review".to_string(),
        index:           Some(0),
        item_label:      None,
        status:          StageOutcome::Succeeded,
        context_updates: BTreeMap::from([("response.review".to_string(), json!("looks good"))]),
    }];
    stage.parallel_results = Some(parallel_results.clone());
    stage.timing = Some(fabro_types::StageTiming::wall_only(1234));
    let usage = sample_usage();
    stage.usage = usage.usage;
    stage.model = Some(usage.model().clone());
    stage.output = Some("output".to_string());

    let serialized = serde_json::to_value(SerializableProjection(&projection))
        .expect("projection should serialize");
    assert_eq!(
        serialized["stages"]["build@2"]["usage"]["tokens"]["input"],
        json!(123)
    );
    assert_eq!(
        serialized["stages"]["build@2"]["model"]["model_id"],
        json!("gpt-5.2")
    );
    let round_tripped: RunProjection =
        serde_json::from_value(serialized).expect("serialized projection should deserialize");
    let node = round_tripped.stage(&stage_id).expect("node should remain");

    assert_eq!(round_tripped.spec().id(), fixtures::RUN_1);
    assert_eq!(
        round_tripped
            .current_checkpoint()
            .expect("checkpoint should remain")
            .current_node,
        "build"
    );
    assert_eq!(round_tripped.status(), RunStatus::Running);
    assert!(!round_tripped.is_terminal());
    assert_eq!(node.prompt, None);
    assert_eq!(node.response, None);
    assert_eq!(node.diff, None);
    assert_eq!(node.output, None);
    assert_eq!(node.first_event_seq, first_event_seq(2));
    assert_eq!(
        node.completion
            .as_ref()
            .map(|completion| completion.outcome),
        Some(StageOutcome::Succeeded)
    );
    assert_eq!(
        node.provider_used
            .as_ref()
            .and_then(|usage| usage.provider.as_deref()),
        Some("openai")
    );
    assert_eq!(
        node.provider_used
            .as_ref()
            .and_then(|usage| usage.model.as_deref()),
        Some("gpt-5.4")
    );
    assert_eq!(
        node.script_invocation,
        Some(json!({ "command": "cargo test" }))
    );
    assert_eq!(node.script_timing, Some(json!({ "duration_ms": 10 })));
    assert_eq!(node.parallel_results, Some(parallel_results));
    assert_eq!(node.timing.map(|t| t.wall_time_ms), Some(1234));
    assert_eq!(node.usage, usage.usage);
    assert_eq!(node.model.as_ref(), Some(usage.model()));
}

#[test]
fn projection_query_methods_expose_common_state() {
    let mut projection = RunProjection::new("Demo".to_string(), sample_run_spec(), Utc::now());
    projection.status = RunStatus::Dead;
    projection.archived_at = Some(Utc::now());
    projection.checkpoints.push(CheckpointRecord {
        seq:        7,
        checkpoint: sample_checkpoint(),
        diff:       RunDiff::default(),
    });
    projection.pending_interviews =
        BTreeMap::from([("q-1".to_string(), fabro_store::PendingInterviewRecord {
            question:   InterviewQuestionRecord {
                id:              "q-1".to_string(),
                text:            "Approve?".to_string(),
                stage:           "build".to_string(),
                question_type:   QuestionType::Freeform,
                options:         Vec::new(),
                allow_freeform:  true,
                timeout_seconds: None,
                context_display: None,
                review_target:   None,
            },
            started_at: Utc::now(),
        })]);

    assert_eq!(projection.spec().workflow_slug(), Some("demo"));
    assert_eq!(projection.status(), RunStatus::Dead);
    assert!(projection.is_archived());
    assert!(projection.is_terminal());
    assert_eq!(
        projection
            .current_checkpoint()
            .map(|checkpoint| checkpoint.current_node.as_str()),
        Some("build")
    );
    assert!(projection.pending_interviews().contains_key("q-1"));
}
