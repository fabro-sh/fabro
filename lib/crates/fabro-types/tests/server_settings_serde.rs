use fabro_types::settings::SettingsLayer;
use serde_json::json;

#[test]
fn settings_layer_round_trips_github_integration_strategy() {
    let source = json!({
        "_version": 1,
        "server": {
            "integrations": {
                "github": {
                    "strategy": "gh_cli",
                    "app_id": "{{ env.GITHUB_APP_ID }}"
                }
            }
        }
    });

    let settings: SettingsLayer =
        serde_json::from_value(source.clone()).expect("settings should deserialize");
    let round_trip = serde_json::to_value(&settings).expect("settings should serialize");

    assert_eq!(round_trip, source);
}
