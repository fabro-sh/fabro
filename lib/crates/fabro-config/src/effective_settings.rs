use anyhow::{Result, anyhow};
use fabro_types::Settings;

use crate::ConfigLayer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveSettingsMode {
    LocalOnly,
    RemoteServer,
    LocalDaemon,
}

#[derive(Clone, Debug, Default)]
pub struct EffectiveSettingsLayers {
    pub args: ConfigLayer,
    pub workflow: ConfigLayer,
    pub project: ConfigLayer,
    pub user: ConfigLayer,
}

impl EffectiveSettingsLayers {
    #[must_use]
    pub fn new(
        args: ConfigLayer,
        workflow: ConfigLayer,
        project: ConfigLayer,
        user: ConfigLayer,
    ) -> Self {
        Self {
            args,
            workflow,
            project,
            user,
        }
    }
}

pub fn resolve_settings(
    layers: EffectiveSettingsLayers,
    server_settings: Option<&Settings>,
    mode: EffectiveSettingsMode,
) -> Result<Settings> {
    let EffectiveSettingsLayers {
        args,
        mut workflow,
        mut project,
        mut user,
    } = layers;

    match mode {
        EffectiveSettingsMode::LocalOnly => args
            .combine(workflow)
            .combine(project)
            .combine(user)
            .resolve(),
        EffectiveSettingsMode::RemoteServer | EffectiveSettingsMode::LocalDaemon => {
            let server_settings = server_settings.ok_or_else(|| {
                anyhow!("server settings are required for server-targeted settings resolution")
            })?;
            strip_server_owned_fields(&mut workflow);
            strip_server_owned_fields(&mut project);
            strip_server_owned_fields(&mut user);

            let server_defaults = match mode {
                EffectiveSettingsMode::RemoteServer => server_defaults_layer(server_settings)?,
                EffectiveSettingsMode::LocalDaemon => {
                    local_daemon_server_overrides_layer(server_settings)?
                }
                EffectiveSettingsMode::LocalOnly => unreachable!(),
            };

            let mut settings = args
                .combine(workflow)
                .combine(project)
                .combine(user)
                .combine(server_defaults)
                .resolve()?;
            settings
                .storage_dir
                .clone_from(&server_settings.storage_dir);
            Ok(settings)
        }
    }
}

fn server_defaults_layer(settings: &Settings) -> Result<ConfigLayer> {
    let mut layer: ConfigLayer = serde_json::from_value(serde_json::to_value(settings)?)?;
    // Run manifests carry their own dry-run intent. Do not let a daemon's
    // startup-time fallback mode silently force every submitted run/preflight
    // into simulation.
    layer.dry_run = None;
    Ok(layer)
}

fn local_daemon_server_overrides_layer(settings: &Settings) -> Result<ConfigLayer> {
    let layer = server_defaults_layer(settings)?;
    Ok(ConfigLayer {
        storage_dir: layer.storage_dir,
        max_concurrent_runs: layer.max_concurrent_runs,
        web: layer.web,
        api: layer.api,
        features: layer.features,
        ..Default::default()
    })
}

fn strip_server_owned_fields(layer: &mut ConfigLayer) {
    layer.server = None;
    layer.exec = None;
    layer.storage_dir = None;
    layer.max_concurrent_runs = None;
    layer.web = None;
    layer.api = None;
    layer.features = None;
    layer.log = None;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{EffectiveSettingsLayers, EffectiveSettingsMode, resolve_settings};
    use crate::ConfigLayer;

    fn layer(source: &str) -> ConfigLayer {
        toml::from_str(source).expect("config layer fixture should parse")
    }

    #[test]
    fn local_only_merges_project_and_user_layers() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                ConfigLayer::default(),
                layer(
                    r#"
[llm]
model = "project-model"

[vars]
project_only = "1"
shared = "project"
"#,
                ),
                layer(
                    r#"
storage_dir = "/tmp/local-storage"

[llm]
provider = "openai"

[vars]
user_only = "1"
shared = "user"
"#,
                ),
            ),
            None,
            EffectiveSettingsMode::LocalOnly,
        )
        .unwrap();

        let llm = settings.llm.expect("llm config");
        assert_eq!(llm.model.as_deref(), Some("project-model"));
        assert_eq!(llm.provider.as_deref(), Some("openai"));
        assert_eq!(
            settings.storage_dir,
            Some(PathBuf::from("/tmp/local-storage"))
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("project_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("user_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings.vars.as_ref().and_then(|vars| vars.get("shared")),
            Some(&"project".to_string())
        );
    }

    #[test]
    fn local_only_merges_workflow_project_and_user_layers() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                layer(
                    r#"
goal = "workflow goal"

[llm]
model = "workflow-model"

[vars]
workflow_only = "1"
shared = "workflow"
"#,
                ),
                layer(
                    r#"
[llm]
model = "project-model"

[vars]
project_only = "1"
shared = "project"
"#,
                ),
                layer(
                    r#"
[llm]
provider = "openai"

[vars]
user_only = "1"
shared = "user"
"#,
                ),
            ),
            None,
            EffectiveSettingsMode::LocalOnly,
        )
        .unwrap();

        let llm = settings.llm.expect("llm config");
        assert_eq!(settings.goal.as_deref(), Some("workflow goal"));
        assert_eq!(llm.model.as_deref(), Some("workflow-model"));
        assert_eq!(llm.provider.as_deref(), Some("openai"));
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("workflow_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("project_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("user_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings.vars.as_ref().and_then(|vars| vars.get("shared")),
            Some(&"workflow".to_string())
        );
    }

    #[test]
    fn remote_server_mode_merges_server_defaults_without_allowing_server_owned_local_overrides() {
        let server_settings: fabro_types::Settings = toml::from_str(
            r#"
storage_dir = "/srv/fabro"
max_concurrent_runs = 9
dry_run = true

[vars]
server_only = "1"
shared = "server"
"#,
        )
        .unwrap();

        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                ConfigLayer::default(),
                layer(
                    r#"
storage_dir = "/tmp/local-storage"
max_concurrent_runs = 3

[vars]
project_only = "1"
shared = "project"
"#,
                ),
                ConfigLayer::default(),
            ),
            Some(&server_settings),
            EffectiveSettingsMode::RemoteServer,
        )
        .unwrap();

        assert_eq!(settings.storage_dir, Some(PathBuf::from("/srv/fabro")));
        assert_eq!(settings.max_concurrent_runs, Some(9));
        assert_eq!(settings.dry_run, None);
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("server_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("project_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings.vars.as_ref().and_then(|vars| vars.get("shared")),
            Some(&"project".to_string())
        );
    }

    #[test]
    fn remote_server_mode_merges_workflow_project_user_and_server_layers() {
        let server_settings: fabro_types::Settings = toml::from_str(
            r#"
storage_dir = "/srv/fabro"

[vars]
server_only = "1"
shared = "server"
"#,
        )
        .unwrap();

        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                layer(
                    r#"
[llm]
model = "workflow-model"

[vars]
workflow_only = "1"
shared = "workflow"
"#,
                ),
                layer(
                    r#"
[vars]
project_only = "1"
shared = "project"
"#,
                ),
                layer(
                    r#"
[llm]
provider = "openai"

[vars]
user_only = "1"
"#,
                ),
            ),
            Some(&server_settings),
            EffectiveSettingsMode::RemoteServer,
        )
        .unwrap();

        let llm = settings.llm.expect("llm config");
        assert_eq!(llm.model.as_deref(), Some("workflow-model"));
        assert_eq!(llm.provider.as_deref(), Some("openai"));
        assert_eq!(settings.storage_dir, Some(PathBuf::from("/srv/fabro")));
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("workflow_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("project_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("user_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings
                .vars
                .as_ref()
                .and_then(|vars| vars.get("server_only")),
            Some(&"1".to_string())
        );
        assert_eq!(
            settings.vars.as_ref().and_then(|vars| vars.get("shared")),
            Some(&"workflow".to_string())
        );
    }

    #[test]
    fn local_daemon_mode_only_applies_server_owned_overrides() {
        let server_settings: fabro_types::Settings = toml::from_str(
            r#"
storage_dir = "/srv/fabro"
max_concurrent_runs = 7

[llm]
model = "server-model"

[vars]
server_only = "1"
"#,
        )
        .unwrap();

        let settings = resolve_settings(
            EffectiveSettingsLayers::default(),
            Some(&server_settings),
            EffectiveSettingsMode::LocalDaemon,
        )
        .unwrap();

        assert_eq!(settings.storage_dir, Some(PathBuf::from("/srv/fabro")));
        assert_eq!(settings.max_concurrent_runs, Some(7));
        assert_eq!(settings.llm, None);
        assert_eq!(settings.vars, None);
    }
}
