use fabro_config::{UserSettingsBuilder, parse_settings_layer};
use fabro_types::settings::SettingsLayer;

#[test]
fn resolves_features_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let features = UserSettingsBuilder::from_layer(&settings)
        .expect("empty settings should resolve")
        .features;

    assert!(!features.session_sandboxes);
}

#[test]
fn resolves_session_sandboxes_flag() {
    let settings: SettingsLayer = parse_settings_layer(
        r"
_version = 1

[features]
session_sandboxes = true
",
    )
    .expect("fixture should parse");

    let features = UserSettingsBuilder::from_layer(&settings)
        .expect("features should resolve")
        .features;

    assert!(features.session_sandboxes);
}
