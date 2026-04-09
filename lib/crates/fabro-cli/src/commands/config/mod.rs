use std::io::Write;
use std::path::Path;

use crate::args::{GlobalArgs, SettingsArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config;
use fabro_config::ConfigLayer;
use fabro_config::effective_settings;
use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::project;
use fabro_types::settings::v2::SettingsFile;

fn config_layers(
    ctx: &CommandContext,
    workflow: Option<&Path>,
) -> anyhow::Result<EffectiveSettingsLayers> {
    let cwd = ctx.cwd();
    let (workflow_layer, project_layer) = match workflow {
        Some(path) => workflow_and_project_layers(path, cwd)?,
        None => (ConfigLayer::default(), ConfigLayer::project(cwd)?),
    };
    let user_layer = user_config::settings_layer_with_config_and_storage_dir(
        Some(ctx.base_config_path()),
        None,
    )?;
    Ok(EffectiveSettingsLayers::new(
        ConfigLayer::default(),
        workflow_layer,
        project_layer,
        user_layer,
    ))
}

fn workflow_and_project_layers(
    path: &Path,
    cwd: &Path,
) -> anyhow::Result<(ConfigLayer, ConfigLayer)> {
    let resolution = project::resolve_workflow_path(path, cwd)?;
    if resolution.workflow_config.is_none() && !resolution.resolved_workflow_path.is_file() {
        anyhow::bail!(
            "Workflow not found: {}",
            resolution.resolved_workflow_path.display()
        );
    }

    let workflow_layer = resolution.workflow_config.unwrap_or_default();
    let project_layer = project::discover_project_config(
        resolution
            .resolved_workflow_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?
    .map(|(_, config)| config)
    .unwrap_or_default();

    Ok((workflow_layer, project_layer))
}

async fn merged_config(args: &SettingsArgs) -> anyhow::Result<SettingsFile> {
    let base_ctx = CommandContext::base()?;
    let layers = config_layers(&base_ctx, args.workflow.as_deref())?;
    if args.local {
        return effective_settings::resolve_settings(
            layers,
            None,
            EffectiveSettingsMode::LocalOnly,
        );
    }

    let ctx = CommandContext::for_target(&args.target)?;
    let target = user_config::resolve_server_target(&args.target, ctx.machine_settings())?;
    // `retrieve_server_settings` currently returns a legacy flat `Settings`;
    // route it through the v2 bridge shim for the consumer-side call.
    // Stage 6.6 rewrites the API client to return v2 types directly.
    let legacy_server = ctx.server().await?.retrieve_server_settings().await?;
    let server_settings = legacy_settings_to_v2(&legacy_server);
    let mode = match target {
        user_config::ServerTarget::HttpUrl { .. } => EffectiveSettingsMode::RemoteServer,
        user_config::ServerTarget::UnixSocket(_) => EffectiveSettingsMode::LocalDaemon,
    };

    effective_settings::resolve_settings(layers, Some(&server_settings), mode)
}

/// Stopgap reverse bridge from the legacy flat `Settings` to a v2
/// `SettingsFile`. `retrieve_server_settings` still returns the legacy
/// shape across the wire; the v2 resolver needs server-settings in v2
/// shape. This reverse-maps the fields that matter for server-side
/// defaults (storage, scheduler, integrations, verbose, run model).
/// Stage 6.6 rewrites the API client to return v2 types directly and
/// deletes this helper.
fn legacy_settings_to_v2(legacy: &fabro_types::Settings) -> SettingsFile {
    use fabro_types::settings::v2::cli::{CliLayer, CliOutputLayer, OutputVerbosity};
    use fabro_types::settings::v2::interp::InterpString;
    use fabro_types::settings::v2::run::{RunLayer, RunModelLayer};
    use fabro_types::settings::v2::server::{
        GithubIntegrationLayer, ServerIntegrationsLayer, ServerLayer, ServerSchedulerLayer,
        ServerStorageLayer, SlackIntegrationLayer,
    };

    let mut file = SettingsFile::default();

    if let Some(storage_dir) = legacy.storage_dir.as_ref() {
        let server = file.server.get_or_insert_with(ServerLayer::default);
        server.storage = Some(ServerStorageLayer {
            root: Some(InterpString::parse(&storage_dir.to_string_lossy())),
        });
    }
    if let Some(max_concurrent) = legacy.max_concurrent_runs {
        let server = file.server.get_or_insert_with(ServerLayer::default);
        server.scheduler = Some(ServerSchedulerLayer {
            max_concurrent_runs: Some(max_concurrent),
        });
    }
    if let Some(git) = legacy.git.as_ref() {
        let server = file.server.get_or_insert_with(ServerLayer::default);
        let integrations = server
            .integrations
            .get_or_insert_with(ServerIntegrationsLayer::default);
        let github = integrations
            .github
            .get_or_insert_with(GithubIntegrationLayer::default);
        github.app_id = git.app_id.as_deref().map(InterpString::parse);
        github.client_id = git.client_id.as_deref().map(InterpString::parse);
        github.slug = git.slug.as_deref().map(InterpString::parse);
    }
    if let Some(slack) = legacy.slack.as_ref() {
        let server = file.server.get_or_insert_with(ServerLayer::default);
        let integrations = server
            .integrations
            .get_or_insert_with(ServerIntegrationsLayer::default);
        integrations.slack = Some(SlackIntegrationLayer {
            enabled: None,
            default_channel: slack.default_channel.as_deref().map(InterpString::parse),
        });
    }
    if let Some(llm) = legacy.llm.as_ref() {
        let run = file.run.get_or_insert_with(RunLayer::default);
        run.model = Some(RunModelLayer {
            provider: llm.provider.as_deref().map(InterpString::parse),
            name: llm.model.as_deref().map(InterpString::parse),
            fallbacks: Vec::new(),
        });
    }
    if let Some(vars) = legacy.vars.as_ref() {
        let run = file.run.get_or_insert_with(RunLayer::default);
        run.inputs = Some(
            vars.iter()
                .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
                .collect(),
        );
    }
    if let Some(true) = legacy.verbose {
        let cli = file.cli.get_or_insert_with(CliLayer::default);
        cli.output = Some(CliOutputLayer {
            verbosity: Some(OutputVerbosity::Verbose),
            ..CliOutputLayer::default()
        });
    }
    file
}

pub(crate) async fn execute(args: &SettingsArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let config = Box::pin(merged_config(args)).await?;
    if globals.json {
        print_json_pretty(&config)?;
        return Ok(());
    }

    let mut yaml = serde_yaml::to_string(&config)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(yaml.as_bytes())?;

    Ok(())
}
