#[cfg(feature = "docker")]
pub mod docker_sandbox;

pub mod agent_profile;
pub mod cli;
pub mod compaction;
pub mod config;
pub mod error;
pub mod event;
pub mod file_tracker;
pub mod history;
pub mod local_sandbox;
pub mod loop_detection;
pub mod mcp_integration;
pub mod memory;
pub mod profiles;
pub mod read_before_write_sandbox;
pub mod sandbox;
pub mod session;
pub mod skills;
pub mod subagent;
pub mod tool_execution;
pub mod tool_registry;
pub mod tools;
pub mod truncation;
pub mod types;
pub mod v4a_patch;

pub use agent_profile::AgentProfile;
pub use config::{SessionOptions, ToolApprovalAdapter, ToolHookCallback, ToolHookDecision};
#[cfg(feature = "docker")]
pub use docker_sandbox::{DockerSandbox, DockerSandboxOptions};
pub use error::{Error, InterruptReason, Result};
pub use event::Emitter;
pub use fabro_mcp::config::McpServerSettings;
pub use history::History;
pub use local_sandbox::LocalSandbox;
pub use loop_detection::detect_loop;
pub use memory::discover_memory;
pub use profiles::{AnthropicProfile, EnvContext, GeminiProfile, OpenAiProfile};
pub use read_before_write_sandbox::ReadBeforeWriteSandbox;
pub use sandbox::{
    CommandOutputCallback, DirEntry, ExecResult, ExecStreamingResult, GrepOptions, Sandbox,
    SandboxEvent, SandboxEventCallback, WorktreeEvent, WorktreeEventCallback, WorktreeOptions,
    WorktreeSandbox, format_lines_numbered, shell_quote,
};
pub use session::{CompletionCoordinator, Session, SessionControlHandle, SteeringItem};
pub use skills::Skill;
pub use subagent::{
    SubAgent, SubAgentEventCallback, SubAgentManager, SubAgentResult, SubAgentStatus,
};
pub use tool_registry::ToolRegistry;
pub use tools::{
    WebFetchSummarizer, make_edit_file_tool, make_glob_tool, make_grep_tool, make_read_file_tool,
    make_shell_tool, make_shell_tool_with_config, make_write_file_tool, register_core_tools,
};
pub use truncation::{TruncationMode, truncate_lines, truncate_output, truncate_tool_output};
pub use types::{AgentEvent, SessionEvent, SessionState, SteerKind, Turn};

#[cfg(test)]
#[allow(
    unreachable_pub,
    reason = "Test support stays crate-visible for cross-module unit tests."
)]
pub(crate) mod test_support;
