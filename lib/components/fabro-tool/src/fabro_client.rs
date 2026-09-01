use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_api::types;
use fabro_types::{
    EventEnvelope, PairId, PairMessageRecord, PairMessageRequest, PairRecord,
    PairTranscriptResponse, Run, RunId, RunIntent, RunIntentArgs, RunPairStatusResponse,
    RunProjection, StageId,
};

use crate::{
    CreateRunSubmission, FabroToolBackend, PreparedRunCreate, RunCreateAdapter, ToolError,
    ValidatedCreateRunSpec,
};

#[derive(Clone)]
pub struct ClientBackend {
    client:             Arc<::fabro_client::Client>,
    run_create_adapter: Option<Arc<dyn RunCreateAdapter>>,
    run_scope:          Option<RunId>,
}

impl ClientBackend {
    #[must_use]
    pub fn new(client: Arc<::fabro_client::Client>) -> Self {
        Self {
            client,
            run_create_adapter: None,
            run_scope: None,
        }
    }

    #[must_use]
    pub fn with_run_create_adapter(mut self, adapter: Arc<dyn RunCreateAdapter>) -> Self {
        self.run_create_adapter = Some(adapter);
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

fn run_intent_from_spec(
    spec: &ValidatedCreateRunSpec,
    prepared: PreparedRunCreate,
    parent_id: Option<RunId>,
) -> (RunIntent, Vec<String>) {
    let PreparedRunCreate {
        workflow_version_id,
        target,
        goal,
        warnings,
    } = prepared;
    let intent = RunIntent {
        workflow_version_id,
        target,
        args: RunIntentArgs {
            model:            spec.model.clone(),
            provider:         spec.provider.clone(),
            inputs:           spec
                .inputs
                .iter()
                .map(|(key, value)| (key.clone(), value.json().clone()))
                .collect(),
            labels:           spec.labels.clone(),
            dry_run:          spec.dry_run,
            auto_approve:     spec.auto_approve,
            preserve_sandbox: spec.preserve_sandbox,
        },
        environment_id: spec.environment.clone(),
        parent_id,
        title: None,
        goal,
    };
    (intent, warnings)
}

#[async_trait]
impl FabroToolBackend for ClientBackend {
    async fn create_run_from_spec(
        &self,
        spec: &crate::ValidatedCreateRunSpec,
        cwd: &Path,
        parent_id: Option<RunId>,
    ) -> anyhow::Result<CreateRunSubmission> {
        if let Some(parent_id) = parent_id.as_ref() {
            self.ensure_run_scope(parent_id)?;
        }
        let Some(adapter) = self.run_create_adapter.as_ref() else {
            return Err(ToolError::message(format!(
                "{} is not available",
                crate::FABRO_RUN_CREATE_TOOL_NAME
            ))
            .into());
        };
        let prepared = adapter.prepare(&self.client, spec, cwd).await?;
        let (intent, warnings) = run_intent_from_spec(spec, prepared, parent_id);
        let run_id = self.client.create_run_from_intent(intent).await?;
        Ok(CreateRunSubmission { run_id, warnings })
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::{GitRunTarget, RunTarget};
    use serde_json::json;

    use super::*;
    use crate::{FabroRunCreateParams, ValidatedCreateRuns};

    fn validated_spec(value: &serde_json::Value) -> ValidatedCreateRunSpec {
        let params: FabroRunCreateParams = serde_json::from_value(json!({ "runs": [value] }))
            .expect("create input should deserialize");
        ValidatedCreateRuns::try_from(params)
            .expect("create input should validate")
            .runs
            .remove(0)
    }

    #[test]
    fn create_run_intent_mapping_is_lossless_and_manifest_free() {
        let workflow_version_id = fabro_types::BlobHash::new(b"exact workflow version").into();
        let parent_id = RunId::new();
        let target = RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "feature/run-intent".to_string(),
            tag:    Some("v1.2.3".to_string()),
            sha:    Some("0123456789ABCDEF0123456789ABCDEF01234567".to_string()),
        });
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": target,
            "model": "gpt-5.4",
            "provider": "openai",
            "inputs": {
                "text": "value",
                "enabled": true,
                "count": 7,
                "ratio": 1.25
            },
            "labels": { "source": "tool" },
            "dry_run": false,
            "auto_approve": true,
            "preserve_sandbox": false,
            "environment": "production",
            "goal": "Ship the exact bytes"
        }));
        let canonical_target = spec.target.clone().unwrap();
        let (intent, warnings) = run_intent_from_spec(
            &spec,
            PreparedRunCreate {
                workflow_version_id,
                target: canonical_target.clone(),
                goal: Some("Ship the exact bytes".to_string()),
                warnings: vec!["not part of the intent".to_string()],
            },
            Some(parent_id),
        );

        assert_eq!(warnings, ["not part of the intent"]);
        assert_eq!(intent.workflow_version_id, workflow_version_id);
        assert_eq!(intent.target, canonical_target);
        assert_eq!(intent.parent_id, Some(parent_id));
        assert_eq!(intent.environment_id.as_deref(), Some("production"));
        assert_eq!(intent.goal.as_deref(), Some("Ship the exact bytes"));
        assert_eq!(intent.title, None);
        assert_eq!(intent.args.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(intent.args.provider.as_deref(), Some("openai"));
        assert_eq!(
            intent.args.inputs,
            HashMap::from([
                ("text".to_string(), json!("value")),
                ("enabled".to_string(), json!(true)),
                ("count".to_string(), json!(7)),
                ("ratio".to_string(), json!(1.25)),
            ])
        );
        assert_eq!(
            intent.args.labels,
            HashMap::from([("source".to_string(), "tool".to_string(),)])
        );
        assert_eq!(intent.args.dry_run, Some(false));
        assert_eq!(intent.args.auto_approve, Some(true));
        assert_eq!(intent.args.preserve_sandbox, Some(false));

        let wire = serde_json::to_value(intent).unwrap();
        assert!(wire.get("cwd").is_none());
        assert!(wire.get("configs").is_none());
        assert!(wire.get("settings").is_none());
        assert!(wire.get("warnings").is_none());
    }

    #[test]
    fn create_run_intent_mapping_preserves_omitted_boolean_overrides() {
        let workflow_version_id = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": { "kind": "none" }
        }));
        let (intent, warnings) = run_intent_from_spec(
            &spec,
            PreparedRunCreate {
                workflow_version_id,
                target: RunTarget::None {},
                goal: None,
                warnings: Vec::new(),
            },
            None,
        );

        assert!(warnings.is_empty());
        assert_eq!(intent.args.dry_run, None);
        assert_eq!(intent.args.auto_approve, None);
        assert_eq!(intent.args.preserve_sandbox, None);
        assert_eq!(serde_json::to_value(intent).unwrap()["args"], json!({}));
    }
}
