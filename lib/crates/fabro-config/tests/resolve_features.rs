use fabro_config::resolve_features_from_file;
use fabro_types::settings::{SettingsFile, parse_settings_file};

#[test]
fn resolves_features_defaults_from_empty_settings() {
    let settings = SettingsFile::default();

    let features = resolve_features_from_file(&settings).expect("empty settings should resolve");

    assert!(!features.session_sandboxes);
}

#[test]
fn resolves_session_sandboxes_flag() {
    let settings: SettingsFile = parse_settings_file(
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
