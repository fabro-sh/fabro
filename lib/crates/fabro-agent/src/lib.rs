#[cfg(feature = "docker")]
pub mod docker_sandbox;

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
pub mod provider_profile;
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

pub use config::{SessionConfig, ToolApprovalAdapter, ToolHookCallback, ToolHookDecision};
#[cfg(feature = "docker")]
pub use docker_sandbox::{DockerSandbox, DockerSandboxConfig};
pub use error::{AbortReason, AgentError};
pub use event::EventEmitter;
pub use fabro_mcp::config::McpServerConfig;
pub use history::History;
pub use local_sandbox::LocalSandbox;
pub use loop_detection::detect_loop;
pub use memory::discover_memory;
pub use profiles::{AnthropicProfile, EnvContext, GeminiProfile, OpenAiProfile};
pub use provider_profile::{ProfileCapabilities, ProviderProfile};
pub use read_before_write_sandbox::ReadBeforeWriteSandbox;
pub use sandbox::{
    format_lines_numbered, shell_quote, DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent,
    SandboxEventCallback, WorktreeConfig, WorktreeEvent, WorktreeEventCallback, WorktreeSandbox,
};
pub use session::Session;
pub use skills::Skill;
pub use subagent::{SubAgent, SubAgentEventCallback, SubAgentManager, SubAgentResult};
pub use tool_registry::ToolRegistry;
pub use tools::{
    make_edit_file_tool, make_glob_tool, make_grep_tool, make_read_file_tool, make_shell_tool,
    make_shell_tool_with_config, make_write_file_tool, register_core_tools, WebFetchSummarizer,
};
pub use truncation::{
    floor_char_boundary, truncate_lines, truncate_output, truncate_tool_output, TruncationMode,
};
pub use types::{AgentEvent, SessionEvent, SessionState, Turn};

#[cfg(test)]
pub(crate) mod test_support;
