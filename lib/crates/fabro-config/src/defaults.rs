use std::sync::LazyLock;

use crate::SettingsLayer;

pub(crate) static DEFAULTS_LAYER: LazyLock<SettingsLayer> = LazyLock::new(|| {
    include_str!("defaults.toml")
        .parse::<SettingsLayer>()
        .expect("embedded defaults.toml must parse as a valid SettingsLayer")
});
