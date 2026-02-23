pub mod cli;
pub mod config;
pub mod error;
pub mod event;
pub mod execution_env;
pub mod history;
pub mod local_env;
pub mod loop_detection;
pub mod profiles;
pub mod project_docs;
pub mod provider_profile;
pub mod session;
pub mod subagent;
pub mod tool_registry;
pub mod tools;
pub mod truncation;
pub mod types;

pub use config::{SessionConfig, ToolApprovalFn};
pub use error::AgentError;
pub use event::EventEmitter;
pub use execution_env::{DirEntry, ExecResult, ExecutionEnvironment, GrepOptions};
pub use history::History;
pub use local_env::LocalExecutionEnvironment;
pub use loop_detection::detect_loop;
pub use project_docs::discover_project_docs;
pub use profiles::{AnthropicProfile, EnvContext, GeminiProfile, OpenAiProfile};
pub use provider_profile::{ProfileCapabilities, ProviderProfile};
pub use session::Session;
pub use subagent::{SubAgent, SubAgentManager, SubAgentResult};
pub use tool_registry::ToolRegistry;
pub use tools::{
    make_edit_file_tool, make_glob_tool, make_grep_tool, make_read_file_tool, make_shell_tool,
    make_shell_tool_with_config, make_write_file_tool,
};
pub use truncation::{truncate_lines, truncate_output, truncate_tool_output, TruncationMode};
pub use types::{EventData, EventKind, SessionEvent, SessionState, Turn};

#[cfg(test)]
pub(crate) mod test_support;
