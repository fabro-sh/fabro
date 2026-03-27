mod install;

use anyhow::Result;

use crate::args::{SkillCommand, SkillNamespace};

pub use install::install_skill_to;

pub fn dispatch(ns: SkillNamespace) -> Result<()> {
    match ns.command {
        SkillCommand::Install(args) => install::run_skill_install(&args),
    }
}
