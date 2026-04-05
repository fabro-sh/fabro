pub(crate) mod deinit;
pub(crate) mod init;

use anyhow::Result;

use crate::args::{GlobalArgs, RepoCommand, RepoNamespace};
use crate::shared::print_json_pretty;

pub(crate) async fn dispatch(ns: RepoNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        RepoCommand::Init(args) => {
            let created = init::run_init(&args, globals).await?;
            if args.skill {
                let base = std::env::current_dir()?.join(".claude").join("skills");
                super::skill::install_skill_to(&base)?;
            }
            if globals.json {
                print_json_pretty(&serde_json::json!({ "created": created }))?;
            }
            Ok(())
        }
        RepoCommand::Deinit => {
            let removed = deinit::run_deinit(globals)?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "removed": removed }))?;
            }
            Ok(())
        }
    }
}
