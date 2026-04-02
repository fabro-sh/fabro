use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use fabro_interview::FileInterviewer;
use fabro_store::{RuntimeState, Store};
use fabro_types::RunId;
use fabro_workflow::event::EventEmitter;
use fabro_workflow::operations::{StartServices, resume as resume_run, start as start_run};

use crate::shared;
use crate::store;
use crate::user_config::load_user_settings;
pub(crate) async fn execute(
    run_id: RunId,
    run_dir: PathBuf,
    storage_dir: Option<PathBuf>,
    launcher_path: PathBuf,
    resume: bool,
) -> Result<()> {
    let _ = fabro_proc::title_init();

    let _launcher_guard = scopeguard::guard(launcher_path.clone(), |path| {
        super::launcher::remove_launcher_record(&path);
    });

    let storage_dir = match storage_dir {
        Some(storage_dir) => storage_dir,
        None => load_user_settings()?.storage_dir(),
    };
    let store = store::build_store(&storage_dir)?;
    let run_store = store
        .open_run(&run_id)
        .await?
        .ok_or_else(|| anyhow!("Run {run_id} not found in store"))?;
    let run_record = run_store
        .get_run()
        .await?
        .ok_or_else(|| anyhow!("Run {run_id} has no run record in store"))?;
    let on_node: fabro_workflow::OnNodeCallback = Some({
        let run_id = run_record.run_id.to_string();
        let short_id = super::short_run_id(&run_id).to_string();
        fabro_proc::title_set(&format!("fabro: {short_id}"));
        Arc::new(move |node_id: &str| {
            fabro_proc::title_set(&format!("fabro: {short_id} {node_id}"));
        }) as Arc<dyn Fn(&str) + Send + Sync>
    });

    let github_app = shared::github::build_github_app_credentials(run_record.settings.app_id())?;
    let runtime_state = RuntimeState::new(&run_dir);

    let services = StartServices {
        run_id: run_record.run_id,
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
