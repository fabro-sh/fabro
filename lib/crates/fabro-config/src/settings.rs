use fabro_types::Settings;

use crate::config::ConfigLayer;

impl TryFrom<ConfigLayer> for Settings {
    type Error = anyhow::Error;

    fn try_from(value: ConfigLayer) -> Result<Self, Self::Error> {
        Ok(Self {
            version: value.version,
            goal: value.goal,
            goal_file: value.goal_file,
            graph: value.graph,
            labels: value.labels,
            work_dir: value.work_dir,
            llm: value.llm.map(Into::into),
            setup: value.setup.map(Into::into),
            sandbox: value.sandbox.map(TryInto::try_into).transpose()?,
            vars: value.vars,
            checkpoint: value.checkpoint.into(),
            pull_request: value.pull_request.map(Into::into),
            artifacts: value.artifacts.map(Into::into),
            hooks: value.hooks,
            mcp_servers: value.mcp_servers,
            github: value.github.map(Into::into),
            mode: value.mode,
            server: value.server.map(TryInto::try_into).transpose()?,
            exec: value.exec.map(Into::into),
            prevent_idle_sleep: value.prevent_idle_sleep,
            verbose: value.verbose,
            upgrade_check: value.upgrade_check,
            dry_run: value.dry_run,
            auto_approve: value.auto_approve,
            no_retro: value.no_retro,
            storage_dir: value.storage_dir,
            max_concurrent_runs: value.max_concurrent_runs,
            web: value.web.map(Into::into),
            api: value.api.map(TryInto::try_into).transpose()?,
            features: value.features.map(Into::into),
            log: value.log.map(Into::into),
            git: value.git.map(TryInto::try_into).transpose()?,
            fabro: value.fabro.map(Into::into),
        })
    }
}

impl TryFrom<&ConfigLayer> for Settings {
    type Error = anyhow::Error;

    fn try_from(value: &ConfigLayer) -> Result<Self, Self::Error> {
        value.clone().try_into()
    }
}
