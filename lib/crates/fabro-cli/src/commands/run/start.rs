use anyhow::Result;
use fabro_types::RunId;

use crate::server_client;

pub(crate) async fn start_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    resume: bool,
) -> Result<()> {
    client.start_run(run_id, resume).await
}
