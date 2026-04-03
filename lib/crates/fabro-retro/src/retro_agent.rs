use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fabro_agent::tool_registry::RegisteredTool;
use fabro_agent::{
    AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile, Sandbox, Session, SessionEvent,
    SessionOptions, Turn,
};
use fabro_llm::client::Client;
use fabro_llm::provider::Provider;
use fabro_llm::types::ToolDefinition;
use fabro_store::SlateRunStore;
use fabro_util::redact::redact_jsonl_line;
use tokio::sync::broadcast::Receiver;
use tokio::task::JoinHandle;

use crate::retro::{RetroNarrative, SmoothnessRating};

const RETRO_SYSTEM_PROMPT: &str = r"You are a workflow run retrospective analyst. Your job is to analyze a completed workflow run and generate a structured retrospective.

You have access to the run's data files:
- `progress.jsonl` — the full event stream (stage starts/completions, agent tool calls, errors, retries)
- `checkpoint.json` — final execution state with node outcomes
- `run.json` — run record with config, graph, and metadata
- `start.json` — start record with start time and git info

## Your task

1. **Explore the data** using grep and read tools to understand what happened:
   - Look for failures, retries, and error messages
   - Check agent tool call patterns for wrong approaches or pivots
   - Note which stages took longest or had issues
   - Look for patterns indicating friction (repeated similar tool calls, error recovery)

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
    pub response: String,
}

#[must_use]
pub fn build_retro_prompt(retro_data_dir: &str) -> String {
    format!(
        "Analyze the workflow run data at `{retro_data_dir}/` and generate a retrospective. \
         The key file is `{retro_data_dir}/progress.jsonl` which contains the full event stream. \
         Also check `{retro_data_dir}/checkpoint.json` for stage outcomes. \
         Use grep to search for interesting signals (failures, retries, errors, approach changes) \
         rather than reading the entire file. When done, call the `submit_retro` tool with your analysis."
    )
}

/// Run a retro agent session that analyzes workflow run data and produces
/// a structured narrative. The agent explores `progress.jsonl` and other
/// files via tool access, then calls `submit_retro` with its analysis.
pub async fn run_retro_agent(
    sandbox: &Arc<dyn Sandbox>,
    run_store: &SlateRunStore,
    run_dir: &Path,
    llm_client: &Client,
    provider: Provider,
    model: &str,
    event_callback: Option<Arc<dyn Fn(SessionEvent) + Send + Sync>>,
) -> anyhow::Result<RetroAgentResult> {
    // Upload data files into sandbox (needed for Daytona; no-op effect for local
    // since the agent can also read from the original paths via tools).
    upload_data_files(sandbox, run_store, run_dir, RETRO_DATA_DIR).await?;

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
        wall_clock_timeout: Some(Duration::from_secs(180)),
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

    // Set up event writer before initialize (which emits SessionStarted)
    let retro_dir = run_dir.join("retro");
    std::fs::create_dir_all(&retro_dir)?;
    let rx = session.subscribe();
    let event_writer_handle = spawn_retro_event_writer(rx, retro_dir.join("retro_session.jsonl"));

    // Optionally forward agent events via the callback
    let event_forwarder_handle = event_callback.map(|cb| spawn_retro_event_forwarder(&session, cb));

    session.initialize().await;

    let prompt = build_retro_prompt(RETRO_DATA_DIR);

    write_retro_prompt(run_store, &retro_dir, &prompt)?;

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
    let (outcome, failure_reason, narrative_result) = match process_result {
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

    // Write artifacts (on both success and failure)
    write_retro_response(run_store, &retro_dir, &response_text)?;
    write_retro_artifacts(
        &retro_dir,
        provider.as_str(),
        model,
        outcome,
        failure_reason.as_deref(),
    );

    // Drop session to close the broadcast channel, then wait for event writer/forwarder
    drop(session);
    let _ = event_writer_handle.await;
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
        smoothness: SmoothnessRating::Smooth,
        intent: "[dry-run] No LLM analysis performed".to_string(),
        outcome: "[dry-run] Run completed in simulated mode".to_string(),
        learnings: vec![],
        friction_points: vec![],
        open_items: vec![],
    }
}

fn write_retro_prompt(
    _run_store: &SlateRunStore,
    retro_dir: &Path,
    prompt: &str,
) -> anyhow::Result<()> {
    std::fs::write(retro_dir.join("prompt.md"), prompt)?;
    Ok(())
}

fn write_retro_response(
    _run_store: &SlateRunStore,
    retro_dir: &Path,
    response: &str,
) -> anyhow::Result<()> {
    std::fs::write(retro_dir.join("response.md"), response)?;
    Ok(())
}

/// Write retro artifact files (provider_used.json, status.json) into `retro_dir`.
/// Called on both success and failure paths so artifacts are always available for debugging.
fn write_retro_artifacts(
    retro_dir: &Path,
    provider: &str,
    model: &str,
    outcome: &str,
    failure_reason: Option<&str>,
) {
    let provider_used = serde_json::json!({
        "mode": "agent",
        "provider": provider,
        "model": model,
    });
    if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
        let _ = std::fs::write(retro_dir.join("provider_used.json"), json);
    }

    let status = serde_json::json!({
        "outcome": outcome,
        "failure_reason": failure_reason,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    });
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        let _ = std::fs::write(retro_dir.join("status.json"), json);
    }
}

/// Spawn a background task that reads `SessionEvent`s from the broadcast receiver
/// and appends them as JSONL to the given path.
fn spawn_retro_event_writer(mut rx: Receiver<SessionEvent>, path: PathBuf) -> JoinHandle<()> {
    tokio::spawn(async move {
        use std::io::Write;
        while let Ok(event) = rx.recv().await {
            if let Ok(line) = serde_json::to_string(&event) {
                let line = redact_jsonl_line(&line);
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
    })
}

/// Spawn a background task that forwards session events via the provided callback.
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
    run_store: &SlateRunStore,
    _run_dir: &Path,
    target_dir: &str,
) -> anyhow::Result<()> {
    // Create target directory
    sandbox
        .exec_command(&format!("mkdir -p {target_dir}"), 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create retro data dir: {e}"))?;

    let progress_content = match run_store.list_events().await {
        Ok(envelopes) => {
            let lines: Vec<String> = envelopes
                .into_iter()
                .filter_map(|env| serde_json::to_string(env.payload.as_value()).ok())
                .collect();
            if lines.is_empty() {
                None
            } else {
                Some(lines.join("\n") + "\n")
            }
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to load events from store: {e}")),
    };
    if let Some(content) = progress_content {
        sandbox
            .write_file(&format!("{target_dir}/progress.jsonl"), &content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload progress.jsonl: {e}"))?;
    }

    let state = run_store
        .state()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to load run state from store: {e}"))?;
    let checkpoint_content = state
        .checkpoint
        .map(|cp| serde_json::to_string_pretty(&cp))
        .transpose()?;
    upload_file(sandbox, target_dir, "checkpoint.json", checkpoint_content).await?;

    let run_content = state
        .run
        .map(|run| serde_json::to_string_pretty(&run))
        .transpose()?;
    upload_file(sandbox, target_dir, "run.json", run_content).await?;

    let start_content = state
        .start
        .map(|start| serde_json::to_string_pretty(&start))
        .transpose()?;
    upload_file(sandbox, target_dir, "start.json", start_content).await?;

    Ok(())
}

async fn upload_file(
    sandbox: &Arc<dyn Sandbox>,
    target_dir: &str,
    filename: &str,
    content: Option<String>,
) -> anyhow::Result<()> {
    if let Some(content) = content {
        sandbox
            .write_file(&format!("{target_dir}/{filename}"), &content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload {filename}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_agent::AgentEvent;
    use std::time::SystemTime;
    use tokio::sync::broadcast;

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
    fn write_retro_artifacts_does_not_clobber_prompt_md() {
        let dir = tempfile::tempdir().unwrap();
        let retro_dir = dir.path().join("retro");
        std::fs::create_dir_all(&retro_dir).unwrap();
        // prompt.md is written separately by run_retro_agent, not by write_retro_artifacts
        std::fs::write(retro_dir.join("prompt.md"), "Analyze the run data").unwrap();
        write_retro_artifacts(
            &retro_dir,
            "anthropic",
            "claude-sonnet-4-20250514",
            "success",
            None,
        );
        let content = std::fs::read_to_string(retro_dir.join("prompt.md")).unwrap();
        assert_eq!(content, "Analyze the run data");
    }

    #[test]
    fn writes_provider_used_json() {
        let dir = tempfile::tempdir().unwrap();
        let retro_dir = dir.path().join("retro");
        std::fs::create_dir_all(&retro_dir).unwrap();
        write_retro_artifacts(&retro_dir, "openai", "gpt-4o", "success", None);
        let content = std::fs::read_to_string(retro_dir.join("provider_used.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mode"], "agent");
        assert_eq!(parsed["provider"], "openai");
        assert_eq!(parsed["model"], "gpt-4o");
    }

    #[test]
    fn writes_status_json_success() {
        let dir = tempfile::tempdir().unwrap();
        let retro_dir = dir.path().join("retro");
        std::fs::create_dir_all(&retro_dir).unwrap();
        write_retro_artifacts(
            &retro_dir,
            "anthropic",
            "claude-sonnet-4-20250514",
            "success",
            None,
        );
        let content = std::fs::read_to_string(retro_dir.join("status.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["outcome"], "success");
        assert!(parsed["failure_reason"].is_null());
        assert!(parsed["timestamp"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn writes_status_json_failure() {
        let dir = tempfile::tempdir().unwrap();
        let retro_dir = dir.path().join("retro");
        std::fs::create_dir_all(&retro_dir).unwrap();
        write_retro_artifacts(
            &retro_dir,
            "anthropic",
            "claude-sonnet-4-20250514",
            "error",
            Some("Retro agent did not call submit_retro"),
        );
        let content = std::fs::read_to_string(retro_dir.join("status.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["outcome"], "error");
        assert_eq!(
            parsed["failure_reason"],
            "Retro agent did not call submit_retro"
        );
    }

    #[tokio::test]
    async fn event_writer_writes_session_events_to_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("retro_session.jsonl");

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);
        let handle = spawn_retro_event_writer(rx, jsonl_path.clone());

        tx.send(SessionEvent {
            event: AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model: Some("claude-opus".into()),
            },
            timestamp: SystemTime::now(),
            session_id: "retro-test".into(),
            parent_session_id: None,
        })
        .unwrap();

        // Drop sender so the receiver loop ends
        drop(tx);
        handle.await.unwrap();

        let content = std::fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["session_id"], "retro-test");
        assert!(lines[0].contains("SessionStarted"));
    }
}
