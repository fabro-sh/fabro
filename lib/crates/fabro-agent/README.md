# agent

A programmable agentic loop for building coding agents. This crate provides the core session management, tool execution, and LLM interaction loop used to power interactive coding assistants.

## Architecture

The crate is organized around a central `Session` that drives an agentic loop:

1. **User input** is appended to a conversation `History`
2. The session builds a `Request` with system prompt, history, and tools
3. An LLM generates a response (text and/or tool calls) via `unified-llm`
4. Tool calls are executed through a `ToolRegistry` against a `Sandbox`
5. Results are recorded and the loop continues until the LLM responds with text only (natural completion), a turn limit is reached, or the session is aborted

```
User Input
    |
    v
[Session::process_input]
    |
    v
+-------------------+
| Build Request     |  <-- system prompt + history + tools
+-------------------+
    |
    v
+-------------------+
| LLM Call          |  <-- via unified-llm Client
+-------------------+
    |
    v
+-------------------+     +-------------------+
| Tool Calls?  -----+-yes-| Execute Tools     |
+-------------------+     | (parallel or seq)  |
    | no                  +-------------------+
    v                         |
  [Done]                      +---> loop back to Build Request
```

### Key Components

- **`Session`** -- Manages the full agentic loop: LLM calls, tool execution, steering, follow-ups, abort handling, and event emission.
- **`AgentProfile`** (trait) -- Defines how to build system prompts, which tools to register, and what capabilities a provider supports. Ships with `AnthropicProfile`, `OpenAiProfile`, and `GeminiProfile`.
- **`Sandbox`** (trait) -- Abstracts filesystem, shell, grep, and glob operations. `LocalSandbox` provides a real implementation; the trait enables sandboxing and testing.
- **`ToolRegistry`** -- Maps tool names to definitions and async executor functions. Tools are registered per-profile.
- **`History`** -- Ordered list of `Turn` variants (`User`, `Assistant`, `ToolResults`, `System`, `Steering`) that converts to LLM messages.
- **`Emitter`** -- Broadcasts `SessionEvent`s (tool calls, text, errors, warnings) over a `tokio::sync::broadcast` channel for UI or logging.
- **`SubAgentManager`** -- Spawns child `Session`s on background tasks for delegated work, with depth limits.
- **`SessionConfig`** -- Tunable parameters: max turns, tool round limits, command timeouts, loop detection, output truncation limits, and user instructions.

## Key Types and Traits

### `Session`

The main entry point. Created with an LLM client, a provider profile, a sandbox, and a config.

### `AgentProfile`

```rust
pub trait AgentProfile: Send + Sync {
    fn id(&self) -> String;
    fn model(&self) -> String;
    fn tool_registry(&self) -> &ToolRegistry;
    fn build_system_prompt(
        &self,
        env: &dyn Sandbox,
        env_context: &EnvContext,
        project_docs: &[String],
        user_instructions: Option<&str>,
    ) -> String;
    // ... default methods for tools(), knowledge_cutoff(), context_window_size()
}
```

Built-in profiles:
- **`AnthropicProfile`** -- 200K context, extended thinking beta headers, tools: `read_file`, `write_file`, `edit_file`, `shell`, `grep`, `glob`
- **`OpenAiProfile`** -- 128K context, reasoning effort support, tools: `read_file`, `write_file`, `shell`, `grep`, `glob`, `apply_patch` (v4a format)
- **`GeminiProfile`** -- 1M context, safety settings, tools: all Anthropic tools plus `read_many_files`, `list_dir`, `web_search`, `web_fetch`

### `Sandbox`

```rust
pub trait Sandbox: Send + Sync {
    async fn read_file(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String, String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<(), String>;
    async fn exec_command(&self, command: &str, timeout_ms: u64, ...) -> Result<ExecResult, String>;
    async fn grep(&self, pattern: &str, path: &str, options: &GrepOptions) -> Result<Vec<String>, String>;
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String>;
    // ... plus delete_file, file_exists, list_directory, initialize, cleanup, platform info
}
```

`LocalSandbox` is the real implementation with env-var filtering (strips secrets), process group management, and ripgrep/grep fallback.

### `SessionConfig`

```rust
pub struct SessionConfig {
    pub max_turns: usize,                    // 0 = unlimited
    pub max_tool_rounds_per_input: usize,    // default: 200
    pub default_command_timeout_ms: u64,     // default: 10s
    pub max_command_timeout_ms: u64,         // default: 600s
    pub enable_loop_detection: bool,         // default: true
    pub loop_detection_window: usize,        // default: 10
    pub max_subagent_depth: usize,           // default: 1
    pub user_instructions: Option<String>,
    pub reasoning_effort: Option<String>,
    // ... plus tool_output_limits, tool_line_limits, git_root
}
```

## Usage

```rust
use agent::{
    AnthropicProfile, LocalSandbox, Session, SessionConfig,
};
use std::path::PathBuf;
use std::sync::Arc;
use unified_llm::client::Client;

// 1. Create an LLM client (via unified-llm)
let client: Client = /* configure unified-llm client */;

// 2. Choose a provider profile
let profile = Arc::new(AnthropicProfile::new("claude-sonnet-4-20250514"));

// 3. Create a sandbox
let env = Arc::new(LocalSandbox::new(
    PathBuf::from("/path/to/project"),
));

// 4. Configure the session
let config = SessionConfig {
    max_tool_rounds_per_input: 50,
    enable_loop_detection: true,
    user_instructions: Some("Always write tests first".into()),
    ..SessionConfig::default()
};

// 5. Create and initialize the session
let mut session = Session::new(client, profile, env, config, None);
session.initialize().await;

// 6. Subscribe to events (for UI rendering)
let mut rx = session.subscribe();
tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
        // Handle SessionEvent: tool calls, text, errors, etc.
    }
});

// 7. Process user input
session.process_input("Fix the failing test in src/lib.rs").await?;
```

### Steering and Follow-ups

Inject guidance mid-conversation or queue follow-up messages:

```rust
// Inject a steering message before the next LLM call
session.steer("Focus on the root cause, not symptoms".into());

// Queue a follow-up that runs after the current input completes
session.follow_up("Now run the test suite to verify".into());
```

### Abort

Cancel a running session from another thread:

```rust
let cancel_token = session.cancel_token();
// From another task:
cancel_token.cancel();
```

### Custom Tools

Register additional tools via the profile's `ToolRegistry`:

```rust
use agent::tool_registry::{RegisteredTool, ToolExecutor};
use unified_llm::types::ToolDefinition;
use std::sync::Arc;

let custom_tool = RegisteredTool {
    definition: ToolDefinition {
        name: "my_tool".into(),
        description: "Does something useful".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "input": {"type": "string"}
            },
            "required": ["input"]
        }),
    },
    executor: Arc::new(|args, env| {
        Box::pin(async move {
            let input = args["input"].as_str().unwrap_or("");
            Ok(format!("Processed: {input}"))
        })
    }),
};

// Register on a mutable profile before creating the session
profile.tool_registry_mut().register(custom_tool);
```

### Subagents

Spawn child sessions for delegated tasks:

```rust
use agent::subagent::SubAgentManager;

let mut profile = AnthropicProfile::new("claude-sonnet-4-20250514");
let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(3)));
let factory = Arc::new(|| { /* create a new Session */ });

// Registers spawn_agent, send_input, wait, close_agent tools
profile.register_subagent_tools(manager, factory, 0);
```

## Safety Features

- **Loop detection** -- Detects repeating tool call patterns (period 1, 2, or 3) and injects a steering warning
- **Context window monitoring** -- Emits `Warning` events (kind `"context_window"`) when estimated usage exceeds 80%
- **Tool argument validation** -- Validates arguments against JSON Schema before execution
- **Tool output truncation** -- Per-tool character and line limits with head/tail or tail-only truncation modes
- **Environment variable filtering** -- `LocalSandbox` strips secrets (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`, `*_CREDENTIAL`) from subprocess environments
- **Command timeouts** -- Configurable per-command with process group cleanup (SIGTERM then SIGKILL)
- **Project doc discovery** -- Automatically discovers `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, or `.codex/instructions.md` based on provider, with a 32KB budget
