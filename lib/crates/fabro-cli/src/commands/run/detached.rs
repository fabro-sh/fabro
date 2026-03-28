use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fabro_interview::FileInterviewer;
use fabro_store::RuntimeState;
use fabro_workflows::event::EventEmitter;
use fabro_workflows::git::GitAuthor;
use fabro_workflows::operations::{StartServices, resume as resume_run, start as start_run};

use crate::cli_config;
use crate::shared;

pub(crate) async fn execute(run_dir: PathBuf, launcher_path: PathBuf, resume: bool) -> Result<()> {
    let cli_config = cli_config::load_cli_settings(None)?;
    let github_app = shared::github::build_github_app_credentials(cli_config.app_id());
    let git_author = GitAuthor::from_options(
        cli_config.git_author().and_then(|a| a.name.clone()),
        cli_config.git_author().and_then(|a| a.email.clone()),
    );

    let _launcher_guard = scopeguard::guard(launcher_path.clone(), |path| {
        super::launcher::remove_launcher_record(&path);
    });
    let runtime_state = RuntimeState::new(&run_dir);

    let services = StartServices {
        cancel_token: None,
        emitter: Arc::new(EventEmitter::new()),
        interviewer: Arc::new(FileInterviewer::new(
            runtime_state.interview_request_path(),
            runtime_state.interview_response_path(),
            runtime_state.interview_claim_path(),
        )),
        git_author,
        github_app,
        registry_override: None,
    };

    if resume {
        let _ = resume_run(&run_dir, services).await?;
    } else {
        let _ = start_run(&run_dir, services).await?;
    }

    Ok(())
}
