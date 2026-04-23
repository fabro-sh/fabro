//! Effective settings resolution: combine layers into one resolved
//! [`SettingsLayer`].
//!
//! Shared layered domains (`project`, `workflow`, `run`, `features`) merge
//! across all three config files (settings.toml, .fabro/project.toml,
//! workflow.toml).
//! Owner-specific domains (`cli`, `server`) are consumed only from the local
//! `~/.fabro/settings.toml` plus explicit process-local overrides. Their
//! stanzas in `.fabro/project.toml` and `workflow.toml` remain schema-valid but
//! inert.

use fabro_types::settings::run::{RunExecutionLayer, RunLayer};
use fabro_types::settings::server::ServerLayer;
use fabro_types::settings::{Combine, SettingsLayer};

use crate::{Error, Result, apply_builtin_defaults};

#[derive(Clone, Debug, Default)]
pub struct EffectiveSettingsLayers {
    pub args:     SettingsLayer,
    pub workflow: SettingsLayer,
    pub project:  SettingsLayer,
    pub user:     SettingsLayer,
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

/// Materialize layered configuration down to a single effective
/// [`SettingsLayer`].
///
/// Precedence, lowest to highest:
///
/// 1. `server_settings.run` / `server_settings.features` — the server's
///    `~/.fabro/settings.toml` contributes its run-level defaults (for example,
///    a server-wide `run.execution.mode = "dry_run"`). Client layers win when
///    they set the same field.
/// 2. `user` — the manifest's `User` configs.
/// 3. `project` — the manifest's `Project` configs, with `cli`/`server`
///    stripped.
/// 4. `workflow` — the manifest's workflow config, with `cli`/`server`
///    stripped.
/// 5. `args` — process-local CLI overrides.
/// 6. A small subset of `server_settings.server` (storage, scheduler,
///    artifacts, web, api) plus `server_settings.features` are applied
///    authoritatively on top. The remaining server-ops fields (listen, auth,
///    ip_allowlist, slatedb, logging, integrations) stay on the server and do
///    not flow into the run's persisted settings — callers that need them
///    should consult the server's resolved settings directly.
pub fn materialize_settings_layer(
    layers: EffectiveSettingsLayers,
    server_settings: Option<&SettingsLayer>,
) -> Result<SettingsLayer> {
    let EffectiveSettingsLayers {
        args,
        mut workflow,
        mut project,
        user,
    } = layers;
    let server_settings = server_settings.ok_or(Error::MissingServerSettings)?;

    // Owner-specific domains (cli, server) may only come from the local
    // ~/.fabro/settings.toml, never from .fabro/project.toml or workflow.toml.
    // The user layer keeps its cli/server fields.
    strip_owner_domains(&mut workflow);
    strip_owner_domains(&mut project);

    // Server's run/features stanzas act as base defaults; client layers win.
    // Server's server/cli stanzas are handled authoritatively below.
    let mut server_defaults = server_settings.clone();
    server_defaults.cli = None;
    server_defaults.server = None;

    let combined = args
        .combine(workflow)
        .combine(project)
        .combine(user)
        .combine(server_defaults);
    let settings = enforce_server_authority(combined, server_settings);

    Ok(apply_builtin_defaults(settings))
}

fn strip_owner_domains(file: &mut SettingsLayer) {
    file.cli = None;
    file.server = None;
}

/// Apply server-owned fields on top of a client-combined [`SettingsLayer`].
///
/// Only the fields runs genuinely need are copied from the server:
/// `storage`, `scheduler`, `artifacts`, `web`, `api`. Operational config
/// (`listen`, `auth`, `ip_allowlist`, `slatedb`, `logging`, `integrations`)
/// stays on the server — callers that need those fields should read
/// `AppState::server_settings()` rather than re-resolving from run settings.
fn enforce_server_authority(mut settings: SettingsLayer, server: &SettingsLayer) -> SettingsLayer {
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
    settings
        .run
        .get_or_insert_with(RunLayer::default)
        .execution
        .get_or_insert_with(RunExecutionLayer::default);
    settings
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::cli::OutputFormat;
    use fabro_types::settings::run::{ApprovalMode, RunGoalLayer};
    use fabro_types::settings::server::{ServerLayer, ServerSchedulerLayer, ServerStorageLayer};
    use fabro_types::settings::{InterpString, SettingsLayer};

    use super::{EffectiveSettingsLayers, materialize_settings_layer};
    use crate::parse::parse_settings_layer;

    fn layer(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("v2 fixture should parse")
    }

    #[test]
    fn materialize_settings_layer_merges_layers_and_applies_server_authority() {
        let settings = materialize_settings_layer(
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
            Some(&layer(
                r#"
_version = 1

[server.storage]
root = "/srv/fabro"

[server.scheduler]
max_concurrent_runs = 7
"#,
            )),
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
        // Per R22, run.inputs replaces wholesale. The winning layer is the
        // highest-precedence layer that sets `inputs` (project here, since it
        // wins over user).
        let inputs = settings
            .run
            .as_ref()
            .and_then(|run| run.inputs.as_ref())
            .unwrap();
        assert!(inputs.contains_key("project_only"));
        assert_eq!(
            inputs.get("shared").and_then(|value| value.as_str()),
            Some("project")
        );
        assert!(
            !inputs.contains_key("user_only"),
            "project.inputs should replace user.inputs wholesale"
        );
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
        assert_eq!(
            settings
                .project
                .as_ref()
                .and_then(|project| project.directory.as_deref()),
            Some(".")
        );
        assert_eq!(
            settings
                .workflow
                .as_ref()
                .and_then(|workflow| workflow.graph.as_deref()),
            Some("workflow.fabro")
        );
        assert_eq!(
            settings
                .run
                .as_ref()
                .and_then(|run| run.execution.as_ref())
                .and_then(|execution| execution.approval),
            Some(ApprovalMode::Prompt)
        );
    }

    #[test]
    fn materialize_settings_layer_preserves_client_values_with_empty_server_layer() {
        let settings = materialize_settings_layer(
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
            Some(&SettingsLayer::default()),
        )
        .unwrap();

        assert_eq!(
            match settings.run.as_ref().and_then(|run| run.goal.as_ref()) {
                Some(RunGoalLayer::Inline(value)) => Some(value.as_source()),
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
    fn materialize_settings_layer_applies_server_owned_overrides() {
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

        let settings =
            materialize_settings_layer(EffectiveSettingsLayers::default(), Some(&server_settings))
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
        assert_eq!(
            settings
                .run
                .as_ref()
                .and_then(|run| run.sandbox.as_ref())
                .and_then(|sandbox| sandbox.provider.as_deref()),
            Some("local")
        );
        assert_eq!(
            settings
                .cli
                .as_ref()
                .and_then(|cli| cli.output.as_ref())
                .and_then(|output| output.format),
            Some(OutputFormat::Text)
        );
    }
}
