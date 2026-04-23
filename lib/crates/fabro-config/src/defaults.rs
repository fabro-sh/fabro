use std::sync::LazyLock;

use fabro_types::settings::{Combine, SettingsLayer};

use crate::parse_settings_layer;

static DEFAULTS_LAYER: LazyLock<SettingsLayer> = LazyLock::new(|| {
    parse_settings_layer(include_str!("defaults.toml"))
        .expect("embedded defaults.toml must parse as a valid SettingsLayer")
});

#[must_use]
pub fn defaults_layer() -> &'static SettingsLayer {
    &DEFAULTS_LAYER
}

#[must_use]
pub fn apply_builtin_defaults(layer: SettingsLayer) -> SettingsLayer {
    layer.combine(defaults_layer().clone())
}
