use fabro_types::settings::run::{EnvironmentLifecycleSettings, RunLifecyclePolicy};

#[test]
fn legacy_lifecycle_defaults_keep_deletion_and_terminal_stop_independent() {
    let legacy: EnvironmentLifecycleSettings = serde_json::from_value(serde_json::json!({
        "preserve": false,
        "auto_stop": null
    }))
    .unwrap();
    assert_eq!(legacy, EnvironmentLifecycleSettings::default());
    assert_eq!(legacy.run_policy(), RunLifecyclePolicy {
        preserve:         false,
        stop_on_terminal: true,
    });
}

#[test]
fn lifecycle_policy_preserves_all_combinations_and_the_existing_wire_shape() {
    for preserve in [false, true] {
        for stop_on_terminal in [false, true] {
            for auto_stop in [serde_json::Value::Null, serde_json::json!("30m")] {
                let wire = serde_json::json!({
                    "preserve": preserve,
                    "stop_on_terminal": stop_on_terminal,
                    "auto_stop": auto_stop,
                });
                let lifecycle: EnvironmentLifecycleSettings =
                    serde_json::from_value(wire.clone()).unwrap();
                assert_eq!(lifecycle.run_policy(), RunLifecyclePolicy {
                    preserve,
                    stop_on_terminal
                });
                let encoded = serde_json::to_value(&lifecycle).unwrap();
                let restored: EnvironmentLifecycleSettings =
                    serde_json::from_value(encoded.clone()).unwrap();
                assert_eq!(restored, lifecycle);
                assert_eq!(encoded, wire);
            }
        }
    }
}
