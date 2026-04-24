use fabro_types::settings::FeaturesNamespace;

use super::ResolveError;
use crate::FeaturesLayer;

pub fn resolve_features(
    layer: &FeaturesLayer,
    _errors: &mut Vec<ResolveError>,
) -> FeaturesNamespace {
    FeaturesNamespace {
        session_sandboxes: layer
            .session_sandboxes
            .expect("defaults.toml should provide features.session_sandboxes"),
    }
}
