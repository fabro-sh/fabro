use std::collections::HashMap;

use fabro_types::graph::Graph;
use fabro_types::run::{DirtyStatus, ForkSourceRef, GitContext, RunSpec};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunGoal;
use fabro_types::test_support::{test_run_provenance, test_workflow_version_id};
use fabro_types::{
    AutomationRef, GitRunTarget, PetriAdmission, ResolvedAutomationGitWorkflowSource, RunTarget,
    WorkflowSettings, fixtures,
};

fn templated_settings() -> WorkflowSettings {
    let mut settings = WorkflowSettings::default();
    settings.run.goal = Some(RunGoal::Inline(InterpString::parse("Ship {{ env.TASK }}")));
    settings
}

#[test]
fn run_spec_round_trips_templated_settings() {
    let record = RunSpec {
        run_id:              fixtures::RUN_1,
        settings:            templated_settings(),
        graph:               Graph::new("ship"),
        graph_source:        None,
        workflow_slug:       Some("demo".to_string()),
        workflow_version_id: Some(test_workflow_version_id()),
        target:              Some(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "main".to_string(),
            tag:    None,
            sha:    Some("abc123".to_string()),
        })),
        automation:          Some(AutomationRef {
            id:              "nightly".to_string(),
            name:            Some("Nightly".to_string()),
            trigger_id:      Some("schedule_1".to_string()),
            workflow_source: Some(Box::new(ResolvedAutomationGitWorkflowSource {
                repo:         "fabro-sh/workflows".to_string(),
                branch:       "main".to_string(),
                tag:          None,
                sha:          None,
                resolved_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            })),
        }),
        source_directory:    Some("/Users/client/project".to_string()),
        labels:              HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance:          test_run_provenance(),
        definition_blob:     None,
        spec_blob:           None,
        git:                 Some(GitContext {
            origin_url: "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:     "main".to_string(),
            sha:        Some("abc123".to_string()),
            dirty:      DirtyStatus::Clean,
        }),
        fork_source_ref:     Some(ForkSourceRef {
            source_run_id:  fixtures::RUN_2,
            checkpoint_sha: "def456".to_string(),
        }),
        admission:           PetriAdmission::default(),
    };

    let json = serde_json::to_value(&record).expect("record should serialize");
    assert!(json.get("working_directory").is_none());
    assert_eq!(
        json["admission"]["graph"]["digest"], "sha256:test-admission",
        "the spec names what Petri admitted"
    );
    assert!(json.get("host_repo_path").is_none());
    assert_eq!(json["source_directory"], "/Users/client/project");
    assert_eq!(
        json["git"]["origin_url"],
        "https://github.com/fabro-sh/fabro.git"
    );
    assert_eq!(json["git"]["branch"], "main");
    assert_eq!(json["git"]["sha"], "abc123");
    assert_eq!(json["git"]["dirty"], "clean");
    assert!(json["git"].get("push_outcome").is_none());
    assert_eq!(json["fork_source_ref"]["checkpoint_sha"], "def456");
    assert_eq!(json["automation"]["id"], "nightly");
    assert_eq!(json["automation"]["trigger_id"], "schedule_1");
    assert_eq!(json["automation"]["workflow_source"]["branch"], "main");
    assert_eq!(
        json["automation"]["workflow_source"]["resolved_sha"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        json["workflow_version_id"],
        test_workflow_version_id().to_string()
    );
    assert_eq!(json["target"]["kind"], "git");
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
