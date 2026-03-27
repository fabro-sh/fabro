use std::path::PathBuf;

use anyhow::Result;

use crate::cli_config;
use crate::shared;

pub async fn execute(run_dir: PathBuf, resume: bool) -> Result<()> {
    let styles: &'static fabro_util::terminal::Styles =
        Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
    let cli_config = cli_config::load_cli_config(None)?;
    let github_app = shared::github::build_github_app_credentials(cli_config.app_id());
    let git_author = fabro_workflows::git::GitAuthor::from_options(
        cli_config.git_author().and_then(|a| a.name.clone()),
        cli_config.git_author().and_then(|a| a.email.clone()),
    );

    let persisted = match fabro_workflows::pipeline::Persisted::load(&run_dir) {
        Ok(persisted) => persisted,
        Err(err) => {
            let anyhow_err: anyhow::Error = anyhow::anyhow!("Failed to load persisted run: {err}");
            let _ = super::detached_support::persist_detached_failure(
                &run_dir,
                "bootstrap",
                fabro_workflows::run_status::StatusReason::BootstrapFailed,
                &anyhow_err,
            );
            return Err(anyhow_err);
        }
    };

    if let Err(err) =
        std::env::set_current_dir(&persisted.run_record().working_directory).map_err(|e| {
            anyhow::anyhow!(
                "Failed to set working directory to {}: {e}",
                persisted.run_record().working_directory.display()
            )
        })
    {
        let _ = super::detached_support::persist_detached_failure(
            &run_dir,
            "bootstrap",
            fabro_workflows::run_status::StatusReason::BootstrapFailed,
            &err,
        );
        return Err(err);
    }

    let result = if resume {
        super::execute::resume_from_record(
            persisted,
            run_dir.clone(),
            cli_config,
            styles,
            github_app,
            git_author,
        )
        .await
    } else {
        super::execute::run_from_record(
            persisted,
            run_dir.clone(),
            cli_config,
            styles,
            github_app,
            git_author,
        )
        .await
    };

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = super::detached_support::persist_detached_failure(
                &run_dir,
                "bootstrap",
                fabro_workflows::run_status::StatusReason::SandboxInitFailed,
                &err,
            );
            Err(err)
        }
    }
}
