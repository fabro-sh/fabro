use std::path::Path;

use anyhow::{Context as _, anyhow};
use fabro_config::project;
use fabro_environment::{DEFAULT_ENVIRONMENT_ID, Environment};
use fabro_types::{RunId, RunIntent};
use fabro_util::terminal::Styles;

use super::overrides::prepare_intent_overrides;
use crate::args::RunArgs;
use crate::command_context::CommandContext;
use crate::commands::resolve_run_id;
use crate::user_config::{RunSettingsKeyPresence, read_project_run_settings_key_presence};

pub(crate) struct CreatedRun {
    pub(crate) run_id: RunId,
}

/// Register the local workflow version closure with the server and create a
/// run from an immutable workflow intent, leaving it in the submitted state.
///
/// This does NOT start the workflow — starting is a separate request.
pub(crate) async fn create_run(
    ctx: &CommandContext,
    args: &RunArgs,
    styles: &Styles,
) -> anyhow::Result<CreatedRun> {
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required"))?;
    let canonical_cwd = ctx.cwd().canonicalize().with_context(|| {
        format!(
            "failed to canonicalize caller working directory {}",
            ctx.cwd().display()
        )
    })?;
    let user_workflows_root = fabro_util::Home::from_env().workflows_dir();
    let package = fabro_manifest::resolve_local_workflow_package(
        workflow_path,
        &canonical_cwd,
        Some(&user_workflows_root),
    )?;
    let prepared = prepare_intent_overrides(args, &canonical_cwd).await?;

    warn_untransmitted_settings(
        ctx,
        styles,
        ctx.base_config_path(),
        *ctx.run_settings_key_presence(),
    );
    let project_config = project::discover_project_config(&package.workflow_location().dir)?;
    if let Some(path) = project_config.as_deref() {
        warn_untransmitted_settings(
            ctx,
            styles,
            path,
            read_project_run_settings_key_presence(path).await?,
        );
    }

    let client = ctx.server().await?;
    let (parent_id, environment) = tokio::try_join!(
        async {
            match args.parent.as_deref() {
                Some(parent_selector) => Ok(Some(
                    resolve_run_id(client.as_ref(), parent_selector).await?,
                )),
                None => Ok(None),
            }
        },
        resolve_run_environment(client.as_ref(), args.environment.as_deref()),
    )?;
    let fabro_manifest::DerivedRunTarget {
        target,
        dirty_worktree,
    } = fabro_manifest::derive_run_target_for_provider(
        environment.settings.provider,
        &canonical_cwd,
        None,
    )?;
    if dirty_worktree {
        fabro_util::printerr!(
            ctx.printer(),
            "{} the caller Git working tree is dirty; uncommitted changes are not included in the run target.",
            styles.yellow.apply_to("Warning:"),
        );
    }
    let workflow_version_id = package.closure().root_id();
    client
        .register_workflow_versions(
            package
                .closure()
                .versions()
                .map(|(_, validated)| validated.version()),
        )
        .await
        .context("could not register workflow versions")?;
    let created_run_id = client
        .create_run_from_intent(RunIntent {
            workflow_version_id,
            target,
            args: prepared.intent_args,
            environment_id: Some(environment.id.to_string()),
            parent_id,
            title: None,
            goal: prepared.goal,
        })
        .await
        .context("could not create run")?;

    Ok(CreatedRun {
        run_id: created_run_id,
    })
}

/// Retrieves the run environment, defaulting to `default` when `--environment`
/// is omitted. A missing `default` is reported with the catalog so the user
/// can pick an explicit environment or create the missing one.
async fn resolve_run_environment(
    client: &fabro_client::Client,
    explicit_id: Option<&str>,
) -> anyhow::Result<Environment> {
    let id = explicit_id.unwrap_or(DEFAULT_ENVIRONMENT_ID);
    match client.retrieve_environment(id).await {
        Ok(environment) => Ok(environment),
        Err(error) if explicit_id.is_none() && fabro_client::is_not_found_error(&error) => {
            Err(missing_default_environment_error(client).await)
        }
        Err(error) => Err(error).with_context(|| format!("could not retrieve environment `{id}`")),
    }
}

async fn missing_default_environment_error(client: &fabro_client::Client) -> anyhow::Error {
    let catalog_hint = match client.list_environments().await {
        Ok(environments) if environments.is_empty() => {
            " The server has no environments configured.".to_string()
        }
        Ok(environments) => {
            let ids = environments
                .iter()
                .map(|environment| format!("`{}`", environment.id))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" Available environments: {ids}.")
        }
        Err(_) => String::new(),
    };
    anyhow!(
        "environment `{DEFAULT_ENVIRONMENT_ID}` not found on the server; pass `--environment <id>` or create an environment named `{DEFAULT_ENVIRONMENT_ID}`.{catalog_hint}"
    )
}

fn warn_untransmitted_settings(
    ctx: &CommandContext,
    styles: &Styles,
    path: &Path,
    presence: RunSettingsKeyPresence,
) {
    let keys = presence.key_paths();
    if keys.is_empty() {
        return;
    }
    fabro_util::printerr!(
        ctx.printer(),
        "{} {} contains {}; `fabro run` and `fabro create` do not transmit these settings. Move workflow-owned run behavior, including `run.pull_request`, to `workflow.toml`; configure placement with server-managed environments.",
        styles.yellow.apply_to("Warning:"),
        path.display(),
        keys.join(", "),
    );
}
