use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_store::RunSummary;
use fabro_workflow::run_lookup::{RunInfo, resolve_run_from_summaries, runs_base};

use crate::server_client::{self, ServerStoreClient};

pub(crate) struct ServerRunLookup {
    client: ServerStoreClient,
    runs_base: PathBuf,
    summaries: Vec<RunSummary>,
}

impl ServerRunLookup {
    pub(crate) async fn connect(storage_dir: &Path) -> Result<Self> {
        Self::connect_from_runs_base(&runs_base(storage_dir)).await
    }

    pub(crate) async fn connect_from_runs_base(runs_base: &Path) -> Result<Self> {
        let storage_dir = runs_base.parent().unwrap_or(runs_base);
        let client = server_client::connect_server(storage_dir).await?;
        let summaries = client.list_store_runs().await?;
        Ok(Self {
            client,
            runs_base: runs_base.to_path_buf(),
            summaries,
        })
    }

    pub(crate) fn client(&self) -> &ServerStoreClient {
        &self.client
    }

    pub(crate) fn runs_base(&self) -> &Path {
        &self.runs_base
    }

    pub(crate) fn summaries(&self) -> &[RunSummary] {
        &self.summaries
    }

    pub(crate) fn resolve(&self, selector: &str) -> Result<RunInfo> {
        resolve_run_from_summaries(&self.summaries, &self.runs_base, selector)
    }
}
