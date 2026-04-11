use std::collections::HashMap;
use std::path::PathBuf;

use fabro_types::fixtures;
use fabro_types::graph::Graph;
use fabro_types::run::RunRecord;
use fabro_types::settings::run::{RunGoalLayer, RunLayer};
use fabro_types::settings::server::{
    GithubIntegrationLayer, ServerIntegrationsLayer, ServerLayer, ServerStorageLayer,
};
use fabro_types::settings::{InterpString, SettingsLayer};

fn templated_settings() -> SettingsLayer {
    SettingsLayer {
        version: Some(1),
        run: Some(RunLayer {
            goal: Some(RunGoalLayer::Inline(InterpString::parse(
                "Ship {{ env.TASK }}",
            ))),
            ..RunLayer::default()
        }),
        server: Some(ServerLayer {
            storage: Some(ServerStorageLayer {
                root: Some(InterpString::parse("{{ env.FABRO_STORAGE }}")),
            }),
            integrations: Some(ServerIntegrationsLayer {
                github: Some(GithubIntegrationLayer {
                    app_id: Some(InterpString::parse("{{ env.GITHUB_APP_ID }}")),
                    ..GithubIntegrationLayer::default()
                }),
                ..ServerIntegrationsLayer::default()
            }),
            ..ServerLayer::default()
        }),
        ..SettingsLayer::default()
    }
}

#[test]
fn run_record_round_trips_templated_settings() {
    let record = RunRecord {
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
    let round_trip: RunRecord =
        serde_json::from_value(json.clone()).expect("record should deserialize");

    assert_eq!(
        serde_json::to_value(&round_trip).expect("round-trip should serialize"),
        json
    );
    assert_eq!(
        round_trip
            .settings
            .run
            .as_ref()
            .and_then(|run| run.goal.as_ref()),
        Some(&RunGoalLayer::Inline(InterpString::parse(
            "Ship {{ env.TASK }}"
        )))
    );
    assert_eq!(
        round_trip
            .settings
            .server
            .as_ref()
            .and_then(|server| server.storage.as_ref())
            .and_then(|storage| storage.root.as_ref())
            .map(InterpString::as_source),
        Some("{{ env.FABRO_STORAGE }}".to_string())
    );
    assert_eq!(
        round_trip
            .settings
            .server
            .as_ref()
            .and_then(|server| server.integrations.as_ref())
            .and_then(|integrations| integrations.github.as_ref())
            .and_then(|github| github.app_id.as_ref())
            .map(InterpString::as_source),
        Some("{{ env.GITHUB_APP_ID }}".to_string())
    );
}
