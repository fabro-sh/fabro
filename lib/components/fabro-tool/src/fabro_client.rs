use std::sync::Arc;

use async_trait::async_trait;
use fabro_api::types;
use fabro_types::{
    PairId, PairMessageRecord, PairMessageRequest, PairRecord, PairTranscriptResponse, Run, RunId,
    RunIntent, RunPairStatusResponse, RunProjection, RunStreamItem, StageId,
};

use crate::{FabroToolBackend, common};

#[derive(Clone)]
pub struct ClientBackend {
    client:                    Arc<::fabro_client::Client>,
    run_scope:                 Option<RunId>,
    workflow_version_packager: Option<Arc<dyn crate::WorkflowVersionPackager>>,
}

impl ClientBackend {
    #[must_use]
    pub fn new(client: Arc<::fabro_client::Client>) -> Self {
        Self {
            client,
            run_scope: None,
            workflow_version_packager: None,
        }
    }

    #[must_use]
    pub fn with_workflow_version_packager(
        mut self,
        packager: Arc<dyn crate::WorkflowVersionPackager>,
    ) -> Self {
        self.workflow_version_packager = Some(packager);
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
    /// Package the supplied tree, then register dependencies before parents.
    /// Versions are immutable and content-addressed, so a failed upload can be
    /// retried with the same contents without cleanup.
    async fn create_workflow_version(
        &self,
        source: crate::ValidatedWorkflowVersionCreate,
    ) -> anyhow::Result<fabro_types::WorkflowVersionId> {
        anyhow::ensure!(
            self.run_scope.is_none(),
            "workflow version creation is outside this tool session's run scope"
        );
        let packager = self
            .workflow_version_packager
            .as_ref()
            .ok_or_else(common::workflow_version_tool_unavailable_error)?;
        let packaged = packager.package(source).await?;
        let versions = packaged
            .versions()
            .map(|(_, v)| v.version())
            .collect::<Vec<_>>();
        self.client.register_workflow_versions(versions).await?;
        Ok(packaged.root_id())
    }

    async fn create_run_from_intent(&self, intent: RunIntent) -> anyhow::Result<RunId> {
        anyhow::ensure!(
            self.run_scope.is_none(),
            "run creation is outside this tool session's run scope"
        );
        self.client.create_run_from_intent(intent).await
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
        self.client.interrupt_run(run_id, None, None).await?;
        Ok(())
    }

    async fn steer_run(&self, run_id: &RunId, text: String, interrupt: bool) -> anyhow::Result<()> {
        self.ensure_run_scope(run_id)?;
        self.client.steer_run(run_id, text, interrupt, None).await?;
        Ok(())
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

    async fn list_run_stream(
        &self,
        run_id: &RunId,
        after: u64,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RunStreamItem>> {
        self.ensure_run_scope(run_id)?;
        match limit {
            Some(limit) => {
                self.client
                    .list_run_stream_until(run_id, after, limit)
                    .await
            }
            None => self.client.list_run_stream(run_id, after).await,
        }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use fabro_types::{WorkflowVersion, WorkflowVersionId};
    use fabro_workflow_version::{CollectedWorkflowClosure, ValidatedWorkflowVersion};
    use serde_json::json;

    use super::*;
    use crate::{ValidatedWorkflowVersionCreate, WorkflowVersionPackager};

    struct FixedPackager(Vec<WorkflowVersion>);

    #[async_trait]
    impl WorkflowVersionPackager for FixedPackager {
        async fn package(
            &self,
            _: ValidatedWorkflowVersionCreate,
        ) -> anyhow::Result<CollectedWorkflowClosure> {
            let versions = self
                .0
                .iter()
                .map(|v| Ok((v.id()?, ValidatedWorkflowVersion::new(v.clone())?)))
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(CollectedWorkflowClosure::from_dependency_order(
                versions.last().unwrap().0,
                versions,
            ))
        }
    }

    fn version(
        entrypoint: &str,
        dependencies: BTreeMap<fabro_types::WorkflowPath, WorkflowVersionId>,
    ) -> WorkflowVersion {
        WorkflowVersion::new(
            entrypoint.parse().unwrap(),
            BTreeMap::from([(
                entrypoint.parse().unwrap(),
                format!(
                    "digraph {entrypoint} {{ {} }}",
                    dependencies
                        .keys()
                        .map(|p| format!("child [stack.child_workflow=\"{p}\"]"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            )]),
            dependencies,
        )
        .unwrap()
    }

    fn source() -> ValidatedWorkflowVersionCreate {
        ValidatedWorkflowVersionCreate {
            entrypoint: "root".parse().unwrap(),
            files:      BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn create_workflow_version_registers_packaged_closure_and_retries_after_failure() {
        let server = httpmock::MockServer::start_async().await;
        let child = version("child", BTreeMap::new());
        let child_id = child.id().unwrap();
        let root = version(
            "root",
            BTreeMap::from([("child".parse().unwrap(), child_id)]),
        );
        let root_id = root.id().unwrap();
        let child_upload = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/v1/workflow-versions")
                    .json_body_obj(&child);
                then.status(201)
                    .json_body(json!({"workflow_version_id": child_id}));
            })
            .await;
        let failed_root = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/v1/workflow-versions")
                    .json_body_obj(&root);
                then.status(400);
            })
            .await;
        let client = ::fabro_client::Client::new_no_proxy(&server.url("")).unwrap();
        let backend = ClientBackend::new(Arc::new(client))
            .with_workflow_version_packager(Arc::new(FixedPackager(vec![child, root.clone()])));

        assert!(backend.create_workflow_version(source()).await.is_err());
        child_upload.assert_calls_async(1).await;
        failed_root.assert_calls_async(1).await;

        // Immutable content: retrying re-sends the child and completes the root.
        failed_root.delete_async().await;
        let root_upload = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/v1/workflow-versions")
                    .json_body_obj(&root);
                then.status(201)
                    .json_body(json!({"workflow_version_id": root_id}));
            })
            .await;
        assert_eq!(
            backend.create_workflow_version(source()).await.unwrap(),
            root_id
        );
        child_upload.assert_calls_async(2).await;
        root_upload.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn create_workflow_version_without_packager_is_unavailable() {
        let client = ::fabro_client::Client::new_no_proxy("http://127.0.0.1:1").unwrap();
        let error = ClientBackend::new(Arc::new(client))
            .create_workflow_version(source())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} is not available",
                crate::FABRO_WORKFLOW_VERSION_CREATE_TOOL_NAME
            )
        );
    }
}
