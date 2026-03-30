use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_interview::FileInterviewer;
use fabro_store::RuntimeState;
use fabro_workflow::event::EventEmitter;
use fabro_workflow::operations::{
    StartServices, open_or_hydrate_run, resume as resume_run, start as start_run,
};
use fabro_workflow::records::{RunRecord, RunRecordExt};

use crate::shared;
use crate::store;
pub(crate) async fn execute(run_dir: PathBuf, launcher_path: PathBuf, resume: bool) -> Result<()> {
    let _ = fabro_proctitle::init();

    let _launcher_guard = scopeguard::guard(launcher_path.clone(), |path| {
        super::launcher::remove_launcher_record(&path);
    });

    let run_record = RunRecord::load(&run_dir)?;
    let on_node: fabro_workflow::OnNodeCallback = Some({
        let run_id = run_record.run_id.to_string();
        let short_id = super::short_run_id(&run_id).to_string();
        fabro_proctitle::set(&format!("fabro: {short_id}"));
        Arc::new(move |node_id: &str| {
            fabro_proctitle::set(&format!("fabro: {short_id} {node_id}"));
        }) as Arc<dyn Fn(&str) + Send + Sync>
    });
    let store = store::build_store(&run_record.settings.storage_dir())?;
    let run_store = open_or_hydrate_run(store.as_ref(), &run_dir).await?;

    let github_app = shared::github::build_github_app_credentials(run_record.settings.app_id())?;
    let runtime_state = RuntimeState::new(&run_dir);

    let services = StartServices {
        cancel_token: None,
        emitter: Arc::new(EventEmitter::new(run_record.run_id)),
        interviewer: Arc::new(FileInterviewer::new(
            runtime_state.interview_request_path(),
            runtime_state.interview_response_path(),
            runtime_state.interview_claim_path(),
        )),
        run_store,
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
