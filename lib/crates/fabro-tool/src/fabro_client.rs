use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_api::types;
use fabro_types::{
    EventEnvelope, PairId, PairMessageRecord, PairMessageRequest, PairRecord,
    PairTranscriptResponse, Run, RunId, RunPairStatusResponse, RunProjection, StageId,
};

use crate::{FabroToolBackend, RunManifestBuilder, ToolError};

#[derive(Clone)]
pub struct ClientBackend {
    client:           Arc<::fabro_client::Client>,
    manifest_builder: Option<Arc<dyn RunManifestBuilder>>,
    run_scope:        Option<RunId>,
}

impl ClientBackend {
    #[must_use]
    pub fn new(client: Arc<::fabro_client::Client>) -> Self {
        Self {
            client,
            manifest_builder: None,
            run_scope: None,
        }
    }

    #[must_use]
    pub fn with_manifest_builder(mut self, builder: Arc<dyn RunManifestBuilder>) -> Self {
        self.manifest_builder = Some(builder);
        self
    }

    /// Restrict this backend to a single run.
    ///
    /// Ask Fabro sessions use this with a same-run worker token so accidental
    /// cross-run tool calls are rejected before they reach the API.
    #[must_use]
    pub fn with_run_scope(mut self, run_id: RunId) -> Self {
        self.run_scope = Some(run_id);
        self
    }

    fn ensure_run_scope(&self, run_id: &RunId) -> anyhow::Result<()> {
        if let Some(scope) = self.run_scope {
            if &scope != run_id {
                anyhow::bail!("run {run_id} is outside this tool session's run scope");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FabroToolBackend for ClientBackend {
    async fn create_run_from_spec(
        &self,
        spec: &crate::ValidatedCreateRunSpec,
        cwd: &Path,
        user_settings_path: &Path,
        parent_id: Option<RunId>,
    ) -> anyhow::Result<RunId> {
        if let Some(parent_id) = parent_id.as_ref() {
            self.ensure_run_scope(parent_id)?;
        }
        let Some(builder) = self.manifest_builder.as_ref() else {
            return Err(ToolError::message(format!(
                "{} is not available",
                crate::FABRO_RUN_CREATE_TOOL_NAME
            ))
            .into());
        };
        let mut manifest = builder
            .build_run_manifest(spec, cwd, user_settings_path)
            .map_err(anyhow::Error::new)?;
        manifest.parent_id = parent_id.map(|run_id| run_id.to_string());
        self.client.create_run_from_manifest(manifest).await
    }

    async fn resolve_run(&self, selector: &str) -> anyhow::Result<Run> {
        if self.run_scope.is_some() {
            let run_id: RunId = selector.parse().map_err(|err| {
                anyhow::anyhow!(
                    "run selector must be the owning run id for this tool session: {err}"
                )
            })?;
            self.ensure_run_scope(&run_id)?;
            return self.retrieve_run(&run_id).await;
        }
        self.client.resolve_run(selector).await
    }

    async fn retrieve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.retrieve_run(run_id).await
    }

    async fn start_run(&self, run_id: &RunId, resume: bool) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.start_run(run_id, resume).await
    }

    async fn approve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.approve_run(run_id).await
    }

    async fn deny_run(&self, run_id: &RunId, reason: Option<String>) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.deny_run(run_id, reason).await
    }

    async fn cancel_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.cancel_run(run_id).await
    }

    async fn interrupt_run(&self, run_id: &RunId) -> anyhow::Result<()> {
        self.ensure_run_scope(run_id)?;
        self.client.interrupt_run(run_id).await
    }

    async fn steer_run(&self, run_id: &RunId, text: String, interrupt: bool) -> anyhow::Result<()> {
        self.ensure_run_scope(run_id)?;
        self.client.steer_run(run_id, text, interrupt).await
    }

    async fn archive_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.archive_run(run_id).await
    }

    async fn unarchive_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(run_id)?;
        self.client.unarchive_run(run_id).await
    }

    async fn list_store_runs(&self) -> anyhow::Result<Vec<Run>> {
        if let Some(run_id) = self.run_scope {
            return Ok(vec![self.retrieve_run(&run_id).await?]);
        }
        self.client.list_store_runs().await
    }

    async fn list_store_runs_by_parent(&self, parent_id: RunId) -> anyhow::Result<Vec<Run>> {
        self.ensure_run_scope(&parent_id)?;
        self.client.list_store_runs_by_parent(parent_id).await
    }

    async fn link_run_parent(&self, child_id: &RunId, parent_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(child_id)?;
        self.client.link_run_parent(child_id, parent_id).await
    }

    async fn unlink_run_parent(&self, child_id: &RunId) -> anyhow::Result<Run> {
        self.ensure_run_scope(child_id)?;
        self.client.unlink_run_parent(child_id).await
    }

    async fn get_run_state(&self, run_id: &RunId) -> anyhow::Result<RunProjection> {
        self.ensure_run_scope(run_id)?;
        self.client.get_run_state(run_id).await
    }

    async fn list_run_events(
        &self,
        run_id: &RunId,
        after: Option<u32>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<EventEnvelope>> {
        self.ensure_run_scope(run_id)?;
        self.client.list_run_events(run_id, after, limit).await
    }

    async fn list_run_events_until(
        &self,
        run_id: &RunId,
        after: Option<u32>,
        limit: usize,
    ) -> anyhow::Result<Vec<EventEnvelope>> {
        self.ensure_run_scope(run_id)?;
        self.client
            .list_run_events_until(run_id, after, limit)
            .await
    }

    async fn list_run_questions(&self, run_id: &RunId) -> anyhow::Result<Vec<types::ApiQuestion>> {
        self.ensure_run_scope(run_id)?;
        self.client.list_run_questions(run_id).await
    }

    async fn submit_run_answer(
        &self,
        run_id: &RunId,
        question_id: &str,
        body: types::SubmitAnswerRequest,
    ) -> anyhow::Result<()> {
        self.ensure_run_scope(run_id)?;
        self.client
            .submit_run_answer(run_id, question_id, body)
            .await
    }

    async fn get_run_pair_status(&self, run_id: &RunId) -> anyhow::Result<RunPairStatusResponse> {
        self.ensure_run_scope(run_id)?;
        self.client.get_run_pair_status(run_id).await
    }

    async fn start_run_pair(
        &self,
        run_id: &RunId,
        stage_id: StageId,
    ) -> anyhow::Result<PairRecord> {
        self.ensure_run_scope(run_id)?;
        self.client.start_run_pair(run_id, stage_id).await
    }

    async fn get_run_pair(&self, run_id: &RunId, pair_id: &PairId) -> anyhow::Result<PairRecord> {
        self.ensure_run_scope(run_id)?;
        self.client.get_run_pair(run_id, pair_id).await
    }

    async fn end_run_pair(&self, run_id: &RunId, pair_id: &PairId) -> anyhow::Result<PairRecord> {
        self.ensure_run_scope(run_id)?;
        self.client.end_run_pair(run_id, pair_id).await
    }

    async fn send_run_pair_message(
        &self,
        run_id: &RunId,
        pair_id: &PairId,
        request: PairMessageRequest,
    ) -> anyhow::Result<PairMessageRecord> {
        self.ensure_run_scope(run_id)?;
        self.client
            .send_run_pair_message(run_id, pair_id, request)
            .await
    }

    async fn get_run_pair_transcript(
        &self,
        run_id: &RunId,
        pair_id: &PairId,
        since_seq: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<PairTranscriptResponse> {
        self.ensure_run_scope(run_id)?;
        self.client
            .get_run_pair_transcript(run_id, pair_id, since_seq, limit)
            .await
    }
}
