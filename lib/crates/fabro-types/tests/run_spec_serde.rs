use std::collections::HashMap;

use fabro_types::graph::Graph;
use fabro_types::run::{DirtyStatus, ForkSourceRef, GitContext, PreRunPushOutcome, RunSpec};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunGoal;
use fabro_types::{WorkflowSettings, fixtures};

fn templated_settings() -> WorkflowSettings {
    let mut settings = WorkflowSettings::default();
    settings.run.goal = Some(RunGoal::Inline(InterpString::parse("Ship {{ env.TASK }}")));
    settings
}

#[test]
fn run_spec_round_trips_templated_settings() {
    let record = RunSpec {
        run_id:           fixtures::RUN_1,
        settings:         templated_settings(),
        graph:            Graph::new("ship"),
        workflow_slug:    Some("demo".to_string()),
        source_directory: Some("/Users/client/project".to_string()),
        labels:           HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance:       None,
        manifest_blob:    None,
        definition_blob:  None,
        git:              Some(GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          Some("abc123".to_string()),
            dirty:        DirtyStatus::Clean,
            push_outcome: PreRunPushOutcome::Succeeded {
                remote: "origin".to_string(),
                branch: "main".to_string(),
            },
        }),
        fork_source_ref:  Some(ForkSourceRef {
            source_run_id:  fixtures::RUN_2,
            checkpoint_sha: "def456".to_string(),
        }),
        in_place:         false,
    };

    let json = serde_json::to_value(&record).expect("record should serialize");
    assert!(json.get("working_directory").is_none());
    assert!(json.get("host_repo_path").is_none());
    assert_eq!(json["source_directory"], "/Users/client/project");
    assert_eq!(
        json["git"]["origin_url"],
        "https://github.com/fabro-sh/fabro.git"
    );
    assert_eq!(json["git"]["branch"], "main");
    assert_eq!(json["git"]["sha"], "abc123");
    assert_eq!(json["git"]["dirty"], "clean");
    assert_eq!(json["git"]["push_outcome"]["type"], "succeeded");
    assert_eq!(json["fork_source_ref"]["checkpoint_sha"], "def456");
    assert_eq!(json["in_place"], false);

    let round_trip: RunSpec =
        serde_json::from_value(json.clone()).expect("record should deserialize");

    assert_eq!(
        serde_json::to_value(&round_trip).expect("round-trip should serialize"),
        json
    );
    assert_eq!(
        round_trip.settings.run.goal,
        Some(RunGoal::Inline(InterpString::parse("Ship {{ env.TASK }}")))
    );
}
