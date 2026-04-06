use std::path::Path;

use anyhow::Result;
use fabro_types::RunId;

use crate::server_client;

/// Queue a run for server-owned execution.
pub(crate) async fn start_run(run_id: &RunId, storage_dir: &Path, resume: bool) -> Result<()> {
    let client = server_client::connect_server(storage_dir).await?;
    start_run_with_client(&client, run_id, resume).await
}

pub(crate) async fn start_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    resume: bool,
) -> Result<()> {
    client.start_run(run_id, resume).await
}
