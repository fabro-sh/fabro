//! Effective settings resolution: combine layers into one resolved
//! [`SettingsLayer`].
//!
//! Shared layered domains (`project`, `workflow`, `run`, `features`) merge
//! across all three config files (settings.toml, .fabro/project.toml,
//! workflow.toml).
//! Owner-specific domains (`cli`, `server`) are consumed only from the local
//! `~/.fabro/settings.toml` plus explicit process-local overrides — their
//! stanzas in `.fabro/project.toml` and `workflow.toml` remain schema-valid but
//! inert.

use fabro_types::settings::SettingsLayer;
use fabro_types::settings::run::{RunExecutionLayer, RunLayer};
use fabro_types::settings::server::ServerLayer;

use crate::merge::combine_files;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveSettingsMode {
    LocalOnly,
    RemoteServer,
    LocalDaemon,
}

#[derive(Clone, Debug, Default)]
pub struct EffectiveSettingsLayers {
    pub args: SettingsLayer,
    pub workflow: SettingsLayer,
    pub project: SettingsLayer,
    pub user: SettingsLayer,
}

impl EffectiveSettingsLayers {
    #[must_use]
    pub fn new(
        args: SettingsLayer,
        workflow: SettingsLayer,
        project: SettingsLayer,
        user: SettingsLayer,
    ) -> Self {
        Self {
            args,
            workflow,
            project,
            user,
        }
    }
}

/// Resolve layered configuration down to a single effective [`SettingsLayer`].
pub fn resolve_settings(
    layers: EffectiveSettingsLayers,
    server_settings: Option<&SettingsLayer>,
    mode: EffectiveSettingsMode,
) -> Result<SettingsLayer> {
    let EffectiveSettingsLayers {
        args,
        mut workflow,
        mut project,
        user,
    } = layers;

    match mode {
        EffectiveSettingsMode::LocalOnly => Ok(combine_files(
            combine_files(combine_files(user, project), workflow),
            args,
        )),
        EffectiveSettingsMode::RemoteServer | EffectiveSettingsMode::LocalDaemon => {
            let server_settings = server_settings.ok_or(Error::MissingServerSettings)?;
            // Owner-specific domains (cli, server) may only come from the
            // local ~/.fabro/settings.toml, never from .fabro/project.toml or
            // workflow.toml. The user layer keeps its cli/server fields.
            strip_owner_domains(&mut workflow);
            strip_owner_domains(&mut project);

            let server_defaults = server_defaults_file(server_settings);

            let combined =
                combine_files(combine_files(combine_files(user, project), workflow), args);

            let mut settings = match mode {
                EffectiveSettingsMode::RemoteServer => {
                    apply_server_defaults(combined, &server_defaults)
                }
                EffectiveSettingsMode::LocalDaemon => {
                    apply_local_daemon_overrides(combined, &server_defaults)
                }
                EffectiveSettingsMode::LocalOnly => unreachable!(),
            };
            // Storage root always comes from the server's local
            // ~/.fabro/settings.toml, never from the client.
            if let Some(server_root) = server_settings
                .server
                .as_ref()
                .and_then(|s| s.storage.as_ref())
                .cloned()
            {
                let server = settings.server.get_or_insert_with(ServerLayer::default);
                server.storage = Some(server_root);
            }
            Ok(settings)
        }
    }
}

fn strip_owner_domains(file: &mut SettingsLayer) {
    file.cli = None;
    file.server = None;
}

/// Copy of the server settings with startup-time dry-run fallback cleared.
/// Run manifests carry their own dry-run intent; a daemon's startup-time
/// fallback mode must not silently force every submitted run into simulation.
fn server_defaults_file(settings: &SettingsLayer) -> SettingsLayer {
    let mut out = settings.clone();
    if let Some(run) = out.run.as_mut() {
        if let Some(execution) = run.execution.as_mut() {
            execution.mode = None;
        }
    }
    out
}

/// Apply server-side defaults to a client-layered [`SettingsLayer`].
///
/// Server-owned domains (`server`, `features`, and parts of `run`) flow from
/// the server's local `~/.fabro/settings.toml` when the corresponding client
/// value is absent. Run-shaped defaults (model, prepare, sandbox, checkpoint,
/// hooks, agent mcps, etc.) also flow from server to client so the persisted
/// run record matches the server's local configuration.
fn apply_server_defaults(mut settings: SettingsLayer, server: &SettingsLayer) -> SettingsLayer {
    // Server-owned domains: server-side always wins when client left blank.
    // Use the v2 merge matrix with the server layer in lower precedence so
    // that client-supplied values still dominate when present.
    settings = combine_files(server.clone(), settings);
    settings
}

/// Apply server-side overrides in LocalDaemon mode.
///
/// In LocalDaemon mode, a subset of server-owned fields unconditionally
/// override any client-side values. Client-controlled run-level fields are
/// left alone.
fn apply_local_daemon_overrides(
    mut settings: SettingsLayer,
    server: &SettingsLayer,
) -> SettingsLayer {
    if let Some(server_layer) = server.server.clone() {
        let client = settings.server.get_or_insert_with(ServerLayer::default);
        if let Some(storage) = server_layer.storage {
            client.storage = Some(storage);
        }
        if let Some(scheduler) = server_layer.scheduler {
            client.scheduler = Some(scheduler);
        }
        if let Some(artifacts) = server_layer.artifacts {
            client.artifacts = Some(artifacts);
        }
        if let Some(web) = server_layer.web {
            client.web = Some(web);
        }
        if let Some(api) = server_layer.api {
            client.api = Some(api);
        }
    }
    if let Some(features) = server.features.clone() {
        settings.features = Some(features);
    }
    // Ensure a run.execution table exists so downstream consumers that check
    // for explicit dry-run defaults see a well-formed layer.
    settings.run.get_or_insert_with(RunLayer::default);
    settings
        .run
        .as_mut()
        .unwrap()
        .execution
        .get_or_insert_with(RunExecutionLayer::default);
    settings
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::run::RunGoalLayer;
    use fabro_types::settings::server::{ServerLayer, ServerSchedulerLayer, ServerStorageLayer};
    use fabro_types::settings::{InterpString, SettingsLayer};

    use super::{EffectiveSettingsLayers, EffectiveSettingsMode, resolve_settings};
    use crate::parse::parse_settings_layer;

    fn layer(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("v2 fixture should parse")
    }

    #[test]
    fn local_only_merges_project_and_user_layers() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                SettingsLayer::default(),
                SettingsLayer::default(),
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

        assert_eq!(
            settings
                .run
                .as_ref()
                .and_then(|run| run.model.as_ref())
                .and_then(|model| model.name.as_ref())
                .map(InterpString::as_source)
                .as_deref(),
            Some("project-model")
        );
        // Per R22, run.inputs replaces wholesale — the winning layer is the
        // highest-precedence layer that sets `inputs` (project here, since it
        // wins over user).
        let inputs = settings
            .run
            .as_ref()
            .and_then(|run| run.inputs.as_ref())
            .unwrap();
        assert!(inputs.contains_key("project_only"));
        assert_eq!(
            inputs.get("shared").and_then(|v| v.as_str()),
            Some("project")
        );
        assert!(
            !inputs.contains_key("user_only"),
            "project.inputs should replace user.inputs wholesale"
        );
    }

    #[test]
    fn local_only_merges_workflow_project_user() {
        let settings = resolve_settings(
            EffectiveSettingsLayers::new(
                SettingsLayer::default(),
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

        assert_eq!(
            match settings.run.as_ref().and_then(|run| run.goal.as_ref()) {
                Some(RunGoalLayer::Inline(value)) => {
                    Some(value.as_source())
                }
                _ => None,
            }
            .as_deref(),
            Some("workflow goal")
        );
        assert_eq!(
            settings
                .run
                .as_ref()
                .and_then(|run| run.model.as_ref())
                .and_then(|model| model.name.as_ref())
                .map(InterpString::as_source)
                .as_deref(),
            Some("workflow-model")
        );
        assert_eq!(
            settings
                .run
                .as_ref()
                .and_then(|run| run.model.as_ref())
                .and_then(|model| model.provider.as_ref())
                .map(InterpString::as_source)
                .as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn cli_and_server_domains_from_fabro_toml_are_inert_under_remote_mode() {
        let server_settings = SettingsLayer {
            server: Some(ServerLayer {
                storage: Some(ServerStorageLayer {
                    root: Some(InterpString::parse("/srv/fabro")),
                }),
                scheduler: Some(ServerSchedulerLayer {
                    max_concurrent_runs: Some(9),
                }),
                ..ServerLayer::default()
            }),
            ..SettingsLayer::default()
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
                SettingsLayer::default(),
                SettingsLayer::default(),
                project_with_server,
                SettingsLayer::default(),
            ),
            Some(&server_settings),
            EffectiveSettingsMode::RemoteServer,
        )
        .unwrap();

        assert_eq!(
            settings
                .server
                .as_ref()
                .and_then(|server| server.storage.as_ref())
                .and_then(|storage| storage.root.as_ref())
                .map(InterpString::as_source)
                .as_deref(),
            Some("/srv/fabro")
        );
        assert_eq!(
            match settings.run.as_ref().and_then(|run| run.goal.as_ref()) {
                Some(RunGoalLayer::Inline(value)) => {
                    Some(value.as_source())
                }
                _ => None,
            }
            .as_deref(),
            Some("project goal")
        );
    }

    #[test]
    fn local_daemon_mode_only_applies_server_owned_overrides() {
        let server_settings = SettingsLayer {
            server: Some(ServerLayer {
                storage: Some(ServerStorageLayer {
                    root: Some(InterpString::parse("/srv/fabro")),
                }),
                scheduler: Some(ServerSchedulerLayer {
                    max_concurrent_runs: Some(7),
                }),
                ..ServerLayer::default()
            }),
            ..SettingsLayer::default()
        };

        let settings = resolve_settings(
            EffectiveSettingsLayers::default(),
            Some(&server_settings),
            EffectiveSettingsMode::LocalDaemon,
        )
        .unwrap();

        assert_eq!(
            settings
                .server
                .as_ref()
                .and_then(|server| server.storage.as_ref())
                .and_then(|storage| storage.root.as_ref())
                .map(InterpString::as_source)
                .as_deref(),
            Some("/srv/fabro")
        );
        assert_eq!(
            settings
                .server
                .as_ref()
                .and_then(|server| server.scheduler.as_ref())
                .and_then(|scheduler| scheduler.max_concurrent_runs),
            Some(7)
        );
    }
}
