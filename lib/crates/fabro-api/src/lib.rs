#[allow(
    clippy::absolute_paths,
    clippy::all,
    clippy::derivable_impls,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::needless_lifetimes,
    clippy::unwrap_used,
    unreachable_pub,
    unused_imports,
    reason = "Generated OpenAPI client code intentionally preserves codegen output."
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/codegen.rs"));
}
pub mod types {
    pub use fabro_types::settings::server::{
        DiscordIntegrationSettings, GithubIntegrationSettings, GithubIntegrationStrategy,
        IntegrationWebhooksSettings, IpAllowEntry, ObjectStoreSettings, ServerApiSettings,
        ServerArtifactsSettings, ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings,
        ServerIntegrationsSettings, ServerIpAllowlistOverrideSettings, ServerIpAllowlistSettings,
        ServerListenSettings, ServerLoggingSettings, ServerSchedulerSettings,
        ServerSlateDbSettings, ServerStorageSettings, ServerWebSettings, SlackIntegrationSettings,
        TeamsIntegrationSettings, WebhookStrategy,
    };
    pub use fabro_types::settings::{FeaturesNamespace, ServerNamespace};
    pub use fabro_types::status::{
        BlockedReason, FailureReason, RunControlAction, RunStatus, SuccessReason, TerminalStatus,
    };
    pub use fabro_types::{ServerSettings, WorkflowSettings};

    pub use crate::generated::types::*;
}
pub use generated::Client as ApiClient;
