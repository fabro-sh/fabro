use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};
use fabro_types::{RunId, RunStatus};
use tokio::time::sleep;

use crate::server_client;
use crate::user_config::load_settings;

pub(crate) async fn execute(
    run_id: RunId,
    storage_dir: Option<PathBuf>,
    resume: bool,
) -> Result<()> {
    let _ = fabro_proc::title_init();

    let storage_dir = match storage_dir {
        Some(storage_dir) => storage_dir,
        None => load_settings()?.storage_dir(),
    };

    let client = server_client::connect_server(&storage_dir).await?;
    client.start_run(&run_id, resume).await?;

    loop {
        let state = client.get_run_state(&run_id).await?;
        let Some(status) = state.status.as_ref().map(|record| record.status) else {
            return Err(anyhow!("Run {run_id} has no status record in store"));
        };

        match status {
            RunStatus::Succeeded => return Ok(()),
            RunStatus::Failed | RunStatus::Dead => {
                return Err(anyhow!("Run {run_id} finished with status {status}"));
            }
            RunStatus::Submitted
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Paused
            | RunStatus::Removing => {
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
