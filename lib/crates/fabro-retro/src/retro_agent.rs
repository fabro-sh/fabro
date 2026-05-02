use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use fabro_agent::tool_registry::RegisteredTool;
use fabro_agent::{
    AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile, Sandbox, Session, SessionEvent,
    SessionOptions, Turn,
};
use fabro_dump::{BlobReader, RunDump};
use fabro_llm::client::Client;
use fabro_llm::provider::Provider;
use fabro_llm::types::ToolDefinition;
use fabro_store::{EventEnvelope, RunProjection};
use tokio::task::JoinHandle;

use crate::retro::{RetroNarrative, SmoothnessRating};

const RETRO_SYSTEM_PROMPT: &str = r"You are a workflow run retrospective analyst. Your job is to analyze a completed workflow run and generate a structured retrospective.

You have access to the run's data files:
- `events.jsonl` — the full event stream (stage starts/completions, agent tool calls, errors, retries)
- `run.json` — serialized run projection with the run spec, checkpoint state, conclusion, retro data, and other metadata
- `graph.fabro` — the workflow source for the run
- `checkpoints/{seq:04}.json` — zero-padded checkpoint snapshots captured during the run
- `run.log` — server/worker log output for the run when available
- `stages/{rank:03}-{node_id}@{visit}/...` — execution-order-prefixed per-stage prompt, response, status, diff, stdout/stderr, and tool metadata files

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

#[must_use]
pub fn build_retro_prompt(retro_data_dir: &str) -> String {
    format!(
        "Analyze the workflow run data at `{retro_data_dir}/` and generate a retrospective. \
         The key file is `{retro_data_dir}/events.jsonl` which contains the full event stream. \
         Use `{retro_data_dir}/run.json` for the run-level snapshot, `{retro_data_dir}/graph.fabro` \
         for the workflow source, `{retro_data_dir}/checkpoints/` for checkpoint snapshots, \
         `{retro_data_dir}/run.log` for server logs when present, and `{retro_data_dir}/stages/` \
         for full per-stage payloads. \
         Use grep to search for interesting signals (failures, retries, errors, approach changes) \
         rather than reading the entire file. When done, call the `submit_retro` tool with your analysis."
    )
}

/// Run a retro agent session that analyzes workflow run data and produces
/// a structured narrative. The agent explores `events.jsonl` and other
/// files via tool access, then calls `submit_retro` with its analysis.
pub async fn run_retro_agent(
    sandbox: &Arc<dyn Sandbox>,
    state: &RunProjection,
    events: &[EventEnvelope],
    run_log: Option<Vec<u8>>,
    blob_reader: Option<BlobReader>,
    llm_client: &Client,
    provider: Provider,
    model: &str,
    event_callback: Option<Arc<dyn Fn(SessionEvent) + Send + Sync>>,
) -> anyhow::Result<RetroAgentResult> {
    // Upload data files into sandbox (needed for Daytona; no-op effect for local
    // since the agent can also read from the original paths via tools).
    upload_data_files(sandbox, state, events, RETRO_DATA_DIR, run_log, blob_reader).await?;

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
        .context("Retro agent session failed");

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
    target_dir: &str,
    run_log: Option<Vec<u8>>,
    blob_reader: Option<BlobReader>,
) -> anyhow::Result<()> {
    let mut dump = RunDump::from_store_state_and_events(state, events)?;
    if let Some(log) = run_log {
        dump.add_file_bytes("run.log", log);
    }
    if let Some(reader) = blob_reader {
        dump.hydrate_referenced_blobs_with_reader(reader).await?;
    }
    for entry in dump.entries() {
        let remote_path = format!("{target_dir}/{}", entry.path());
        let bytes = entry.to_bytes()?;
        let text = String::from_utf8(bytes)
            .map_err(|err| anyhow::anyhow!("non-UTF8 retro entry {}: {err}", entry.path()))?;
        sandbox
            .write_file(&remote_path, &text)
            .await
            .map_err(|err| anyhow::anyhow!("Failed to upload {}: {err}", entry.path()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use fabro_agent::LocalSandbox;
    use fabro_store::StageId;
    use fabro_types::{StageCompletion, StageOutcome, first_event_seq};
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
        let stage = state.stage_entry(stage_id.node_id(), stage_id.visit(), first_event_seq(2));
        stage.prompt = Some("plan".to_string());
        stage.response = Some("done".to_string());
        stage.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          Some("ok".to_string()),
            failure_reason: None,
            timestamp:      Utc
                .with_ymd_and_hms(2026, 4, 20, 12, 1, 0)
                .single()
                .unwrap(),
        });
        stage.provider_used = Some(serde_json::json!({ "provider": "openai" }));
        stage.diff = Some("diff --git a/a b/a".to_string());
        stage.script_invocation = Some(serde_json::json!({ "command": "cargo test" }));
        stage.script_timing = Some(serde_json::json!({ "duration_ms": 10 }));
        stage.parallel_results = Some(serde_json::json!([{ "stage": "fanout@1" }]));
        stage.stdout = Some("stdout".to_string());
        stage.stderr = Some("stderr".to_string());

        upload_data_files(
            &sandbox,
            &state,
            &[],
            &target_dir_str,
            Some(b"server log\n".to_vec()),
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
        assert!(run_json["stages"]["build@2"]["prompt"].is_null());
        assert!(run_json["stages"]["build@2"]["diff"].is_null());
        assert_eq!(
            fs::read_to_string(target_dir.join("graph.fabro"))
                .await
                .expect("graph.fabro should exist"),
            "digraph Ship {}"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/001-build@2/prompt.md"))
                .await
                .expect("prompt file should exist"),
            "plan"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/001-build@2/response.md"))
                .await
                .expect("response file should exist"),
            "done"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/001-build@2/stdout.log"))
                .await
                .expect("stdout file should exist"),
            "stdout"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("events.jsonl"))
                .await
                .expect("events file should exist"),
            ""
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("run.log"))
                .await
                .expect("run.log should exist"),
            "server log\n"
        );
        assert!(
            target_dir.join("stages/001-build@2/status.json").exists(),
            "status file should exist"
        );
        assert!(
            !target_dir.join("progress.jsonl").exists(),
            "legacy progress file should not be emitted"
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
        let stdout_ref = fabro_types::format_blob_ref(&stdout_id);
        let stderr_ref = fabro_types::format_blob_ref(&stderr_id);
        let stage = state.stage_entry(stage_id.node_id(), stage_id.visit(), first_event_seq(1));
        stage.script_invocation = Some(serde_json::json!({
            "command": "cargo test",
            "stdout": stdout_ref,
            "stderr": stderr_ref,
        }));
        stage.script_timing = Some(serde_json::json!({
            "exit_code": 0,
            "stdout": stdout_ref,
            "stderr": stderr_ref,
        }));
        stage.stdout = Some(stdout_ref);
        stage.stderr = Some(stderr_ref);

        let reader: BlobReader = Box::new(move |blob_id| {
            let stdout_blob = stdout_blob.clone();
            let stderr_blob = stderr_blob.clone();
            Box::pin(async move {
                if blob_id == stdout_id {
                    Ok(Some(stdout_blob.into()))
                } else if blob_id == stderr_id {
                    Ok(Some(stderr_blob.into()))
                } else {
                    Ok(None)
                }
            })
        });

        upload_data_files(&sandbox, &state, &[], &target_dir_str, None, Some(reader))
            .await
            .expect("retro files should upload");

        assert_eq!(
            fs::read_to_string(target_dir.join("stages/001-build@1/stdout.log"))
                .await
                .expect("stdout file should exist"),
            "resolved stdout"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("stages/001-build@1/stderr.log"))
                .await
                .expect("stderr file should exist"),
            "resolved stderr"
        );

        let script_timing: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(target_dir.join("stages/001-build@1/script_timing.json"))
                .await
                .expect("script timing should exist"),
        )
        .expect("script timing should parse");
        assert_eq!(script_timing["stdout"], "resolved stdout");
        assert_eq!(script_timing["stderr"], "resolved stderr");

        let script_invocation: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(target_dir.join("stages/001-build@1/script_invocation.json"))
                .await
                .expect("script invocation should exist"),
        )
        .expect("script invocation should parse");
        assert_eq!(script_invocation["stdout"], "resolved stdout");
        assert_eq!(script_invocation["stderr"], "resolved stderr");

        let run_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(target_dir.join("run.json"))
                .await
                .expect("run.json should exist"),
        )
        .expect("run.json should parse");
        assert_eq!(
            run_json["stages"]["build@1"]["script_timing"]["stdout"],
            "resolved stdout"
        );
        assert_eq!(
            run_json["stages"]["build@1"]["script_timing"]["stderr"],
            "resolved stderr"
        );
        assert_eq!(
            run_json["stages"]["build@1"]["script_invocation"]["stdout"],
            "resolved stdout"
        );
        assert!(run_json["stages"]["build@1"]["stdout"].is_null());
        assert!(run_json["stages"]["build@1"]["stderr"].is_null());
    }
}
