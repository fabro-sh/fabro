pub(crate) mod foreground;
pub(crate) mod start;
pub(crate) mod status;
pub(crate) mod stop;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fabro_client::{AuthEntry, AuthStore, DevTokenEntry, ServerTarget};
use fabro_config::bind::{self, Bind, BindRequest};
use fabro_config::user::{active_settings_path, default_storage_dir};
use fabro_server::install::{self, InstallAppState, InstallFinishHook, InstallFinishInfo};
use fabro_server::serve::{self, ServeArgs};
use fabro_static::EnvVars;
use fabro_util::browser;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use ring::rand::{SecureRandom, SystemRandom};
use tracing::info;

use crate::args::{
    GlobalArgs, ServerCommand, ServerRestartArgs, ServerServeArgs, ServerStartArgs,
    ServerStatusArgs, ServerStopArgs,
};
use crate::{local_server, user_config};

pub(crate) async fn dispatch(
    command: ServerCommand,
    _globals: &GlobalArgs,
    foreground_log_bootstrap: Option<start::ForegroundServerLogBootstrap>,
    printer: Printer,
) -> Result<()> {
    match command {
        ServerCommand::Start(ServerStartArgs {
            storage_dir,
            foreground,
            serve_args,
        }) => {
            if let Some(bootstrap) = maybe_install_bootstrap(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
                &serve_args,
            )? {
                if serve_args.no_web {
                    fabro_util::printerr!(
                        printer,
                        "Warning: --no-web is ignored during install; will be respected on next start."
                    );
                }
                return run_install_mode(bootstrap, printer).await;
            }

            let local_config = local_server::LocalServerConfig::load(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
            )?;
            let storage_dir = local_config.storage_dir().to_path_buf();
            let bind_addr = local_config.bind_request(serve_args.bind.as_deref())?;
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            Box::pin(start::execute(
                bind_addr,
                foreground,
                serve_args,
                storage_dir,
                foreground_log_bootstrap,
                styles,
                printer,
            ))
            .await
        }
        ServerCommand::Stop(ServerStopArgs {
            storage_dir,
            timeout,
        }) => {
            let local_config =
                local_server::LocalServerConfig::load_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = local_config.storage_dir().to_path_buf();
            stop::execute(&storage_dir, Duration::from_secs(timeout), printer).await
        }
        ServerCommand::Restart(ServerRestartArgs {
            storage_dir,
            timeout,
            foreground,
            serve_args,
        }) => {
            if let Some(bootstrap) = maybe_install_bootstrap(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
                &serve_args,
            )? {
                stop::stop_server(&bootstrap.storage_dir, Duration::from_secs(timeout)).await?;
                if serve_args.no_web {
                    fabro_util::printerr!(
                        printer,
                        "Warning: --no-web is ignored during install; will be respected on next start."
                    );
                }
                return run_install_mode(bootstrap, printer).await;
            }

            let local_config = local_server::LocalServerConfig::load(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
            )?;
            let storage_dir = local_config.storage_dir().to_path_buf();
            stop::stop_server(&storage_dir, Duration::from_secs(timeout)).await?;
            let bind_addr = local_config.bind_request(serve_args.bind.as_deref())?;
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            Box::pin(start::execute(
                bind_addr,
                foreground,
                serve_args,
                storage_dir,
                foreground_log_bootstrap,
                styles,
                printer,
            ))
            .await
        }
        ServerCommand::Status(ServerStatusArgs { storage_dir, json }) => {
            let local_config =
                local_server::LocalServerConfig::load_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = local_config.storage_dir().to_path_buf();
            status::execute(&storage_dir, json, printer)
        }
        ServerCommand::Serve(ServerServeArgs {
            storage_dir,
            serve_args,
        }) => {
            let local_config = local_server::LocalServerConfig::load(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
            )?;
            let active_config_path = Some(
                serve_args
                    .config
                    .clone()
                    .unwrap_or_else(|| user_config::active_settings_path(None)),
            );
            let storage_dir = local_config.storage_dir().to_path_buf();
            let bind_addr = local_config.bind_request(serve_args.bind.as_deref())?;
            let _ = printer;
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            Box::pin(foreground::serve_with_daemon_record(
                ServeArgs {
                    config: active_config_path,
                    ..serve_args
                },
                bind_addr,
                storage_dir,
                styles,
            ))
            .await
        }
    }
}

struct InstallBootstrap {
    bind_request: BindRequest,
    storage_dir:  std::path::PathBuf,
    config_path:  std::path::PathBuf,
    token:        String,
}

fn maybe_install_bootstrap(
    explicit_config: Option<&std::path::Path>,
    storage_dir: Option<&std::path::Path>,
    serve_args: &ServeArgs,
) -> Result<Option<InstallBootstrap>> {
    if explicit_config.is_some() || has_config_env_override() {
        return Ok(None);
    }

    let config_path = active_settings_path(None);
    if config_path.exists() {
        return Ok(None);
    }

    let bind_request = match serve_args.bind.as_deref() {
        Some(bind) => bind::parse_bind(bind)?,
        None => default_install_bind_request(),
    };

    let storage_dir = storage_dir.map_or_else(default_storage_dir, std::path::Path::to_path_buf);

    Ok(Some(InstallBootstrap {
        bind_request,
        storage_dir,
        config_path,
        token: generate_install_token()?,
    }))
}

#[expect(
    clippy::disallowed_methods,
    reason = "Install bootstrap checks whether the documented FABRO_CONFIG override is set."
)]
fn has_config_env_override() -> bool {
    std::env::var_os(EnvVars::FABRO_CONFIG).is_some()
}

async fn run_install_mode(bootstrap: InstallBootstrap, printer: Printer) -> Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let token = bootstrap.token.clone();
    let state = InstallAppState::new(
        bootstrap.token,
        &bootstrap.storage_dir,
        &bootstrap.config_path,
    )
    .with_finish_hook(persist_install_dev_token_hook());
    install::serve_install_command(bootstrap.bind_request, state, move |bind| {
        announce_install_mode(bind, &token, styles, printer);
        Ok(())
    })
    .await
}

fn persist_install_dev_token_hook() -> InstallFinishHook {
    Arc::new(|info: &InstallFinishInfo| {
        let Some(token) = &info.dev_token else {
            return Ok(());
        };
        let target = ServerTarget::http_url(&info.canonical_url)?;
        AuthStore::default().put(
            &target,
            AuthEntry::DevToken(DevTokenEntry {
                token:        token.clone(),
                logged_in_at: chrono::Utc::now(),
            }),
        )?;
        Ok(())
    })
}

fn announce_install_mode(bind: &Bind, token: &str, styles: &Styles, printer: Printer) {
    info!(
        bind = %bind,
        install_url = install_url_hint(bind, "<redacted>").as_deref().unwrap_or("<unavailable>"),
        "entering install mode"
    );
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "  {} Fabro server is unconfigured — install mode active.",
        styles.bold.apply_to("⚒️")
    );
    fabro_util::printerr!(printer, "");
    match install_url_hint(bind, token) {
        Some(url) => {
            fabro_util::printerr!(printer, "  Open this URL in your browser to finish setup:");
            fabro_util::printerr!(printer, "    {}", styles.cyan.apply_to(&url));
            if let Err(e) = browser::try_open(&url) {
                fabro_util::printerr!(printer, "");
                fabro_util::printerr!(printer, "  Could not open a browser automatically: {e}");
                fabro_util::printerr!(printer, "  Open the URL above manually to continue.");
            }
        }
        None => {
            fabro_util::printerr!(
                printer,
                "  Open the server root through your configured reverse proxy to finish setup."
            );
        }
    }
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(printer, "  If prompted, paste the install token:");
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(printer, "    {}", styles.bold_cyan.apply_to(token));
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "{}",
        install_mode_next_step_message(running_in_container())
    );
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "  Or visit the root path for the install token instructions."
    );
    fabro_util::printerr!(printer, "");
}

fn install_mode_next_step_message(supervised: bool) -> &'static str {
    if supervised {
        "  After install, the server should restart automatically."
    } else {
        "  After install, you'll be prompted to re-run `fabro server start`."
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Install-mode URL hints honor Railway's documented public-domain env var."
)]
fn install_url_hint(bind: &Bind, token: &str) -> Option<String> {
    if let Some(domain) = std::env::var(EnvVars::RAILWAY_PUBLIC_DOMAIN)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Some(format!("https://{domain}/install?token={token}"));
    }

    match bind {
        Bind::Tcp(addr) => Some(format!("http://{addr}/install?token={token}")),
        Bind::Unix(_) => None,
    }
}

fn default_install_bind_request() -> BindRequest {
    if running_in_container() {
        BindRequest::Tcp(std::net::SocketAddr::from((
            [0, 0, 0, 0],
            serve::DEFAULT_TCP_PORT,
        )))
    } else {
        BindRequest::Tcp(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            serve::DEFAULT_TCP_PORT,
        )))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Install-mode bind defaults inspect known container platform env markers."
)]
fn running_in_container() -> bool {
    std::env::var_os(EnvVars::RAILWAY_PUBLIC_DOMAIN).is_some()
        || std::env::var_os(EnvVars::RAILWAY_ENVIRONMENT).is_some()
        || std::env::var_os(EnvVars::KUBERNETES_SERVICE_HOST).is_some()
        || std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
}

fn generate_install_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("failed to generate install token"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::install_mode_next_step_message;

    #[test]
    fn install_mode_next_step_message_recommends_manual_restart_locally() {
        assert_eq!(
            install_mode_next_step_message(false),
            "  After install, you'll be prompted to re-run `fabro server start`."
        );
    }

    #[test]
    fn install_mode_next_step_message_mentions_automatic_restart_in_supervised_envs() {
        assert_eq!(
            install_mode_next_step_message(true),
            "  After install, the server should restart automatically."
        );
    }
}
