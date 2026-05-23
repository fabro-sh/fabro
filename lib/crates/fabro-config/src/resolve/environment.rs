use std::collections::HashMap;

use fabro_types::settings::run::{
    DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentProvider,
    EnvironmentResourcesSettings, EnvironmentSettings, EnvironmentVolumeSettings,
    RunEnvironmentSettings,
};

use super::ResolveError;
use crate::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer, EnvironmentLifecycleLayer,
    EnvironmentNetworkLayer, EnvironmentResourcesLayer, EnvironmentVolumeLayer, MergeMap,
    RunEnvironmentLayer,
};

pub(crate) fn resolve_environments(
    layers: &MergeMap<EnvironmentLayer>,
    errors: &mut Vec<ResolveError>,
) -> HashMap<String, EnvironmentSettings> {
    layers
        .iter()
        .map(|(slug, layer)| {
            let path = format!("environments.{slug}");
            (
                slug.clone(),
                resolve_environment_layer(layer, &path, errors),
            )
        })
        .collect()
}

pub(crate) fn resolve_run_environment(
    layer: Option<&RunEnvironmentLayer>,
    environments: &HashMap<String, EnvironmentSettings>,
    errors: &mut Vec<ResolveError>,
) -> RunEnvironmentSettings {
    let layer = layer.expect("defaults.toml should provide run.environment defaults");
    let id = layer.id.clone().unwrap_or_else(|| {
        errors.push(ResolveError::Missing {
            path: "run.environment.id".to_string(),
        });
        "default".to_string()
    });

    let Some(base) = environments.get(&id) else {
        errors.push(ResolveError::Invalid {
            path:   "run.environment.id".to_string(),
            reason: format!("unknown environment: {id}"),
        });
        return RunEnvironmentSettings::from_environment(id, EnvironmentSettings::default());
    };

    let mut environment = base.clone();
    apply_run_environment_overrides(&mut environment, layer, errors);
    validate_provider_capabilities(&environment, "run.environment", errors);
    RunEnvironmentSettings::from_environment(id, environment)
}

fn resolve_environment_layer(
    layer: &EnvironmentLayer,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> EnvironmentSettings {
    let provider = if let Some(raw) = layer.provider.as_deref() {
        parse_provider(raw, &format!("{path}.provider"), errors)
    } else {
        errors.push(ResolveError::Missing {
            path: format!("{path}.provider"),
        });
        EnvironmentProvider::Local
    };

    let environment = EnvironmentSettings {
        provider,
        image: resolve_image(layer.image.as_ref()),
        resources: resolve_resources(layer.resources.as_ref()),
        network: resolve_network(layer.network.as_ref(), &format!("{path}.network"), errors),
        lifecycle: resolve_lifecycle(layer.lifecycle.as_ref()),
        labels: layer.labels.clone().into_inner(),
        volumes: resolve_volumes(layer.volumes.as_deref()),
        env: layer.env.clone().into_inner(),
    };
    validate_daytona_snapshot_name(&environment, path, errors);
    environment
}

fn parse_provider(raw: &str, path: &str, errors: &mut Vec<ResolveError>) -> EnvironmentProvider {
    if let Ok(provider) = raw.parse::<EnvironmentProvider>() {
        provider
    } else {
        errors.push(ResolveError::Invalid {
            path:   path.to_string(),
            reason: format!("unknown environment provider: {raw}"),
        });
        EnvironmentProvider::Local
    }
}

fn resolve_image(layer: Option<&EnvironmentImageLayer>) -> EnvironmentImageSettings {
    let Some(layer) = layer else {
        return EnvironmentImageSettings::default();
    };
    EnvironmentImageSettings {
        reference:  layer.reference.clone(),
        dockerfile: layer.dockerfile.as_ref().map(dockerfile_source),
    }
}

fn resolve_resources(layer: Option<&EnvironmentResourcesLayer>) -> EnvironmentResourcesSettings {
    let Some(layer) = layer else {
        return EnvironmentResourcesSettings::default();
    };
    EnvironmentResourcesSettings {
        cpu:    layer.cpu,
        memory: layer.memory,
        disk:   layer.disk,
    }
}

fn resolve_network(
    layer: Option<&EnvironmentNetworkLayer>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> EnvironmentNetworkSettings {
    let Some(layer) = layer else {
        return EnvironmentNetworkSettings::default();
    };

    for (index, cidr) in layer.allow.iter().enumerate() {
        if cidr.parse::<ipnet::IpNet>().is_err() {
            errors.push(ResolveError::Invalid {
                path:   format!("{path}.allow[{index}]"),
                reason: format!("invalid CIDR: {cidr}"),
            });
        }
    }

    let mode = match layer.mode.as_deref() {
        Some(raw) => parse_network_mode(raw, &format!("{path}.mode"), errors),
        None if layer.allow.is_empty() => EnvironmentNetworkMode::AllowAll,
        None => EnvironmentNetworkMode::CidrAllowList,
    };

    EnvironmentNetworkSettings {
        mode,
        allow: layer.allow.clone(),
    }
}

fn parse_network_mode(
    raw: &str,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> EnvironmentNetworkMode {
    if let Ok(mode) = raw.parse::<EnvironmentNetworkMode>() {
        mode
    } else {
        errors.push(ResolveError::Invalid {
            path:   path.to_string(),
            reason: format!("unknown environment network mode: {raw}"),
        });
        EnvironmentNetworkMode::AllowAll
    }
}

fn resolve_lifecycle(layer: Option<&EnvironmentLifecycleLayer>) -> EnvironmentLifecycleSettings {
    let Some(layer) = layer else {
        return EnvironmentLifecycleSettings::default();
    };
    EnvironmentLifecycleSettings {
        preserve:         layer.preserve.unwrap_or(false),
        stop_on_terminal: layer.stop_on_terminal.unwrap_or(true),
        auto_stop:        layer.auto_stop,
    }
}

fn resolve_volumes(layers: Option<&[EnvironmentVolumeLayer]>) -> Vec<EnvironmentVolumeSettings> {
    layers
        .unwrap_or(&[])
        .iter()
        .map(|volume| EnvironmentVolumeSettings {
            id:         volume.id.clone(),
            mount_path: volume.mount_path.clone(),
            subpath:    volume.subpath.clone(),
        })
        .collect()
}

fn apply_run_environment_overrides(
    environment: &mut EnvironmentSettings,
    layer: &RunEnvironmentLayer,
    errors: &mut Vec<ResolveError>,
) {
    if let Some(image) = layer.image.as_ref() {
        apply_image_override(&mut environment.image, image);
    }
    if let Some(resources) = layer.resources.as_ref() {
        apply_resources_override(&mut environment.resources, resources);
    }
    if let Some(network) = layer.network.as_ref() {
        apply_network_override(
            &mut environment.network,
            network,
            "run.environment.network",
            errors,
        );
    }
    if let Some(lifecycle) = layer.lifecycle.as_ref() {
        apply_lifecycle_override(&mut environment.lifecycle, lifecycle);
    }
    environment.labels.extend(layer.labels.clone().into_inner());
    if let Some(volumes) = layer.volumes.as_deref() {
        environment.volumes = resolve_volumes(Some(volumes));
    }
    environment.env.extend(layer.env.clone().into_inner());
}

fn apply_image_override(target: &mut EnvironmentImageSettings, layer: &EnvironmentImageLayer) {
    if let Some(reference) = layer.reference.as_ref() {
        target.reference = Some(reference.clone());
    }
    if let Some(dockerfile) = layer.dockerfile.as_ref() {
        target.dockerfile = Some(dockerfile_source(dockerfile));
    }
}

fn apply_resources_override(
    target: &mut EnvironmentResourcesSettings,
    layer: &EnvironmentResourcesLayer,
) {
    if layer.cpu.is_some() {
        target.cpu = layer.cpu;
    }
    if layer.memory.is_some() {
        target.memory = layer.memory;
    }
    if layer.disk.is_some() {
        target.disk = layer.disk;
    }
}

fn apply_network_override(
    target: &mut EnvironmentNetworkSettings,
    layer: &EnvironmentNetworkLayer,
    path: &str,
    errors: &mut Vec<ResolveError>,
) {
    for (index, cidr) in layer.allow.iter().enumerate() {
        if cidr.parse::<ipnet::IpNet>().is_err() {
            errors.push(ResolveError::Invalid {
                path:   format!("{path}.allow[{index}]"),
                reason: format!("invalid CIDR: {cidr}"),
            });
        }
    }
    if let Some(raw) = layer.mode.as_deref() {
        target.mode = parse_network_mode(raw, &format!("{path}.mode"), errors);
    }
    if !layer.allow.is_empty() {
        target.allow.clone_from(&layer.allow);
    }
}

fn dockerfile_source(dockerfile: &EnvironmentDockerfileLayer) -> DockerfileSource {
    match dockerfile {
        EnvironmentDockerfileLayer::Inline(text) => DockerfileSource::Inline(text.clone()),
        EnvironmentDockerfileLayer::Path { path } => DockerfileSource::Path { path: path.clone() },
    }
}

fn apply_lifecycle_override(
    target: &mut EnvironmentLifecycleSettings,
    layer: &EnvironmentLifecycleLayer,
) {
    if let Some(preserve) = layer.preserve {
        target.preserve = preserve;
    }
    if let Some(stop_on_terminal) = layer.stop_on_terminal {
        target.stop_on_terminal = stop_on_terminal;
    }
    if layer.auto_stop.is_some() {
        target.auto_stop = layer.auto_stop;
    }
}

fn validate_daytona_snapshot_name(
    environment: &EnvironmentSettings,
    path: &str,
    errors: &mut Vec<ResolveError>,
) {
    if environment.provider == EnvironmentProvider::Daytona
        && environment.image.dockerfile.is_some()
        && environment.image.reference.is_none()
    {
        errors.push(ResolveError::Invalid {
            path:   format!("{path}.image"),
            reason: "daytona environments with image.dockerfile must also set image.ref"
                .to_string(),
        });
    }
}

fn validate_provider_capabilities(
    environment: &EnvironmentSettings,
    path: &str,
    errors: &mut Vec<ResolveError>,
) {
    validate_daytona_snapshot_name(environment, path, errors);
    match environment.provider {
        EnvironmentProvider::Local => {
            if matches!(
                environment.network.mode,
                EnvironmentNetworkMode::Block | EnvironmentNetworkMode::CidrAllowList
            ) {
                errors.push(ResolveError::Invalid {
                    path:   format!("{path}.network.mode"),
                    reason:
                        "local environments cannot enforce blocked or CIDR allow-list networking"
                            .to_string(),
                });
            }
        }
        EnvironmentProvider::Docker => {
            if environment.network.mode == EnvironmentNetworkMode::CidrAllowList {
                errors.push(ResolveError::Invalid {
                    path:   format!("{path}.network.mode"),
                    reason: "docker environments cannot enforce CIDR allow-list networking"
                        .to_string(),
                });
            }
        }
        EnvironmentProvider::Daytona => {}
    }
}
