use std::collections::BTreeMap;
use std::path::Path;

use fabro_config::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer, EnvironmentLifecycleLayer,
    EnvironmentNetworkLayer, EnvironmentResourcesLayer, EnvironmentVolumeLayer, StickyMap,
};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentResourcesSettings,
    EnvironmentSettings, EnvironmentVolumeSettings,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

use crate::{
    EnvironmentId, EnvironmentRevision, EnvironmentStoreError, EnvironmentValidationError,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id:       EnvironmentId,
    pub revision: EnvironmentRevision,
    #[serde(flatten)]
    pub settings: EnvironmentSettings,
}

impl Environment {
    pub(crate) fn from_persisted_path(
        id: EnvironmentId,
        bytes: &[u8],
        path: &Path,
    ) -> Result<Self, EnvironmentStoreError> {
        let revision = EnvironmentRevision::from_bytes(bytes);
        let mut persisted = parse_persisted(bytes, path)?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        inline_layer_dockerfile_paths(&mut persisted, base_dir)?;
        let settings = resolve_environment(&persisted)?;
        Ok(Self {
            id,
            revision,
            settings,
        })
    }

    pub(crate) async fn from_settings(
        id: EnvironmentId,
        settings: EnvironmentSettings,
        dockerfile_base_dir: &Path,
    ) -> Result<(Self, Vec<u8>), EnvironmentStoreError> {
        let settings = inline_dense_dockerfile(settings, dockerfile_base_dir).await?;
        let persisted = environment_settings_to_layer(&settings);
        let settings = resolve_environment(&persisted)?;
        let bytes = canonical_bytes(&persisted).into_bytes();
        let revision = EnvironmentRevision::from_bytes(&bytes);
        Ok((
            Self {
                id,
                revision,
                settings,
            },
            bytes,
        ))
    }

    pub(crate) fn to_layer(&self) -> EnvironmentLayer {
        environment_settings_to_layer(&self.settings)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentDraft {
    pub id:       EnvironmentId,
    #[serde(flatten)]
    pub settings: EnvironmentSettings,
}

pub(crate) fn canonical_bytes(layer: &EnvironmentLayer) -> String {
    let mut doc = DocumentMut::new();
    if let Some(provider) = layer.provider.as_deref() {
        doc["provider"] = value(provider);
    }
    if let Some(image) = layer.image.as_ref() {
        append_image(doc.as_table_mut(), image);
    }
    if let Some(resources) = layer.resources.as_ref() {
        append_resources(doc.as_table_mut(), resources);
    }
    if let Some(network) = layer.network.as_ref() {
        append_network(doc.as_table_mut(), network);
    }
    if let Some(lifecycle) = layer.lifecycle.as_ref() {
        append_lifecycle(doc.as_table_mut(), lifecycle);
    }
    append_string_map(doc.as_table_mut(), "labels", &layer.labels);
    if let Some(volumes) = layer.volumes.as_deref() {
        append_volumes(doc.as_table_mut(), volumes);
    }
    append_interp_map(doc.as_table_mut(), "env", &layer.env);
    doc.to_string()
}

fn parse_persisted(bytes: &[u8], path: &Path) -> Result<EnvironmentLayer, EnvironmentStoreError> {
    let content = std::str::from_utf8(bytes)
        .map_err(|err| EnvironmentStoreError::invalid_utf8(path.to_path_buf(), err))?;
    toml::from_str(content).map_err(|err| EnvironmentStoreError::parse(path.to_path_buf(), err))
}

fn resolve_environment(
    layer: &EnvironmentLayer,
) -> Result<EnvironmentSettings, EnvironmentValidationError> {
    fabro_config::resolve_environment_layer(layer, "environment").map_err(|errors| {
        EnvironmentValidationError::InvalidSettings {
            errors: errors.into_iter().map(|err| err.to_string()).collect(),
        }
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "Dockerfile inlining runs during synchronous startup load before request handling."
)]
fn inline_layer_dockerfile_paths(
    layer: &mut EnvironmentLayer,
    base_dir: &Path,
) -> Result<(), EnvironmentValidationError> {
    let Some(image) = layer.image.as_mut() else {
        return Ok(());
    };
    let Some(EnvironmentDockerfileLayer::Path { path }) = image.dockerfile.as_ref() else {
        return Ok(());
    };
    let path = base_dir.join(path);
    let content = std::fs::read_to_string(&path).map_err(|source| {
        EnvironmentValidationError::DockerfileRead {
            path: path.clone(),
            source,
        }
    })?;
    image.dockerfile = Some(EnvironmentDockerfileLayer::Inline(content));
    Ok(())
}

async fn inline_dense_dockerfile(
    mut settings: EnvironmentSettings,
    base_dir: &Path,
) -> Result<EnvironmentSettings, EnvironmentValidationError> {
    let Some(DockerfileSource::Path { path }) = settings.image.dockerfile.as_ref() else {
        return Ok(settings);
    };
    let path = base_dir.join(path);
    let content = fs::read_to_string(&path).await.map_err(|source| {
        EnvironmentValidationError::DockerfileRead {
            path: path.clone(),
            source,
        }
    })?;
    settings.image.dockerfile = Some(DockerfileSource::Inline(content));
    Ok(settings)
}

fn environment_settings_to_layer(settings: &EnvironmentSettings) -> EnvironmentLayer {
    EnvironmentLayer {
        provider:  Some(settings.provider.to_string()),
        image:     image_settings_to_layer(&settings.image),
        resources: resources_settings_to_layer(&settings.resources),
        network:   network_settings_to_layer(&settings.network),
        lifecycle: lifecycle_settings_to_layer(&settings.lifecycle),
        labels:    StickyMap::from(settings.labels.clone()),
        volumes:   volumes_settings_to_layer(&settings.volumes),
        env:       StickyMap::from(settings.env.clone()),
    }
}

fn image_settings_to_layer(settings: &EnvironmentImageSettings) -> Option<EnvironmentImageLayer> {
    if settings.docker.is_none() && settings.dockerfile.is_none() {
        return None;
    }
    Some(EnvironmentImageLayer {
        docker:     settings.docker.clone(),
        dockerfile: settings.dockerfile.as_ref().map(dockerfile_source_to_layer),
    })
}

fn dockerfile_source_to_layer(source: &DockerfileSource) -> EnvironmentDockerfileLayer {
    match source {
        DockerfileSource::Inline(value) => EnvironmentDockerfileLayer::Inline(value.clone()),
        DockerfileSource::Path { path } => EnvironmentDockerfileLayer::Path { path: path.clone() },
    }
}

fn resources_settings_to_layer(
    settings: &EnvironmentResourcesSettings,
) -> Option<EnvironmentResourcesLayer> {
    if settings.cpu.is_none() && settings.memory.is_none() && settings.disk.is_none() {
        return None;
    }
    Some(EnvironmentResourcesLayer {
        cpu:    settings.cpu,
        memory: settings.memory,
        disk:   settings.disk,
    })
}

fn network_settings_to_layer(
    settings: &EnvironmentNetworkSettings,
) -> Option<EnvironmentNetworkLayer> {
    if settings.mode == EnvironmentNetworkMode::AllowAll && settings.allow.is_empty() {
        return None;
    }
    Some(EnvironmentNetworkLayer {
        mode:  Some(settings.mode.to_string()),
        allow: settings.allow.clone(),
    })
}

fn lifecycle_settings_to_layer(
    settings: &EnvironmentLifecycleSettings,
) -> Option<EnvironmentLifecycleLayer> {
    if !settings.preserve && settings.stop_on_terminal && settings.auto_stop.is_none() {
        return None;
    }
    Some(EnvironmentLifecycleLayer {
        preserve:         settings.preserve.then_some(true),
        stop_on_terminal: (!settings.stop_on_terminal).then_some(false),
        auto_stop:        settings.auto_stop,
    })
}

fn volumes_settings_to_layer(
    settings: &[EnvironmentVolumeSettings],
) -> Option<Vec<EnvironmentVolumeLayer>> {
    if settings.is_empty() {
        return None;
    }
    Some(settings.iter().map(volume_settings_to_layer).collect())
}

fn volume_settings_to_layer(settings: &EnvironmentVolumeSettings) -> EnvironmentVolumeLayer {
    EnvironmentVolumeLayer {
        id:         settings.id.clone(),
        mount_path: settings.mount_path.clone(),
        subpath:    settings.subpath.clone(),
    }
}

fn append_image(root: &mut Table, image: &EnvironmentImageLayer) {
    let table = ensure_table(root, &["image"]);
    if let Some(docker) = image.docker.as_deref() {
        table["docker"] = value(docker);
    }
    if let Some(dockerfile) = image.dockerfile.as_ref() {
        match dockerfile {
            EnvironmentDockerfileLayer::Inline(content) => {
                table["dockerfile"] = value(content.as_str());
            }
            EnvironmentDockerfileLayer::Path { path } => {
                let dockerfile_table = ensure_table(table, &["dockerfile"]);
                dockerfile_table["path"] = value(path.as_str());
            }
        }
    }
}

fn append_resources(root: &mut Table, resources: &EnvironmentResourcesLayer) {
    let table = ensure_table(root, &["resources"]);
    if let Some(cpu) = resources.cpu {
        table["cpu"] = value(i64::from(cpu));
    }
    if let Some(memory) = resources.memory {
        table["memory"] = value(memory.to_string());
    }
    if let Some(disk) = resources.disk {
        table["disk"] = value(disk.to_string());
    }
}

fn append_network(root: &mut Table, network: &EnvironmentNetworkLayer) {
    let table = ensure_table(root, &["network"]);
    if let Some(mode) = network.mode.as_deref() {
        table["mode"] = value(mode);
    }
    if !network.allow.is_empty() {
        table["allow"] = string_array(&network.allow);
    }
}

fn append_lifecycle(root: &mut Table, lifecycle: &EnvironmentLifecycleLayer) {
    let table = ensure_table(root, &["lifecycle"]);
    if let Some(preserve) = lifecycle.preserve {
        table["preserve"] = value(preserve);
    }
    if let Some(stop_on_terminal) = lifecycle.stop_on_terminal {
        table["stop_on_terminal"] = value(stop_on_terminal);
    }
    if let Some(auto_stop) = lifecycle.auto_stop {
        table["auto_stop"] = value(auto_stop.to_string());
    }
}

fn append_string_map(root: &mut Table, name: &str, map: &StickyMap<String>) {
    if map.is_empty() {
        return;
    }
    let table = ensure_table(root, &[name]);
    for (key, entry) in sorted_map(map) {
        table[key] = value(entry.as_str());
    }
}

fn append_interp_map(root: &mut Table, name: &str, map: &StickyMap<InterpString>) {
    if map.is_empty() {
        return;
    }
    let table = ensure_table(root, &[name]);
    for (key, entry) in sorted_map(map) {
        table[key] = value(entry.as_source());
    }
}

fn append_volumes(root: &mut Table, volumes: &[EnvironmentVolumeLayer]) {
    if volumes.is_empty() {
        return;
    }
    let mut array = ArrayOfTables::new();
    for volume in volumes {
        let mut table = Table::new();
        table["id"] = value(volume.id.as_str());
        table["mount_path"] = value(volume.mount_path.as_str());
        if let Some(subpath) = volume.subpath.as_deref() {
            table["subpath"] = value(subpath);
        }
        array.push(table);
    }
    root["volumes"] = Item::ArrayOfTables(array);
}

fn ensure_table<'a>(root: &'a mut Table, path: &[&str]) -> &'a mut Table {
    let mut current = root;
    for key in path {
        if !current.contains_key(key) {
            current[*key] = Item::Table(Table::new());
        }
        current = current[*key]
            .as_table_mut()
            .expect("environment canonical table should be a table");
    }
    current
}

fn string_array(values: &[String]) -> Item {
    let mut array = Array::new();
    for value in values {
        array.push(value.as_str());
    }
    Item::Value(Value::Array(array))
}

fn sorted_map<V>(map: &StickyMap<V>) -> BTreeMap<&String, &V> {
    map.iter().collect()
}
