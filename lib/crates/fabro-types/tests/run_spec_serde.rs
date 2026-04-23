use std::collections::HashMap;
use std::path::PathBuf;

use fabro_types::graph::Graph;
use fabro_types::run::RunSpec;
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
        run_id:            fixtures::RUN_1,
        settings:          templated_settings(),
        graph:             Graph::new("ship"),
        workflow_slug:     Some("demo".to_string()),
        working_directory: PathBuf::from("/tmp/project"),
        host_repo_path:    Some("/tmp/project".to_string()),
        repo_origin_url:   Some("https://github.com/fabro-sh/fabro.git".to_string()),
        base_branch:       Some("main".to_string()),
        labels:            HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance:        None,
        manifest_blob:     None,
        definition_blob:   None,
    };

    let json = serde_json::to_value(&record).expect("record should serialize");
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
