pub(crate) mod deinit;
pub(crate) mod init;

use anyhow::Result;

use crate::args::{RepoCommand, RepoNamespace};

pub(crate) async fn dispatch(ns: RepoNamespace) -> Result<()> {
    match ns.command {
        RepoCommand::Init { skill } => {
            init::run_init().await?;
            if skill {
                let base = std::env::current_dir()?.join(".claude").join("skills");
                super::skill::install_skill_to(&base)?;
            }
            Ok(())
        }
        RepoCommand::Deinit => deinit::run_deinit(),
    }
}
