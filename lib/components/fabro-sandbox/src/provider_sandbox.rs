//! Run sandboxes on any provider fabro can name: a bundled kind in process
//! or a sandbox-driver plugin executable.
//!
//! One path builds them all. The environment's spec arrives built (see
//! [`crate::environment`]), the provider is connected through the single
//! construction function, and a bundled provider adds only what its
//! backend needs on top: Docker its fixed working directory and default
//! image, Daytona its fixed working directory, default snapshot, and
//! lifecycle timers, the Host the designated directory it works in,
//! created when missing. A plugin gets the spec as is, trimmed to what it
//! can honor, laid out inside the working directory the provider chooses.

use std::path::PathBuf;
use std::sync::Arc;

use fabro_github::GitHubCredentials;
use fabro_types::{BundledProvider, RunId, SandboxProviderKind};
use sandbox_driver::{
    EventContext, OwnedProvider, SandboxId, SandboxProvider, SandboxSource,
    SandboxSpec as DriverSpec,
};
use tokio::fs;

use crate::driver::{ProviderAccess, connect_provider};
use crate::driver_sandbox::{LayoutSource, RepoWorkspace, RunSandbox};
use crate::environment::{self, CloneRequest};
use crate::sandbox_spec::SandboxSpec;
use crate::{daytona, docker, managed_labels};

/// A sandbox for a run on `kind`. The sandbox is created by `initialize`;
/// construction validates the clone request and connects the provider, so
/// a bad request, a missing credential, or a missing plugin executable
/// fails before any backend call.
pub async fn provider_sandbox(
    kind: SandboxProviderKind,
    access: &ProviderAccess,
    spec: DriverSpec,
    clone: &CloneRequest,
    github_app: Option<&GitHubCredentials>,
    run_id: Option<RunId>,
) -> crate::Result<RunSandbox> {
    let workspace = RepoWorkspace::plan(layout_source(&kind), clone, github_app)?;
    let provider = connect(&kind, access, run_id.as_ref()).await?;
    let mut spec = spec;
    if let Some(run_id) = &run_id {
        spec = spec.name(environment::run_name(run_id));
    }
    Ok(match kind.bundled() {
        Some(BundledProvider::Docker) => {
            RunSandbox::pending(kind, provider, docker::overlay(spec), workspace)
        }
        Some(BundledProvider::Daytona) => RunSandbox::pending(
            kind,
            provider,
            daytona::overlay(spec, run_id.as_ref()),
            workspace,
        ),
        Some(BundledProvider::Local) | None => {
            if kind.is_local() {
                designate_directory(&spec).await?;
            }
            let capabilities = provider.capabilities();
            spec.network = environment::supported_network(spec.network, capabilities);
            spec.timers = environment::supported_timers(spec.timers, capabilities);
            RunSandbox::pending(kind, provider, spec, workspace)
        }
    })
}

/// The Host provider works in a designated directory in place and needs it
/// to exist. A run may point at a fresh scratch path, so the directory is
/// created before the provider sees the spec.
async fn designate_directory(spec: &DriverSpec) -> crate::Result<()> {
    let Some(directory) = &spec.working_directory else {
        return Ok(());
    };
    fs::create_dir_all(directory).await.map_err(|error| {
        crate::Error::context(
            format!("Failed to create working directory {directory}"),
            error,
        )
    })
}

/// A sandbox on this host at `working_directory`, ready to use: the `local`
/// kind, built through the provider path with default settings and
/// initialized. For the agent CLI and tests; a run builds its sandbox from
/// its [`SandboxSpec`] and initializes it itself.
pub async fn local_sandbox(working_directory: impl Into<PathBuf>) -> crate::Result<RunSandbox> {
    let spec = SandboxSpec::local(working_directory, ProviderAccess::default());
    let sandbox =
        provider_sandbox(spec.kind, &spec.access, spec.spec, &spec.clone, None, None).await?;
    sandbox.initialize().await?;
    Ok(sandbox)
}

/// Reattach to a run's sandbox on `kind` by its persisted id. The driver
/// reports the sandbox's lifecycle from here on through `events`.
///
/// On a shared backend the sandbox must carry fabro's managed label and,
/// when a run id is known, the matching run label: fabro never operates on
/// a sandbox it did not create, and the ownership scope the provider is
/// connected through refuses anything else. A local sandbox attaches by
/// the id the Host provider derives from its directory.
pub async fn attach_provider_sandbox(
    kind: SandboxProviderKind,
    access: &ProviderAccess,
    sandbox_id: &str,
    repo_cloned: bool,
    working_directory: String,
    clone_origin_url: Option<String>,
    run_id: Option<RunId>,
    events: Option<EventContext>,
) -> crate::Result<RunSandbox> {
    let provider = connect(&kind, access, run_id.as_ref()).await?;
    let id = SandboxId::try_new(sandbox_id)
        .map_err(|error| crate::Error::context(format!("Invalid {kind} sandbox id"), error))?;
    let handle = match provider.attach(&id, events.clone()).await {
        Ok(handle) => handle,
        // A host sandbox is the directory it designates. An id the host
        // provider minted for a long path lives only in the registry of the
        // process that created it (a run's Petri worker, say), so a
        // reconnect from another process designates the directory again:
        // the same workspace, whatever the id.
        Err(error)
            if kind.bundled() == Some(BundledProvider::Local)
                && matches!(error, sandbox_driver::Error::NotFound { .. }) =>
        {
            let spec = DriverSpec::new(SandboxSource::HostDirectory)
                .working_directory(working_directory.clone());
            provider.create(&spec, events).await.map_err(|error| {
                crate::Error::context(
                    format!(
                        "Failed to reconnect {kind} sandbox '{sandbox_id}' at {working_directory}"
                    ),
                    error,
                )
            })?
        }
        Err(error) => {
            return Err(crate::Error::context(
                format!("Failed to reconnect {kind} sandbox '{sandbox_id}'"),
                error,
            ));
        }
    };
    let status = handle.describe().await?;
    let workspace = RepoWorkspace::attached(
        layout_source(&kind),
        repo_cloned,
        working_directory,
        clone_origin_url,
    );
    let sandbox = RunSandbox::attached(kind, handle, workspace);
    if let Some(snapshot) = status.snapshot {
        sandbox.set_snapshot(snapshot);
    }
    Ok(sandbox)
}

/// The image the run record names for a sandbox on `kind`: the
/// environment's, or Docker's default when the environment names none.
pub(crate) fn recorded_image(kind: &SandboxProviderKind, spec: &DriverSpec) -> Option<String> {
    match (kind.bundled(), &spec.source) {
        (Some(BundledProvider::Docker), _) => Some(docker::effective_image(spec)),
        (_, SandboxSource::Image { reference }) => Some(reference.clone()),
        _ => None,
    }
}

/// Where a run's repository checks out on `kind`: fabro fixes the roots
/// inside the containers and VMs it shapes itself, and follows the working
/// directory a plugin provider chooses.
pub(crate) fn layout_source(kind: &SandboxProviderKind) -> LayoutSource {
    match kind.bundled() {
        Some(BundledProvider::Docker) => LayoutSource::Fixed(docker::layout()),
        Some(BundledProvider::Daytona) => LayoutSource::Fixed(daytona::layout()),
        Some(BundledProvider::Local) | None => LayoutSource::ProviderWorkingDirectory,
    }
}

/// The in-process Docker provider with default settings, for `fabro doctor`.
pub(crate) async fn connect_bundled_docker(
    access: &ProviderAccess,
) -> crate::Result<Arc<dyn SandboxProvider>> {
    connect(&SandboxProviderKind::DOCKER, access, None).await
}

const MISSING_DAYTONA_CREDENTIALS: &str = "Daytona sandboxes require DAYTONA_API_KEY in the vault; run `fabro secret set DAYTONA_API_KEY`";

/// The provider for `kind`, scoped to the sandboxes fabro owns — narrowed to
/// one run when `run_id` is known — so creates carry fabro's labels and
/// attaches to anything else are refused.
async fn connect(
    kind: &SandboxProviderKind,
    access: &ProviderAccess,
    run_id: Option<&RunId>,
) -> crate::Result<Arc<dyn SandboxProvider>> {
    if kind.bundled() == Some(BundledProvider::Daytona) && access.daytona.is_none() {
        return Err(crate::Error::message(MISSING_DAYTONA_CREDENTIALS));
    }
    let settings = access.settings_for(kind).ok_or_else(|| {
        crate::Error::message(format!(
            "sandbox provider `{kind}` is not configured; add [server.sandbox.providers.{kind}] to settings.toml"
        ))
    })?;
    let connected = connect_provider(kind, &settings, &access.connect_options())
        .await
        .map_err(|error| {
            crate::Error::context(format!("Failed to connect to the {kind} provider"), error)
        })?;
    // A local sandbox is a directory the caller designated; it carries no
    // labels, and nothing else shares the host's directories with fabro.
    if kind.bundled() == Some(BundledProvider::Local) {
        return Ok(connected.provider);
    }
    Ok(Arc::new(OwnedProvider::new(
        connected.provider,
        managed_labels::ownership(run_id),
    )))
}
