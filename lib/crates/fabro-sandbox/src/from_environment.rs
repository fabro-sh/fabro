//! Convert resolved [`RunEnvironmentSettings`] into runtime sandbox configs.
//!
//! These mappings are consumed by both the workflow run-start path and the
//! server preflight path, so they live here next to their destination types.

use std::path::{Path, PathBuf};

use fabro_types::settings::ResolveError;
#[cfg(feature = "daytona")]
use fabro_types::settings::run::DockerfileSource as ResolvedDockerfileSource;
use fabro_types::settings::run::{EnvironmentNetworkMode, RunEnvironmentSettings};

#[cfg(feature = "daytona")]
use crate::config::{
    DaytonaNetwork, DaytonaSnapshotSettings, DockerfileSource as SandboxDockerfileSource,
};
#[cfg(feature = "forkd")]
use crate::config::{ForkdNetwork, ForkdSettings, ForkdSnapshotSettings};
#[cfg(feature = "daytona")]
use crate::daytona::DaytonaConfig;
#[cfg(feature = "docker")]
use crate::docker::DockerSandboxOptions;
#[cfg(feature = "forkd")]
use crate::forkd::DEFAULT_SNAPSHOT_TAG;
#[cfg(feature = "forkd")]
use crate::forkd::ForkdConfig;

#[cfg(feature = "daytona")]
#[must_use]
pub fn daytona_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DaytonaConfig {
    DaytonaConfig {
        auto_stop_interval: settings
            .lifecycle
            .auto_stop
            .map(|duration| duration_to_minutes_i32(duration.as_std())),
        labels: (!settings.labels.is_empty()).then(|| settings.labels.clone()),
        snapshot: settings
            .image
            .dockerfile
            .as_ref()
            .map(|dockerfile| DaytonaSnapshotSettings {
                cpu:        settings.resources.cpu,
                memory:     settings
                    .resources
                    .memory
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                disk:       settings
                    .resources
                    .disk
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                dockerfile: Some(match dockerfile {
                    ResolvedDockerfileSource::Inline(text) => {
                        SandboxDockerfileSource::Inline(text.clone())
                    }
                    ResolvedDockerfileSource::Path { path } => {
                        SandboxDockerfileSource::Path { path: path.clone() }
                    }
                }),
            }),
        network: Some(match settings.network.mode {
            EnvironmentNetworkMode::Block => DaytonaNetwork::Block,
            EnvironmentNetworkMode::AllowAll => DaytonaNetwork::AllowAll,
            EnvironmentNetworkMode::CidrAllowList => {
                DaytonaNetwork::AllowList(settings.network.allow.clone())
            }
        }),
        skip_clone,
    }
}

#[cfg(feature = "docker")]
#[must_use]
pub fn docker_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DockerSandboxOptions {
    // No vault is available on this path (server preflight / manifest), so
    // resolve `{{ env.* }}` against the process environment and let every other
    // token (including `{{ secrets.* }}`) fall back to its source form.
    let env = settings
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.resolve_or_source(process_env_var)))
        .collect();
    docker_config_from_environment_env(settings, skip_clone, env)
}

#[cfg(feature = "docker")]
pub fn docker_config_from_environment_with_secrets(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
    secrets_lookup: impl FnMut(&str) -> Option<String>,
) -> Result<DockerSandboxOptions, ResolveError> {
    let env = settings.resolve_env(process_env_var, secrets_lookup)?;
    Ok(docker_config_from_environment_env(
        settings, skip_clone, env,
    ))
}

#[cfg(feature = "docker")]
fn docker_config_from_environment_env(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
    env: std::collections::HashMap<String, String>,
) -> DockerSandboxOptions {
    let mut env_vars = env
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    env_vars.sort();
    let default_options = DockerSandboxOptions::default();

    DockerSandboxOptions {
        image: settings
            .image
            .docker
            .clone()
            .unwrap_or(default_options.image),
        network_mode: match settings.network.mode {
            EnvironmentNetworkMode::Block => Some("none".to_string()),
            EnvironmentNetworkMode::AllowAll | EnvironmentNetworkMode::CidrAllowList => {
                default_options.network_mode
            }
        },
        memory_limit: settings
            .resources
            .memory
            .and_then(|size| i64::try_from(size.as_bytes()).ok()),
        cpu_quota: settings
            .resources
            .cpu
            .map(|cpu| i64::from(cpu).saturating_mul(100_000)),
        env_vars,
        skip_clone,
        ..DockerSandboxOptions::default()
    }
}

/// Build a [`ForkdConfig`] from the current run environment settings and
/// process environment variables.
///
/// - `FORKD_URL`   — controller base URL (default: `http://127.0.0.1:8889`)
/// - `FORKD_TOKEN` — bearer token       (default: `forkd-local-token`)
#[cfg(feature = "forkd")]
#[must_use]
pub fn forkd_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> ForkdConfig {
    #[expect(
        clippy::disallowed_methods,
        reason = "Forkd config resolves server-level credentials from the process environment."
    )]
    let forkd_url =
        std::env::var("FORKD_URL").unwrap_or_else(|_| "http://127.0.0.1:8889".to_string());
    #[expect(
        clippy::disallowed_methods,
        reason = "Forkd config resolves server-level credentials from the process environment."
    )]
    let forkd_token =
        std::env::var("FORKD_TOKEN").unwrap_or_else(|_| "forkd-local-token".to_string());

    #[allow(
        clippy::cast_possible_truncation,
        reason = "environment memory limits are configured well within u32 MiB range"
    )]
    let mem_mib = settings
        .resources
        .memory
        .map(|size| (size.as_bytes() / (1024 * 1024)) as u32);

    let snapshot = ForkdSnapshotSettings {
        image: settings.image.docker.clone(),
        kernel: None, // resolved from FORKD_KERNEL by ForkdConfig::effective_kernel()
        mem_mib,
        extra_packages: None,
    };

    let network = match settings.network.mode {
        EnvironmentNetworkMode::Block => ForkdNetwork::Block,
        EnvironmentNetworkMode::AllowAll => ForkdNetwork::AllowAll,
        EnvironmentNetworkMode::CidrAllowList => {
            ForkdNetwork::AllowList(settings.network.allow.clone())
        }
    };

    #[expect(
        clippy::disallowed_methods,
        reason = "Forkd config resolves snapshot tag from the process environment."
    )]
    let snapshot_tag =
        std::env::var("FORKD_SNAPSHOT_TAG").unwrap_or_else(|_| DEFAULT_SNAPSHOT_TAG.to_string());

    ForkdConfig {
        forkd_url,
        forkd_token,
        settings: ForkdSettings {
            snapshot_tag,
            snapshot: Some(snapshot),
            network: Some(network),
            skip_clone,
            auto_stop_minutes: settings
                .lifecycle
                .auto_stop
                .map(|d| duration_to_minutes_i32(d.as_std())),
        },
    }
}

pub fn local_working_directory_from_environment(
    settings: &RunEnvironmentSettings,
    source_directory: Option<&Path>,
) -> crate::Result<PathBuf> {
    if let Some(cwd) = settings.cwd.as_deref() {
        return Ok(PathBuf::from(cwd));
    }

    let Some(source_directory) = source_directory else {
        return Err(crate::Error::message(
            "local environment requires a server-side working directory; configure `environment.cwd = \"/absolute/path\"` on the selected local environment",
        ));
    };

    if source_directory.is_dir() {
        return Ok(source_directory.to_path_buf());
    }

    Err(crate::Error::message(format!(
        "local environment source_directory does not exist or is not a directory on this server: {}. Configure `environment.cwd = \"/absolute/path\"` on the selected local environment for remote client/server deployments.",
        source_directory.display()
    )))
}

#[cfg(feature = "docker")]
#[expect(
    clippy::disallowed_methods,
    reason = "Environment interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(any(feature = "daytona", feature = "forkd"))]
fn duration_to_minutes_i32(duration: std::time::Duration) -> i32 {
    let minutes = duration.as_secs() / 60;
    i32::try_from(minutes).unwrap_or(i32::MAX)
}

#[cfg(feature = "daytona")]
fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use fabro_types::settings::run::{
        EnvironmentImageSettings, EnvironmentLifecycleSettings, EnvironmentNetworkSettings,
        EnvironmentProvider, EnvironmentResourcesSettings,
    };

    use super::*;

    fn run_environment(provider: EnvironmentProvider) -> RunEnvironmentSettings {
        RunEnvironmentSettings {
            id: "host".to_string(),
            provider,
            cwd: None,
            image: EnvironmentImageSettings::default(),
            resources: EnvironmentResourcesSettings::default(),
            network: EnvironmentNetworkSettings::default(),
            lifecycle: EnvironmentLifecycleSettings::default(),
            labels: HashMap::new(),
            env: HashMap::new(),
        }
    }

    #[test]
    fn local_working_directory_prefers_environment_cwd() {
        let mut settings = run_environment(EnvironmentProvider::Local);
        settings.cwd = Some("/srv/fabro/workspaces/team-a".to_string());
        let missing_source = Path::new("/path/that/should/not/exist");

        let resolved = local_working_directory_from_environment(&settings, Some(missing_source))
            .expect("configured cwd should be accepted");

        assert_eq!(resolved, PathBuf::from("/srv/fabro/workspaces/team-a"));
        assert!(!missing_source.exists());
    }

    #[test]
    fn local_working_directory_uses_existing_source_directory_without_cwd() {
        let settings = run_environment(EnvironmentProvider::Local);
        let dir = tempfile::tempdir().unwrap();

        let resolved = local_working_directory_from_environment(&settings, Some(dir.path()))
            .expect("existing source directory should be accepted");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn local_working_directory_rejects_missing_source_directory_without_cwd() {
        let settings = run_environment(EnvironmentProvider::Local);
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("client-only");

        let err = local_working_directory_from_environment(&settings, Some(&missing))
            .expect_err("missing source directory without cwd should fail");

        let message = err.to_string();
        assert!(
            message.contains("environment.cwd") && message.contains("does not exist"),
            "unexpected error: {message}"
        );
        assert!(!missing.exists());
    }
}
