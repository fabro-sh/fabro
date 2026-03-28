use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fabro_interview::FileInterviewer;
use fabro_store::RuntimeState;
use fabro_workflows::event::EventEmitter;
use fabro_workflows::git::GitAuthor;
use fabro_workflows::operations::{StartServices, resume as resume_run, start as start_run};
use fabro_workflows::records::{RunRecord, RunRecordExt};

use crate::cli_config;
use crate::shared;

pub(crate) async fn execute(run_dir: PathBuf, launcher_path: PathBuf, resume: bool) -> Result<()> {
    let _ = fabro_proctitle::init();

    let _launcher_guard = scopeguard::guard(launcher_path.clone(), |path| {
        super::launcher::remove_launcher_record(&path);
    });

    let cli_settings = cli_config::load_cli_settings(None)?;
    let on_node: Option<Arc<dyn Fn(&str) + Send + Sync>> =
        RunRecord::load(&run_dir).ok().map(|record| {
            let short_id = super::short_run_id(&record.run_id).to_string();
            fabro_proctitle::set(&format!("fabro: {short_id}"));
            Arc::new(move |node_id: &str| {
                fabro_proctitle::set(&format!("fabro: {short_id} {node_id}"));
            }) as Arc<dyn Fn(&str) + Send + Sync>
        });

    let github_app = shared::github::build_github_app_credentials(cli_settings.app_id());
    let git_author = GitAuthor::from_options(
        cli_settings.git_author().and_then(|a| a.name.clone()),
        cli_settings.git_author().and_then(|a| a.email.clone()),
    );
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
        on_node,
        registry_override: None,
    };

    if resume {
        let _ = resume_run(&run_dir, services).await?;
    } else {
        let _ = start_run(&run_dir, services).await?;
    }

    Ok(())
}
