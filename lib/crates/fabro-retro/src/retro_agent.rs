use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fabro_agent::tool_registry::RegisteredTool;
use fabro_agent::{
    AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile, Sandbox, Session, SessionEvent,
    SessionOptions, Turn, shell_quote,
};
use fabro_llm::client::Client;
use fabro_llm::provider::Provider;
use fabro_llm::types::ToolDefinition;
use fabro_store::{EventEnvelope, RunProjection, SerializableProjection};
use fabro_types::{RunBlobId, parse_blob_ref};
use tokio::task::JoinHandle;

use crate::retro::{RetroNarrative, SmoothnessRating};

const RETRO_SYSTEM_PROMPT: &str = r"You are a workflow run retrospective analyst. Your job is to analyze a completed workflow run and generate a structured retrospective.

You have access to the run's data files:
- `progress.jsonl` — the full event stream (stage starts/completions, agent tool calls, errors, retries)
- `run.json` — serialized run projection with the run spec, checkpoint state, conclusion, retro data, and other metadata
- `graph.fabro` — the workflow source for the run
- `stages/{node_id}@{visit}/...` — per-stage prompt, response, status, diff, stdout/stderr, and tool metadata files

## Your task

1. **Explore the data** using grep and read tools to understand what happened:
   - Look for failures, retries, and error messages
   - Check agent tool call patterns for wrong approaches or pivots
   - Note which stages took longest or had issues
   - Look for patterns indicating friction (repeated similar tool calls, error recovery)
   - Use `run.json` for the run-level snapshot, `graph.fabro` for workflow intent, and `stages/` for full per-stage payloads

2. **Call the `submit_retro` tool** with your structured analysis.

## Smoothness grading guidelines

Grade the run on a 5-point scale:

- **effortless** — Run achieved its goal on the first try with no retries, no wrong approaches. Agent moved efficiently from start to finish.
- **smooth** — Goal achieved with minor hiccups (1-2 retries or a brief wrong approach quickly corrected). No human intervention needed. Overall clean execution.
- **bumpy** — Goal achieved but with notable friction: multiple retries, at least one significant wrong approach, or substantial time spent on dead ends.
- **struggled** — Goal achieved only with difficulty: many retries, major approach changes, human intervention, or partial failures requiring recovery.
- **failed** — Run did not achieve its stated goal. May have completed some stages but the overall intent was not fulfilled.

Consider the full context: not just stage pass/fail, but the quality of the journey visible in the agent events (tool call patterns, error recovery, approach pivots).

## Guidelines for qualitative fields

- **intent**: What was the workflow run trying to accomplish? Summarize the goal in a sentence.
- **outcome**: What actually happened? Did it succeed? What was produced?
- **learnings**: What was discovered about the repo, code, workflow, or tools?
- **friction_points**: Where did things get stuck? What caused slowdowns?
- **open_items**: What follow-up work, tech debt, or gaps were identified?

Be specific and concise. Reference actual stage names, file paths, and error messages where relevant.";

const SUBMIT_RETRO_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "smoothness": {
      "type": "string",
      "enum": ["effortless", "smooth", "bumpy", "struggled", "failed"],
      "description": "Overall smoothness rating for the workflow run"
    },
    "intent": {
      "type": "string",
      "description": "What was the workflow run trying to accomplish?"
    },
    "outcome": {
      "type": "string",
      "description": "What actually happened? Did it succeed?"
    },
    "learnings": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "category": { "type": "string", "enum": ["repo", "code", "workflow", "tool"] },
          "text": { "type": "string" }
        },
        "required": ["category", "text"]
      },
      "description": "What was discovered during the run?"
    },
    "friction_points": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "kind": { "type": "string", "enum": ["retry", "timeout", "wrong_approach", "tool_failure", "ambiguity"] },
          "description": { "type": "string" },
          "stage_id": { "type": "string" }
        },
        "required": ["kind", "description"]
      },
      "description": "Where did things get stuck?"
    },
    "open_items": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "kind": { "type": "string", "enum": ["tech_debt", "follow_up", "investigation", "test_gap"] },
          "description": { "type": "string" }
        },
        "required": ["kind", "description"]
      },
      "description": "Follow-up work or gaps identified"
    }
  },
  "required": ["smoothness", "intent", "outcome"]
}"#;

pub const RETRO_DATA_DIR: &str = "/tmp/retro_data";

pub struct RetroAgentResult {
    pub narrative: RetroNarrative,
    pub response:  String,
}

pub type RetroBlobReader = Arc<
    dyn Fn(RunBlobId) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Vec<u8>>>> + Send>>
        + Send
        + Sync,
>;

#[must_use]
pub fn build_retro_prompt(retro_data_dir: &str) -> String {
    format!(
        "Analyze the workflow run data at `{retro_data_dir}/` and generate a retrospective. \
         The key file is `{retro_data_dir}/progress.jsonl` which contains the full event stream. \
         Use `{retro_data_dir}/run.json` for the run-level snapshot, `{retro_data_dir}/graph.fabro` \
         for the workflow source, and `{retro_data_dir}/stages/` for full per-stage payloads. \
         Use grep to search for interesting signals (failures, retries, errors, approach changes) \
         rather than reading the entire file. When done, call the `submit_retro` tool with your analysis."
    )
}

/// Run a retro agent session that analyzes workflow run data and produces
/// a structured narrative. The agent explores `progress.jsonl` and other
/// files via tool access, then calls `submit_retro` with its analysis.
pub async fn run_retro_agent(
    sandbox: &Arc<dyn Sandbox>,
    state: &RunProjection,
    events: &[EventEnvelope],
    run_dir: &Path,
    blob_reader: Option<RetroBlobReader>,
    llm_client: &Client,
    provider: Provider,
    model: &str,
    event_callback: Option<Arc<dyn Fn(SessionEvent) + Send + Sync>>,
) -> anyhow::Result<RetroAgentResult> {
    // Upload data files into sandbox (needed for Daytona; no-op effect for local
    // since the agent can also read from the original paths via tools).
    upload_data_files(
        sandbox,
        state,
        events,
        run_dir,
        RETRO_DATA_DIR,
        blob_reader.as_ref(),
    )
    .await?;

    // Build provider profile with the submit_retro tool
    let captured: Arc<Mutex<Option<RetroNarrative>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);

    let mut profile = build_profile(provider, model);

    // Register submit_retro tool
    let submit_tool = RegisteredTool {
        definition: ToolDefinition {
            name: "submit_retro".to_string(),
            description: "Submit the structured retrospective analysis. Call this once you have analyzed the workflow run data.".to_string(),
            parameters: serde_json::from_str(SUBMIT_RETRO_SCHEMA)
                .expect("submit_retro schema should be valid JSON"),
        },
        executor: Arc::new(move |args, _ctx| {
            let captured = Arc::clone(&captured_clone);
            Box::pin(async move {
                let narrative: RetroNarrative = serde_json::from_value(args)
                    .map_err(|e| format!("Invalid retro submission: {e}"))?;
                *captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(narrative);
                Ok("Retrospective submitted successfully.".to_string())
            })
        }),
    };
    profile.tool_registry_mut().register(submit_tool);

    let profile: Arc<dyn AgentProfile> = Arc::from(profile);

    let config = SessionOptions {
        max_tool_rounds_per_input: 20,
        wall_clock_timeout: Some(Duration::from_mins(3)),
        // Disable features not needed for retro analysis
        enable_context_compaction: false,
        skill_dirs: Some(vec![]),
        user_instructions: Some(RETRO_SYSTEM_PROMPT.to_string()),
        ..SessionOptions::default()
    };

    let mut session = Session::new(
        llm_client.clone(),
        profile,
        Arc::clone(sandbox),
        config,
        None,
    );

    // Optionally forward agent events via the callback
    let event_forwarder_handle = event_callback.map(|cb| spawn_retro_event_forwarder(&session, cb));

    session.initialize().await;

    let prompt = build_retro_prompt(RETRO_DATA_DIR);

    let process_result = session
        .process_input(&prompt)
        .await
        .map_err(|e| anyhow::anyhow!("Retro agent session failed: {e}"));

    // Extract response from session history
    let response_text = session
        .history()
        .turns()
        .iter()
        .rev()
        .find_map(|t| match t {
            Turn::Assistant { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or_default()
        .to_string();

    // Extract result / determine outcome
    let (_outcome, _failure_reason, narrative_result) = match process_result {
        Ok(()) => {
            let maybe_narrative = captured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            match maybe_narrative {
                Some(narrative) => ("success", None, Ok(narrative)),
                None => (
                    "error",
                    Some("Retro agent did not call submit_retro".to_string()),
                    Err(anyhow::anyhow!("Retro agent did not call submit_retro")),
                ),
            }
        }
        Err(e) => {
            let reason = e.to_string();
            ("error", Some(reason), Err(e))
        }
    };

    // Drop session to close the broadcast channel, then wait for event forwarder
    drop(session);
    if let Some(handle) = event_forwarder_handle {
        let _ = handle.await;
    }

    narrative_result.map(|narrative| RetroAgentResult {
        narrative,
        response: response_text,
    })
}

/// Return a placeholder narrative for dry-run mode. Exercises the full
/// derive → apply_narrative → save path without making LLM calls.
pub fn dry_run_narrative() -> RetroNarrative {
    RetroNarrative {
        smoothness:      SmoothnessRating::Smooth,
        intent:          "[dry-run] No LLM analysis performed".to_string(),
        outcome:         "[dry-run] Run completed in simulated mode".to_string(),
        learnings:       vec![],
        friction_points: vec![],
        open_items:      vec![],
    }
}

/// Spawn a background task that forwards session events via the provided
/// callback.
fn spawn_retro_event_forwarder(
    session: &Session,
    callback: Arc<dyn Fn(SessionEvent) + Send + Sync>,
) -> JoinHandle<()> {
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            callback(event);
        }
    })
}

fn build_profile(provider: Provider, model: &str) -> Box<dyn AgentProfile> {
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::new(model)),
        Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => Box::new(OpenAiProfile::new(model).with_provider(provider)),
        Provider::Gemini => Box::new(GeminiProfile::new(model)),
        Provider::Anthropic => Box::new(AnthropicProfile::new(model)),
    }
}

async fn upload_data_files(
    sandbox: &Arc<dyn Sandbox>,
    state: &RunProjection,
    events: &[EventEnvelope],
    _run_dir: &Path,
    target_dir: &str,
    blob_reader: Option<&RetroBlobReader>,
) -> anyhow::Result<()> {
    let progress_content = (!events.is_empty()).then(|| {
        let mut buf = String::new();
        for env in events {
            if let Ok(line) = serde_json::to_string(&env.event) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    });
    upload_file(
        sandbox,
        target_dir,
        Path::new("progress.jsonl"),
        progress_content,
    )
    .await?;

    let run_content = serde_json::to_string_pretty(&SerializableProjection(state))?;
    upload_file(
        sandbox,
        target_dir,
        Path::new("run.json"),
        Some(run_content),
    )
    .await?;
    upload_file(
        sandbox,
        target_dir,
        Path::new("graph.fabro"),
        state.graph_source.clone(),
    )
    .await?;

    let mut stage_ids: Vec<_> = state
        .iter_nodes()
        .map(|(stage_id, _)| stage_id.clone())
        .collect();
    stage_ids.sort();

    for stage_id in stage_ids {
        let Some(node) = state.node(&stage_id) else {
            continue;
        };
        let base = PathBuf::from("stages").join(stage_id.to_string());
        upload_file(
            sandbox,
            target_dir,
            &base.join("prompt.md"),
            node.prompt.clone(),
        )
        .await?;
        upload_file(
            sandbox,
            target_dir,
            &base.join("response.md"),
            node.response.clone(),
        )
        .await?;
        upload_json_file(
            sandbox,
            target_dir,
            &base.join("status.json"),
            node.status.as_ref(),
        )
        .await?;
        upload_json_file(
            sandbox,
            target_dir,
            &base.join("provider_used.json"),
            node.provider_used.as_ref(),
        )
        .await?;
        upload_file(
            sandbox,
            target_dir,
            &base.join("diff.patch"),
            node.diff.clone(),
        )
        .await?;
        upload_json_file(
            sandbox,
            target_dir,
            &base.join("script_invocation.json"),
            node.script_invocation.as_ref(),
        )
        .await?;
        upload_json_file(
            sandbox,
            target_dir,
            &base.join("script_timing.json"),
            node.script_timing.as_ref(),
        )
        .await?;
        upload_json_file(
            sandbox,
            target_dir,
            &base.join("parallel_results.json"),
            node.parallel_results.as_ref(),
        )
        .await?;
        upload_file(
            sandbox,
            target_dir,
            &base.join("stdout.log"),
            resolve_text_file_content(node.stdout.clone(), blob_reader).await?,
        )
        .await?;
        upload_file(
            sandbox,
            target_dir,
            &base.join("stderr.log"),
            resolve_text_file_content(node.stderr.clone(), blob_reader).await?,
        )
        .await?;
    }

    Ok(())
}

async fn resolve_text_file_content(
    content: Option<String>,
    blob_reader: Option<&RetroBlobReader>,
) -> anyhow::Result<Option<String>> {
    let Some(content) = content else {
        return Ok(None);
    };
    let Some(blob_id) = parse_blob_ref(&content) else {
        return Ok(Some(content));
    };
    let Some(blob_reader) = blob_reader else {
        return Ok(Some(content));
    };

    let bytes = blob_reader(blob_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("text blob missing: {blob_id}"))?;
    let text = serde_json::from_slice::<String>(&bytes)
        .map_err(|err| anyhow::anyhow!("text blob was not a JSON string: {err}"))?;
    Ok(Some(text))
}

async fn upload_file(
    sandbox: &Arc<dyn Sandbox>,
    target_dir: &str,
    relative: &Path,
    content: Option<String>,
) -> anyhow::Result<()> {
    let Some(content) = content else {
        return Ok(());
    };
    let path = Path::new(target_dir).join(relative);
    let remote_path = path.to_string_lossy().into_owned();
    ensure_remote_dir(sandbox, &path).await?;
    sandbox
        .write_file(&remote_path, &content)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to upload {}: {e}", relative.display()))?;
    Ok(())
}

async fn upload_json_file<T>(
    sandbox: &Arc<dyn Sandbox>,
    target_dir: &str,
    relative: &Path,
    value: Option<&T>,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let content = value.map(serde_json::to_string_pretty).transpose()?;
    upload_file(sandbox, target_dir, relative, content).await
}

async fn ensure_remote_dir(sandbox: &Arc<dyn Sandbox>, path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Retro upload path has no parent: {}", path.display()))?;
    let command = format!("mkdir -p {}", shell_quote(&parent.to_string_lossy()));
    let result = sandbox
        .exec_command(&command, 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create retro upload dir: {e}"))?;
    if result.exit_code != 0 {
        return Err(anyhow::anyhow!(
            "Failed to create retro upload dir {}: {}",
            parent.display(),
            result.stderr
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use fabro_agent::LocalSandbox;
    use fabro_store::{NodeState, StageId};
    use fabro_types::{NodeStatusRecord, StageOutcome};
    use tokio::fs;

    use super::*;

    #[test]
    fn submit_retro_schema_is_valid_json() {
        let schema: serde_json::Value = serde_json::from_str(SUBMIT_RETRO_SCHEMA).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["smoothness"].is_object());
        assert!(schema["properties"]["intent"].is_object());
        assert!(schema["properties"]["outcome"].is_object());
    }

    #[test]
    fn retro_narrative_parses_from_submit_retro_args() {
        let args = serde_json::json!({
            "smoothness": "smooth",
            "intent": "Fix the login bug",
            "outcome": "Successfully fixed the authentication flow",
            "learnings": [
                { "category": "code", "text": "Token refresh was in wrong module" }
            ],
            "friction_points": [
                { "kind": "retry", "description": "First attempt had wrong import", "stage_id": "code" }
            ],
            "open_items": [
                { "kind": "test_gap", "description": "No integration test for token refresh" }
            ]
        });

        let narrative: RetroNarrative = serde_json::from_value(args).unwrap();
        assert_eq!(narrative.smoothness, SmoothnessRating::Smooth);
        assert_eq!(narrative.intent, "Fix the login bug");
        assert_eq!(narrative.learnings.len(), 1);
        assert_eq!(narrative.friction_points.len(), 1);
        assert_eq!(narrative.open_items.len(), 1);
    }

    #[test]
    fn retro_narrative_parses_minimal_args() {
        let args = serde_json::json!({
            "smoothness": "effortless",
            "intent": "Deploy feature",
            "outcome": "Deployed successfully"
        });

        let narrative: RetroNarrative = serde_json::from_value(args).unwrap();
        assert_eq!(narrative.smoothness, SmoothnessRating::Effortless);
        assert!(narrative.learnings.is_empty());
        assert!(narrative.friction_points.is_empty());
        assert!(narrative.open_items.is_empty());
    }

    #[test]
    fn retro_prompt_mentions_graph_and_stage_files() {
        let prompt = build_retro_prompt(RETRO_DATA_DIR);

        assert!(prompt.contains("run.json"));
        assert!(prompt.contains("graph.fabro"));
        assert!(prompt.contains("stages/"));
    }

    #[tokio::test]
    async fn upload_data_files_writes_projection_graph_and_stage_files() {
        let sandbox_root = tempfile::tempdir().expect("sandbox tempdir should exist");
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(sandbox_root.path().to_path_buf()));
        let output_dir = tempfile::tempdir().expect("retro tempdir should exist");
        let target_dir = output_dir.path().join("retro");
        let target_dir_str = target_dir.to_string_lossy().to_string();

        let stage_id = StageId::new("build", 2);
        let mut state = RunProjection::default();
        state.graph_source = Some("digraph Ship {}".to_string());
        state.set_node(stage_id, NodeState {
            prompt:            Some("plan".to_string()),
            response:          Some("done".to_string()),
            status:            Some(NodeStatusRecord {
                status:         StageOutcome::Succeeded,
                notes:          Some("ok".to_string()),
                failure_reason: None,
                timestamp:      Utc
                    .with_ymd_and_hms(2026, 4, 20, 12, 1, 0)
                    .single()
                    .unwrap(),
            }),
            provider_used:     Some(serde_json::json!({ "provider": "openai" })),
            diff:              Some("diff --git a/a b/a".to_string()),
            script_invocation: Some(serde_json::json!({ "command": "cargo test" })),
            script_timing:     Some(serde_json::json!({ "duration_ms": 10 })),
            parallel_results:  Some(serde_json::json!([{ "stage": "fanout@1" }])),
            stdout:            Some("stdout".to_string()),
            stderr:            Some("stderr".to_string()),
            stdout_bytes:      None,
            stderr_bytes:      None,
            streams_separated: None,
            live_streaming:    None,
        });

        upload_data_files(
            &sandbox,
            &state,
            &[],
            output_dir.path(),
            &target_dir_str,
            None,
        )
        .await
        .expect("retro files should upload");

        let run_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(target_dir.join("run.json"))
                .await
                .expect("run.json should exist"),
        )
        .expect("run.json should parse");
        assert!(run_json.get("spec").is_some());
        assert!(run_json.get("run").is_none());
        assert!(run_json["nodes"]["build@2"]["prompt"].is_null());
        assert!(run_json["nodes"]["build@2"]["diff"].is_null());
        assert_eq!(
            fs::read_to_string(target_dir.join("graph.fabro"))
                .await
                .expect("graph.fabro should exist"),
            "digraph Ship {}"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/build@2/prompt.md"))
                .await
                .expect("prompt file should exist"),
            "plan"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/build@2/response.md"))
                .await
                .expect("response file should exist"),
            "done"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/build@2/stdout.log"))
                .await
                .expect("stdout file should exist"),
            "stdout"
        );
        assert!(
            target_dir.join("stages/build@2/status.json").exists(),
            "status file should exist"
        );
        assert!(
            !target_dir.join("progress.jsonl").exists(),
            "progress file should be omitted when there are no events"
        );
    }

    #[tokio::test]
    async fn upload_data_files_resolves_command_stdout_stderr_blob_refs() {
        let sandbox_root = tempfile::tempdir().expect("sandbox tempdir should exist");
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(sandbox_root.path().to_path_buf()));
        let output_dir = tempfile::tempdir().expect("retro tempdir should exist");
        let target_dir = output_dir.path().join("retro");
        let target_dir_str = target_dir.to_string_lossy().to_string();

        let stdout_blob = serde_json::to_vec("resolved stdout").unwrap();
        let stderr_blob = serde_json::to_vec("resolved stderr").unwrap();
        let stdout_id = fabro_types::RunBlobId::new(&stdout_blob);
        let stderr_id = fabro_types::RunBlobId::new(&stderr_blob);

        let stage_id = StageId::new("build", 1);
        let mut state = RunProjection::default();
        state.set_node(stage_id, NodeState {
            stdout: Some(fabro_types::format_blob_ref(&stdout_id)),
            stderr: Some(fabro_types::format_blob_ref(&stderr_id)),
            ..NodeState::default()
        });

        let reader: RetroBlobReader = Arc::new(move |blob_id| {
            let stdout_blob = stdout_blob.clone();
            let stderr_blob = stderr_blob.clone();
            Box::pin(async move {
                if blob_id == stdout_id {
                    Ok(Some(stdout_blob))
                } else if blob_id == stderr_id {
                    Ok(Some(stderr_blob))
                } else {
                    Ok(None)
                }
            })
        });

        upload_data_files(
            &sandbox,
            &state,
            &[],
            output_dir.path(),
            &target_dir_str,
            Some(&reader),
        )
        .await
        .expect("retro files should upload");

        assert_eq!(
            fs::read_to_string(target_dir.join("stages/build@1/stdout.log"))
                .await
                .expect("stdout file should exist"),
            "resolved stdout"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/build@1/stderr.log"))
                .await
                .expect("stderr file should exist"),
            "resolved stderr"
        );
    }
}
