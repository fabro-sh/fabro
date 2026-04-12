use fabro_types::settings::features::{FeaturesLayer, FeaturesSettings};

use super::ResolveError;

pub fn resolve_features(
    layer: &FeaturesLayer,
    _errors: &mut Vec<ResolveError>,
) -> FeaturesSettings {
    FeaturesSettings {
        session_sandboxes: layer.session_sandboxes.unwrap_or(false),
    }
}
