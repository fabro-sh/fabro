use fabro_config::parse_settings_layer;
use fabro_config::resolve_features_from_file;
use fabro_types::settings::SettingsLayer;

#[test]
fn resolves_features_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let features = resolve_features_from_file(&settings).expect("empty settings should resolve");

    assert!(!features.session_sandboxes);
}

#[test]
fn resolves_session_sandboxes_flag() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[features]
session_sandboxes = true
"#,
    )
    .expect("fixture should parse");

    let features = resolve_features_from_file(&settings).expect("features should resolve");

    assert!(features.session_sandboxes);
}
