use crate::{SettingsLayer, UserSettingsBuilder};

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
    let features = UserSettingsBuilder::from_toml(
        r"
_version = 1

[features]
session_sandboxes = true
",
    )
    .expect("features should resolve")
    .features;

    assert!(features.session_sandboxes);
}
