pub mod bridge;
pub mod config;
pub mod executor;
pub mod runner;
pub mod types;

pub use bridge::WorkflowToolHookCallback;
pub use config::{HookDefinition, HookSettings, HookType, TlsMode};
// Re-exported because the interpolatable fields of `HookType` are typed as
// `InterpString`; constructing a hook definition requires it.
pub use fabro_types::settings::InterpString;
pub use runner::HookRunner;
pub use types::{HookContext, HookDecision, HookEvent, HookExecutionContext};
