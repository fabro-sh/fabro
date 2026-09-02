use std::path::Path;

use anyhow::{Context as _, anyhow, bail};
use fabro_config::project;
use fabro_environment::{DEFAULT_ENVIRONMENT_ID, Environment, select_implicit_run_environment};
use fabro_types::settings::run::EnvironmentProvider;
use fabro_types::{DirtyStatus, RunId, RunIntent, RunTarget};
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
    let (target, dirty_worktree) =
        run_target_for_environment(environment.settings.provider, &canonical_cwd)?;
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

async fn resolve_run_environment(
    client: &fabro_client::Client,
    explicit_id: Option<&str>,
) -> anyhow::Result<Environment> {
    let id = explicit_id.unwrap_or(DEFAULT_ENVIRONMENT_ID);
    match client.retrieve_environment(id).await {
        Ok(environment) => Ok(environment),
        Err(error) if explicit_id.is_none() && fabro_client::is_not_found_error(&error) => {
            let environments = client
                .list_environments()
                .await
                .context("could not list environments")?;
            select_implicit_run_environment(&environments)
                .cloned()
                .map_err(anyhow::Error::new)
        }
        Err(error) => Err(error).with_context(|| format!("could not retrieve environment `{id}`")),
    }
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

/// Derives the run target from the caller directory for the environment's
/// provider. Returns the target plus whether a clone-based observation found a
/// dirty Git worktree, so the caller can warn about it.
fn run_target_for_environment(
    provider: EnvironmentProvider,
    canonical_cwd: &Path,
) -> anyhow::Result<(RunTarget, bool)> {
    if !provider.is_clone_based() {
        let path = canonical_cwd.to_str().ok_or_else(|| {
            anyhow!(
                "caller working directory is not valid UTF-8: {}",
                canonical_cwd.display()
            )
        })?;
        return Ok((
            RunTarget::Folder {
                path: path.to_string(),
            },
            false,
        ));
    }
    let Some(observation) = fabro_manifest::observe_git_run_target(canonical_cwd, None) else {
        return Ok((none_target_for_unversioned_directory(canonical_cwd)?, false));
    };
    let dirty = observation.legacy_git_context.dirty == DirtyStatus::Dirty;
    let target = observation.run_target.ok_or_else(|| {
        anyhow!("the caller Git checkout cannot be represented as a canonical GitHub run target")
    })?;
    if target.sha.is_none() {
        bail!(
            "the exact local Git commit could not be made available from the canonical GitHub origin; push the commit and try again"
        );
    }
    Ok((RunTarget::Git(target), dirty))
}

fn none_target_for_unversioned_directory(canonical_cwd: &Path) -> anyhow::Result<RunTarget> {
    let repository = match git2::Repository::discover(canonical_cwd) {
        Ok(repository) => repository,
        Err(source) if source.code() == git2::ErrorCode::NotFound => return Ok(RunTarget::None {}),
        Err(source) => {
            return Err(anyhow::Error::new(source)).with_context(|| {
                format!(
                    "failed to inspect caller working directory {} for Git metadata",
                    canonical_cwd.display()
                )
            });
        }
    };

    if repository.is_bare() {
        bail!(
            "the caller directory resolves to a bare Git repository; clone-based runs require a non-bare checkout with an attached branch"
        );
    }
    match repository.head() {
        Err(source)
            if matches!(
                source.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            bail!(
                "the caller Git checkout has no commits; create a commit before using a clone-based environment"
            );
        }
        Err(source) => {
            return Err(anyhow::Error::new(source))
                .context("failed to inspect the caller Git checkout HEAD");
        }
        Ok(head) if !head.is_branch() => {
            bail!(
                "the caller Git checkout has a detached HEAD; check out a branch before using a clone-based environment"
            );
        }
        Ok(_) => {}
    }

    bail!(
        "the caller Git checkout does not have a usable attached branch for a clone-based run target"
    )
}
