use fabro_types::settings::cli::{
    CliAuthSettings, CliExecAgentSettings, CliExecLayer, CliExecModelSettings, CliExecSettings,
    CliLayer, CliLoggingSettings, CliOutputSettings, CliSettings, CliTargetLayer,
    CliTargetSettings, CliTargetTlsSettings, CliUpdatesSettings,
};

use super::{ResolveError, require_interp};

pub fn resolve_cli(layer: &CliLayer, errors: &mut Vec<ResolveError>) -> CliSettings {
    CliSettings {
        target: resolve_target(layer.target.as_ref(), errors),
        auth: CliAuthSettings {
            strategy: layer.auth.as_ref().and_then(|auth| auth.strategy),
        },
        exec: resolve_exec(layer.exec.as_ref()),
        output: CliOutputSettings {
            format: layer
                .output
                .as_ref()
                .and_then(|output| output.format)
                .expect("defaults.toml should provide cli.output.format"),
            verbosity: layer
                .output
                .as_ref()
                .and_then(|output| output.verbosity)
                .expect("defaults.toml should provide cli.output.verbosity"),
        },
        updates: CliUpdatesSettings {
            check: layer
                .updates
                .as_ref()
                .and_then(|updates| updates.check)
                .expect("defaults.toml should provide cli.updates.check"),
        },
        logging: CliLoggingSettings {
            level: layer
                .logging
                .as_ref()
                .and_then(|logging| logging.level.clone()),
        },
    }
}

fn resolve_target(
    target: Option<&CliTargetLayer>,
    errors: &mut Vec<ResolveError>,
) -> Option<CliTargetSettings> {
    match target {
        Some(CliTargetLayer::Http { url, tls }) => Some(CliTargetSettings::Http {
            url: require_interp(url.as_ref(), "cli.target.url", errors),
            tls: tls.as_ref().map(|tls| CliTargetTlsSettings {
                cert: require_interp(tls.cert.as_ref(), "cli.target.tls.cert", errors),
                key: require_interp(tls.key.as_ref(), "cli.target.tls.key", errors),
                ca: require_interp(tls.ca.as_ref(), "cli.target.tls.ca", errors),
            }),
        }),
        Some(CliTargetLayer::Unix { path }) => Some(CliTargetSettings::Unix {
            path: require_interp(path.as_ref(), "cli.target.path", errors),
        }),
        None => None,
    }
}

fn resolve_exec(exec: Option<&CliExecLayer>) -> CliExecSettings {
    let exec = exec.expect("defaults.toml should provide cli.exec defaults");

    CliExecSettings {
        prevent_idle_sleep: exec
            .prevent_idle_sleep
            .expect("defaults.toml should provide cli.exec.prevent_idle_sleep"),
        model: CliExecModelSettings {
            provider: exec.model.as_ref().and_then(|model| model.provider.clone()),
            name: exec.model.as_ref().and_then(|model| model.name.clone()),
        },
        agent: CliExecAgentSettings {
            permissions: exec.agent.as_ref().and_then(|agent| agent.permissions),
            mcps: exec
                .agent
                .as_ref()
                .map(|agent| {
                    agent
                        .mcps
                        .iter()
                        .map(|(name, entry)| {
                            (
                                name.clone(),
                                super::run::resolve_mcp_entry(name.as_str(), entry),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
    }
}
