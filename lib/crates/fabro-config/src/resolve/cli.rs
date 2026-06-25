use fabro_types::settings::cli::{
    CliAuthSettings, CliExecAgentSettings, CliExecModelSettings, CliExecSettings,
    CliLoggingSettings, CliNamespace, CliOutputSettings, CliTargetSettings, CliUpdatesSettings,
};

use super::{ResolveError, require_string};
use crate::{CliExecLayer, CliLayer, CliTargetLayer};

pub fn resolve_cli(layer: &CliLayer, errors: &mut Vec<ResolveError>) -> CliNamespace {
    CliNamespace {
        target:  resolve_target(layer.target.as_ref(), errors),
        auth:    CliAuthSettings {
            strategy: layer.auth.as_ref().and_then(|auth| auth.strategy),
        },
        exec:    resolve_exec(layer.exec.as_ref()),
        output:  CliOutputSettings {
            format:    layer
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
        Some(CliTargetLayer::Http { url }) => {
            super::warn_if_demoted_template("cli.target.url", url.as_deref());
            Some(CliTargetSettings::Http {
                url: require_string(url.as_ref(), "cli.target.url", errors),
            })
        }
        Some(CliTargetLayer::Unix { path }) => {
            super::warn_if_demoted_template("cli.target.path", path.as_deref());
            Some(CliTargetSettings::Unix {
                path: require_string(path.as_ref(), "cli.target.path", errors),
            })
        }
        None => None,
    }
}

fn resolve_exec(exec: Option<&CliExecLayer>) -> CliExecSettings {
    let exec = exec.expect("defaults.toml should provide cli.exec defaults");

    let model = exec.model.as_ref();
    super::warn_if_demoted_template(
        "cli.exec.model.provider",
        model.and_then(|model| model.provider.as_deref()),
    );
    super::warn_if_demoted_template(
        "cli.exec.model.name",
        model.and_then(|model| model.name.as_deref()),
    );

    CliExecSettings {
        prevent_idle_sleep: exec
            .prevent_idle_sleep
            .expect("defaults.toml should provide cli.exec.prevent_idle_sleep"),
        model:              CliExecModelSettings {
            provider: model.and_then(|model| model.provider.clone()),
            name:     model.and_then(|model| model.name.clone()),
        },
        agent:              CliExecAgentSettings {
            permissions: exec.agent.as_ref().and_then(|agent| agent.permissions),
            mcps:        exec.agent.as_ref().and_then(|agent| {
                (!agent.mcps.is_empty()).then(|| super::run::resolve_enabled_mcps(&agent.mcps))
            }),
        },
    }
}
