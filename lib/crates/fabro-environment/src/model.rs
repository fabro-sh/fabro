use std::collections::BTreeMap;
use std::path::Path;

use fabro_config::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer, EnvironmentLifecycleLayer,
    EnvironmentNetworkLayer, EnvironmentResourcesLayer, StickyMap,
};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentResourcesSettings,
    EnvironmentSettings,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

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
    pub(crate) async fn from_legacy_path(
        id: EnvironmentId,
        bytes: &[u8],
        path: &Path,
    ) -> Result<Self, EnvironmentStoreError> {
        let mut persisted = parse_persisted(bytes, path)?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        inline_layer_dockerfile_paths(&mut persisted, base_dir).await?;
        let settings = resolve_environment(&persisted)?;
        Self::from_settings(id, &settings)
    }

    pub(crate) fn from_settings(
        id: EnvironmentId,
        settings: &EnvironmentSettings,
    ) -> Result<Self, EnvironmentStoreError> {
        reject_dockerfile_paths(settings)?;
        let persisted = environment_settings_to_layer(settings);
        let settings = resolve_environment(&persisted)?;
        let bytes = canonical_bytes(&persisted).into_bytes();
        let revision = EnvironmentRevision::from_bytes(&bytes);
        Ok(Self {
            id,
            revision,
            settings,
        })
    }

    pub(crate) fn from_row(
        id: EnvironmentId,
        revision: EnvironmentRevision,
        layer: &EnvironmentLayer,
    ) -> Result<Self, EnvironmentStoreError> {
        let settings = resolve_environment(layer)?;
        Ok(Self {
            id,
            revision,
            settings,
        })
    }

    /// Builds an in-memory environment from settings without touching the
    /// filesystem. Unlike [`from_settings`], this never inlines Dockerfile
    /// paths, so it stays synchronous — suitable for reserved environments
    /// (e.g. `local`) that carry no Dockerfile and are never persisted.
    pub(crate) fn synthetic(
        id: EnvironmentId,
        settings: &EnvironmentSettings,
    ) -> Result<Self, EnvironmentStoreError> {
        let persisted = environment_settings_to_layer(settings);
        let settings = resolve_environment(&persisted)?;
        let bytes = canonical_bytes(&persisted).into_bytes();
        let revision = EnvironmentRevision::from_bytes(&bytes);
        Ok(Self {
            id,
            revision,
            settings,
        })
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
    if let Some(cwd) = layer.cwd.as_deref() {
        doc["cwd"] = value(cwd);
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

async fn inline_layer_dockerfile_paths(
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
    let content = fs::read_to_string(&path).await.map_err(|source| {
        EnvironmentValidationError::DockerfileRead {
            path: path.clone(),
            source,
        }
    })?;
    image.dockerfile = Some(EnvironmentDockerfileLayer::Inline(content));
    Ok(())
}

fn reject_dockerfile_paths(
    settings: &EnvironmentSettings,
) -> Result<(), EnvironmentValidationError> {
    if matches!(
        settings.image.dockerfile,
        Some(DockerfileSource::Path { .. })
    ) {
        return Err(EnvironmentValidationError::DockerfilePathUnsupported);
    }
    Ok(())
}

fn environment_settings_to_layer(settings: &EnvironmentSettings) -> EnvironmentLayer {
    EnvironmentLayer {
        provider:  Some(settings.provider.to_string()),
        cwd:       settings.cwd.clone(),
        image:     image_settings_to_layer(&settings.image),
        resources: resources_settings_to_layer(&settings.resources),
        network:   network_settings_to_layer(&settings.network),
        lifecycle: lifecycle_settings_to_layer(&settings.lifecycle),
        labels:    StickyMap::from(settings.labels.clone()),
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

#[expect(
    clippy::disallowed_methods,
    reason = "serializing the InterpString map back to its TOML source form; tokens must \
              round-trip unresolved (resolution happens at consumption, not serialization)"
)]
fn append_interp_map(root: &mut Table, name: &str, map: &StickyMap<InterpString>) {
    if map.is_empty() {
        return;
    }
    let table = ensure_table(root, &[name]);
    for (key, entry) in sorted_map(map) {
        table[key] = value(entry.as_source());
    }
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
