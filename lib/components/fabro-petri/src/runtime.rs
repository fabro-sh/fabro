//! The Petri runtime Fabro runs its workflows on, assembled the same way at
//! create time (for `Runtime::check`) and at execution.
//!
//! The pieces are Petri's own: [`Runtime::standard`] with the Fabro frontend
//! carrying the server's settings layer, the Attractor step kinds (the real
//! ones, or the simulated registry for a dry run), the model client as the
//! `PebbleClient` capability so Petri's admission pass pins every LLM node's
//! route, the Fabro home for the skills step, and, at execution, Fabro's run
//! tools as the `HostTools` capability when the run enables them. Nothing
//! here knows about a run's record: the store and the run options are added
//! by the caller.

use std::path::PathBuf;
use std::sync::Arc;

use fabro_http::HttpClient;
use fabro_workflow::services::FabroRunToolServices;
use lithos_llm::Client;
use lithos_llm::catalog::{Catalog, ProviderId};
use lithos_llm::client::ClientBuildError;
use lithos_llm::credentials::CredentialProvider;
use petri_attractor_steps::pebble::PebbleClient;
use petri_attractor_steps::skills::FabroHome;
use petri_frontend_fabro::Fabro;
use petri_runtime::Runtime;
use tracing::debug;

use crate::host_tools;

/// What every Petri runtime Fabro builds is configured with.
#[derive(Clone, Default)]
pub struct RuntimeSpec {
    /// The operator's settings layer, as `~/.fabro/settings.toml` text: the
    /// lowest of the three layers the Fabro frontend reads (`[run.model]`
    /// defaults, `[[run.hooks]]`, `[run.agent.mcps]`, `[run.environment]`
    /// and the `[environments.<id>]` catalog a bundle may name).
    pub settings_toml:    Option<String>,
    /// The server's MCP catalog, as the TOML text the Fabro frontend
    /// resolves `[run.agent.mcps.<name>] id = "..."` references against: a
    /// table keyed by catalog id, each entry in the inline
    /// `[run.agent.mcps.<name>]` shape. `None` leaves every reference
    /// refused, as the standalone runner refuses it.
    pub mcp_catalog_toml: Option<String>,
    /// The model client the native agent and prompt steps call, and the
    /// catalog the admission pass resolves model selectors against. `None`
    /// leaves every LLM node unpinned and every model call unconfigured.
    pub model_client:     Option<Client>,
    /// Run the simulated step registry (Fabro's `--dry-run` handlers)
    /// instead of the real one.
    pub dry_run:          bool,
    /// The Fabro home the skills step reads; `None` leaves it to Petri's
    /// own lookup (`FABRO_HOME`, else `$HOME/.fabro`).
    pub fabro_home:       Option<PathBuf>,
    /// Fabro's run tools for every native agent session of the run, when
    /// the run enables them (`[run.agent] fabro_tools` and the worker
    /// token's `agent:run_tools` scope); `None` gives the sessions Pebble's
    /// tools alone. See [`crate::host_tools`].
    pub run_tools:        Option<FabroRunToolServices>,
}

impl RuntimeSpec {
    /// Assemble the runtime. The admission pass that pins models is part of
    /// the real registry, so a dry run's `check` still uses the real
    /// registry: only execution swaps in the stubs.
    #[must_use]
    pub fn runtime(&self, for_execution: bool) -> Runtime {
        let mut runtime = Runtime::standard().frontend(
            Fabro::new()
                .with_settings_toml(self.settings_toml.clone())
                .with_mcp_catalog_toml(self.mcp_catalog_toml.clone()),
        );
        if let Some(client) = &self.model_client {
            runtime = runtime.capability(PebbleClient(client.clone()));
        }
        let home = self
            .fabro_home
            .clone()
            .map(FabroHome)
            .or_else(FabroHome::from_env);
        if let Some(home) = home {
            runtime = runtime.capability(home);
        }
        if let Some(services) = &self.run_tools {
            runtime = runtime.capability(host_tools::capability(services.clone()));
        }
        if for_execution && self.dry_run {
            petri_attractor_steps::register_stubs(runtime)
        } else {
            petri_attractor_steps::register(runtime)
        }
    }
}

/// The model client Fabro hands Petri: the server's catalog, its credential
/// provider, its HTTP client (so a test's loopback client and a server's
/// proxy policy carry over), and only the providers whose credentials are
/// ready, the same eligible set the legacy compiler pinned models against.
/// `None` when no provider is eligible, so Petri's admission pass leaves the
/// graph alone rather than refusing every model.
pub fn model_client(
    catalog: Catalog,
    credentials: Arc<dyn CredentialProvider>,
    http: Option<HttpClient>,
    eligible: &[ProviderId],
) -> Result<Option<Client>, ClientBuildError> {
    if eligible.is_empty() {
        debug!("no eligible model provider; the Petri runtime gets no model client");
        return Ok(None);
    }
    let mut builder = Client::builder()
        .catalog(catalog)
        .credentials_arc(credentials)
        .enabled_providers(eligible.iter().cloned());
    if let Some(http) = http {
        builder = builder.http(http);
    }
    let build = builder.build()?;
    for issue in &build.issues {
        debug!(provider = %issue.provider, cause = %issue.cause, "model provider unavailable");
    }
    Ok(Some(build.client))
}
