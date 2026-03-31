mod install;

use anyhow::Result;

use crate::args::{GlobalArgs, SkillCommand, SkillNamespace};

pub(crate) use install::install_skill_to;

pub(crate) fn dispatch(ns: SkillNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        SkillCommand::Install(args) => install::run_skill_install(&args, globals),
    }
}
