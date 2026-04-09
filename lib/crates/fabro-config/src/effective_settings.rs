//! Effective settings resolution: combine layers into one resolved [`Settings`].
//!
//! Shared layered domains (`project`, `workflow`, `run`, `features`) merge
//! across all three config files (settings.toml, fabro.toml, workflow.toml).
//! Owner-specific domains (`cli`, `server`) are consumed only from the local
//! `~/.fabro/settings.toml` plus explicit process-local overrides — their
//! stanzas in `fabro.toml` and `workflow.toml` remain schema-valid but inert.

use anyhow::{Result, anyhow};
use fabro_types::Settings;
use fabro_types::settings::v2::SettingsFile;

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
        user,
    } = layers;

    match mode {
        EffectiveSettingsMode::LocalOnly => Ok(args
            .combine(workflow)
            .combine(project)
            .combine(user)
            .resolve()),
        EffectiveSettingsMode::RemoteServer | EffectiveSettingsMode::LocalDaemon => {
            let server_settings = server_settings.ok_or_else(|| {
                anyhow!("server settings are required for server-targeted settings resolution")
            })?;
            strip_owner_domains(workflow.as_v2_mut());
            strip_owner_domains(project.as_v2_mut());
            let mut stripped_user = user;
            strip_owner_domains(stripped_user.as_v2_mut());

            let server_defaults = server_defaults_layer(server_settings);

            let mut settings = args
                .combine(workflow)
                .combine(project)
                .combine(stripped_user)
                .resolve();

            match mode {
                EffectiveSettingsMode::RemoteServer => {
                    apply_server_defaults(&mut settings, &server_defaults);
                }
                EffectiveSettingsMode::LocalDaemon => {
                    apply_local_daemon_overrides(&mut settings, &server_defaults);
                }
                EffectiveSettingsMode::LocalOnly => unreachable!(),
            }
            settings
                .storage_dir
                .clone_from(&server_settings.storage_dir);
            Ok(settings)
        }
    }
}

fn strip_owner_domains(file: &mut SettingsFile) {
    file.cli = None;
    file.server = None;
}

fn server_defaults_layer(settings: &Settings) -> Settings {
    let mut out = settings.clone();
    // Run manifests carry their own dry-run intent. Do not let a daemon's
    // startup-time fallback mode silently force every submitted run/preflight
    // into simulation.
    out.dry_run = None;
    out
}

fn apply_server_defaults(settings: &mut Settings, server: &Settings) {
    if settings.storage_dir.is_none() {
        settings.storage_dir.clone_from(&server.storage_dir);
    }
    if settings.max_concurrent_runs.is_none() {
        settings.max_concurrent_runs = server.max_concurrent_runs;
    }
    if settings.artifact_storage.is_none() {
        settings
            .artifact_storage
            .clone_from(&server.artifact_storage);
    }
    if settings.web.is_none() {
        settings.web.clone_from(&server.web);
    }
    if settings.api.is_none() {
        settings.api.clone_from(&server.api);
    }
    if settings.features.is_none() {
        settings.features.clone_from(&server.features);
    }
    if settings.log.is_none() {
        settings.log.clone_from(&server.log);
    }
    if settings.git.is_none() {
        settings.git.clone_from(&server.git);
    }
    if settings.vars.is_none() {
        settings.vars.clone_from(&server.vars);
    } else if let (Some(local), Some(server_vars)) = (settings.vars.as_mut(), server.vars.as_ref())
    {
        for (k, v) in server_vars {
            local.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

fn apply_local_daemon_overrides(settings: &mut Settings, server: &Settings) {
    settings.storage_dir.clone_from(&server.storage_dir);
    settings.max_concurrent_runs = server.max_concurrent_runs;
    settings
        .artifact_storage
        .clone_from(&server.artifact_storage);
    settings.web.clone_from(&server.web);
    settings.api.clone_from(&server.api);
    settings.features.clone_from(&server.features);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{EffectiveSettingsLayers, EffectiveSettingsMode, resolve_settings};
    use crate::ConfigLayer;

    fn layer(source: &str) -> ConfigLayer {
        ConfigLayer::parse(source).expect("v2 fixture should parse")
    }

    #[test]
    fn local_only_merges_project_and_user_layers() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                ConfigLayer::default(),
                layer(
                    r#"
_version = 1

[run.model]
name = "project-model"

[run.inputs]
project_only = "1"
shared = "project"
"#,
                ),
                layer(
                    r#"
_version = 1

[server.storage]
root = "/tmp/local-storage"

[run.model]
provider = "openai"

[run.inputs]
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
        // Per R22, run.inputs replaces wholesale — the winning layer is the
        // highest-precedence layer that sets `inputs` (project here, since it
        // wins over user).
        let vars = settings.vars.as_ref().unwrap();
        assert_eq!(vars.get("project_only"), Some(&"1".to_string()));
        assert_eq!(vars.get("shared"), Some(&"project".to_string()));
        assert!(
            vars.get("user_only").is_none(),
            "project.inputs should replace user.inputs wholesale"
        );
    }

    #[test]
    fn local_only_merges_workflow_project_user() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                layer(
                    r#"
_version = 1

[run]
goal = "workflow goal"

[run.model]
name = "workflow-model"
"#,
                ),
                layer(
                    r#"
_version = 1

[run.model]
name = "project-model"
"#,
                ),
                layer(
                    r#"
_version = 1

[run.model]
provider = "openai"
"#,
                ),
            ),
            None,
            EffectiveSettingsMode::LocalOnly,
        )
        .unwrap();

        assert_eq!(settings.goal.as_deref(), Some("workflow goal"));
        let llm = settings.llm.expect("llm config");
        assert_eq!(llm.model.as_deref(), Some("workflow-model"));
        assert_eq!(llm.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn cli_and_server_domains_from_fabro_toml_are_inert_under_remote_mode() {
        let server_settings: fabro_types::Settings = fabro_types::Settings {
            storage_dir: Some(PathBuf::from("/srv/fabro")),
            max_concurrent_runs: Some(9),
            ..Default::default()
        };

        let project_with_server = layer(
            r#"
_version = 1

[run]
goal = "project goal"

[server.storage]
root = "/tmp/should-be-inert"
"#,
        );

        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                ConfigLayer::default(),
                ConfigLayer::default(),
                project_with_server,
                ConfigLayer::default(),
            ),
            Some(&server_settings),
            EffectiveSettingsMode::RemoteServer,
        )
        .unwrap();

        assert_eq!(settings.storage_dir, Some(PathBuf::from("/srv/fabro")));
        assert_eq!(settings.goal.as_deref(), Some("project goal"));
    }

    #[test]
    fn local_daemon_mode_only_applies_server_owned_overrides() {
        let server_settings: fabro_types::Settings = fabro_types::Settings {
            storage_dir: Some(PathBuf::from("/srv/fabro")),
            max_concurrent_runs: Some(7),
            ..Default::default()
        };

        let settings = resolve_settings(
            EffectiveSettingsLayers::default(),
            Some(&server_settings),
            EffectiveSettingsMode::LocalDaemon,
        )
        .unwrap();

        assert_eq!(settings.storage_dir, Some(PathBuf::from("/srv/fabro")));
        assert_eq!(settings.max_concurrent_runs, Some(7));
    }
}
