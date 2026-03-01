use std::path::Path;
use std::sync::{Arc, Mutex};

use arc_agent::{
    AnthropicProfile, ExecutionEnvironment, GeminiProfile, OpenAiProfile, ProviderProfile,
    Session, SessionConfig,
};
use arc_llm::client::Client;
use arc_llm::provider::Provider;
use arc_llm::types::ToolDefinition;

use crate::retro::RetroNarrative;

const RETRO_SYSTEM_PROMPT: &str = r#"You are a pipeline retrospective analyst. Your job is to analyze a completed pipeline run and generate a structured retrospective.

You have access to the pipeline's data files:
- `progress.ndjson` — the full event stream (stage starts/completions, agent tool calls, errors, retries)
- `checkpoint.json` — final execution state with node outcomes
- `manifest.json` — run metadata (if available)

## Your task

1. **Explore the data** using grep and read tools to understand what happened:
   - Look for failures, retries, and error messages
   - Check agent tool call patterns for wrong approaches or pivots
   - Note which stages took longest or had issues
   - Look for patterns indicating friction (repeated similar tool calls, error recovery)

2. **Call the `submit_retro` tool** with your structured analysis.

## Smoothness grading guidelines

Grade the run on a 5-point scale:

- **effortless** — Pipeline achieved its goal on the first try with no retries, no wrong approaches. Agent moved efficiently from start to finish.
- **smooth** — Goal achieved with minor hiccups (1-2 retries or a brief wrong approach quickly corrected). No human intervention needed. Overall clean execution.
- **bumpy** — Goal achieved but with notable friction: multiple retries, at least one significant wrong approach, or substantial time spent on dead ends.
- **struggled** — Goal achieved only with difficulty: many retries, major approach changes, human intervention, or partial failures requiring recovery.
- **failed** — Pipeline did not achieve its stated goal. May have completed some stages but the overall intent was not fulfilled.

Consider the full context: not just stage pass/fail, but the quality of the journey visible in the agent events (tool call patterns, error recovery, approach pivots).

## Guidelines for qualitative fields

- **intent**: What was the pipeline trying to accomplish? Summarize the goal in a sentence.
- **outcome**: What actually happened? Did it succeed? What was produced?
- **learnings**: What was discovered about the repo, code, workflow, or tools?
- **friction_points**: Where did things get stuck? What caused slowdowns?
- **open_items**: What follow-up work, tech debt, or gaps were identified?

Be specific and concise. Reference actual stage names, file paths, and error messages where relevant."#;

const SUBMIT_RETRO_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "smoothness": {
      "type": "string",
      "enum": ["effortless", "smooth", "bumpy", "struggled", "failed"],
      "description": "Overall smoothness rating for the pipeline run"
    },
    "intent": {
      "type": "string",
      "description": "What was the pipeline trying to accomplish?"
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

/// Run a retro agent session that analyzes pipeline run data and produces
/// a structured narrative. The agent explores `progress.ndjson` and other
/// files via tool access, then calls `submit_retro` with its analysis.
pub async fn run_retro_agent(
    execution_env: &Arc<dyn ExecutionEnvironment>,
    logs_root: &Path,
    llm_client: &Client,
    provider: Provider,
    model: &str,
) -> anyhow::Result<RetroNarrative> {
    // Upload data files into execution env (needed for Daytona; no-op effect for local
    // since the agent can also read from the original paths via tools).
    let retro_data_dir = "/tmp/retro_data";
    upload_data_files(execution_env, logs_root, retro_data_dir).await?;

    // Build provider profile with the submit_retro tool
    let captured: Arc<Mutex<Option<RetroNarrative>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);

    let mut profile = build_profile(provider, model);

    // Register submit_retro tool
    let submit_tool = arc_agent::tool_registry::RegisteredTool {
        definition: ToolDefinition {
            name: "submit_retro".to_string(),
            description: "Submit the structured retrospective analysis. Call this once you have analyzed the pipeline run data.".to_string(),
            parameters: serde_json::from_str(SUBMIT_RETRO_SCHEMA)
                .expect("submit_retro schema should be valid JSON"),
        },
        executor: Arc::new(move |args, _ctx| {
            let captured = Arc::clone(&captured_clone);
            Box::pin(async move {
                let narrative: RetroNarrative = serde_json::from_value(args)
                    .map_err(|e| format!("Invalid retro submission: {e}"))?;
                *captured.lock().unwrap() = Some(narrative);
                Ok("Retrospective submitted successfully.".to_string())
            })
        }),
    };
    profile.tool_registry_mut().register(submit_tool);

    let profile: Arc<dyn ProviderProfile> = Arc::from(profile);

    let config = SessionConfig {
        max_tool_rounds_per_input: 10,
        // Disable features not needed for retro analysis
        enable_context_compaction: false,
        skill_dirs: Some(vec![]),
        user_instructions: Some(RETRO_SYSTEM_PROMPT.to_string()),
        ..SessionConfig::default()
    };

    let mut session = Session::new(
        llm_client.clone(),
        profile,
        Arc::clone(execution_env),
        config,
    );

    session.initialize().await;

    let prompt = format!(
        "Analyze the pipeline run data at `{retro_data_dir}/` and generate a retrospective. \
         The key file is `{retro_data_dir}/progress.ndjson` which contains the full event stream. \
         Also check `{retro_data_dir}/checkpoint.json` for stage outcomes. \
         Use grep to search for interesting signals (failures, retries, errors, approach changes) \
         rather than reading the entire file. When done, call the `submit_retro` tool with your analysis."
    );

    session.process_input(&prompt).await.map_err(|e| {
        anyhow::anyhow!("Retro agent session failed: {e}")
    })?;

    // Extract the captured narrative
    let narrative = captured
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| anyhow::anyhow!("Retro agent did not call submit_retro"))?;

    Ok(narrative)
}

fn build_profile(provider: Provider, model: &str) -> Box<dyn ProviderProfile> {
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::new(model)),
        Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => {
            Box::new(OpenAiProfile::new(model).with_provider(provider))
        }
        Provider::Gemini => Box::new(GeminiProfile::new(model)),
        Provider::Anthropic => Box::new(AnthropicProfile::new(model)),
    }
}

async fn upload_data_files(
    execution_env: &Arc<dyn ExecutionEnvironment>,
    logs_root: &Path,
    target_dir: &str,
) -> anyhow::Result<()> {
    // Create target directory
    execution_env
        .exec_command(&format!("mkdir -p {target_dir}"), 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create retro data dir: {e}"))?;

    let files = ["progress.ndjson", "checkpoint.json", "manifest.json"];
    for filename in &files {
        let source = logs_root.join(filename);
        if source.exists() {
            let content = std::fs::read_to_string(&source)?;
            execution_env
                .write_file(&format!("{target_dir}/{filename}"), &content)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to upload {filename}: {e}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            narrative.smoothness,
            crate::retro::SmoothnessRating::Smooth
        );
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
        assert_eq!(
            narrative.smoothness,
            crate::retro::SmoothnessRating::Effortless
        );
        assert!(narrative.learnings.is_empty());
        assert!(narrative.friction_points.is_empty());
        assert!(narrative.open_items.is_empty());
    }
}
