use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::{PlannedCommand, plugins, spa_refresh};

#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    /// Arguments forwarded to `cargo build`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cargo_args: Vec<String>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev tooling reads Cargo's configured target"
)]
pub(crate) fn build(args: BuildArgs) -> Result<()> {
    let root = super::workspace_root();
    spa_refresh::spa_refresh_root(&root)?;

    let target =
        argument(&args.cargo_args, "--target").or_else(|| std::env::var("CARGO_BUILD_TARGET").ok());
    let directory = plugins::prepare(&root, target.as_deref(), false)?;
    let output_profile = output_profile(&args.cargo_args);
    let default_root = directory
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .context("plugin staging directory has no Cargo target root")?;
    let mut output_root = argument(&args.cargo_args, "--target-dir")
        .map(PathBuf::from)
        .map_or_else(|| default_root.to_path_buf(), |path| root.join(path));
    if let Some(target) = &target {
        output_root.push(target);
    }
    let output = output_root.join(output_profile);
    let mut command = PlannedCommand::new("cargo").arg("build");
    for arg in args.cargo_args {
        command = command.arg(arg);
    }

    super::run_command(&root, &plugins::configure(command, &directory))?;
    plugins::stage(&directory, &output)
}

fn argument(args: &[String], name: &str) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        if arg == name {
            args.get(index + 1).cloned()
        } else {
            arg.strip_prefix(&format!("{name}=")).map(str::to_owned)
        }
    })
}

fn output_profile(args: &[String]) -> String {
    let profile = argument(args, "--profile").unwrap_or_else(|| {
        if args.iter().any(|arg| arg == "--release" || arg == "-r") {
            "release".to_owned()
        } else {
            "dev".to_owned()
        }
    });
    match profile.as_str() {
        "dev" | "test" => "debug".to_owned(),
        "release" | "bench" => "release".to_owned(),
        _ => profile,
    }
}

#[cfg(test)]
mod tests {
    use super::output_profile;

    #[test]
    fn forwarded_cargo_profiles_use_cargos_output_directories() {
        for (args, directory) in [
            (vec![], "debug"),
            (vec!["--release"], "release"),
            (vec!["-r"], "release"),
            (vec!["--profile", "dev"], "debug"),
            (vec!["--profile", "test"], "debug"),
            (vec!["--profile=bench"], "release"),
            (vec!["--profile=release"], "release"),
            (vec!["--profile", "custom"], "custom"),
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(output_profile(&args), directory, "{args:?}");
        }
    }
}
