//! Build the exact sandbox-driver executables whose hashes Petri embeds.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use fabro_static::{EnvVars, SANDBOX_PLUGIN_BINARIES, SANDBOX_PLUGINS};
use serde::Deserialize;

use super::PlannedCommand;

/// Where `prepare` left the pinned plugin executables.
pub(crate) struct PreparedPlugins {
    /// Cargo's target directory for this workspace.
    pub(crate) target_directory: PathBuf,
    /// The staged plugin executables:
    /// `<target_directory>/<target>/plugins/bin`.
    pub(crate) directory:        PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PluginsArgs {
    /// Rust target triple (defaults to the active toolchain's host).
    #[arg(long)]
    target:   Option<String>,
    /// Use cargo-zigbuild for the plugin binaries (musl release targets).
    #[arg(long)]
    zigbuild: bool,
}

#[derive(Deserialize)]
struct Metadata {
    packages:         Vec<Package>,
    target_directory: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    name:   String,
    source: Option<String>,
}

#[expect(
    clippy::print_stdout,
    reason = "dev command prints the completed plugin directory"
)]
pub(crate) fn plugins(args: &PluginsArgs) -> Result<()> {
    let prepared = prepare(
        &super::workspace_root(),
        args.target.as_deref(),
        args.zigbuild,
    )?;
    println!(
        "Verified-plugin build inputs: {}",
        prepared.directory.display()
    );
    Ok(())
}

/// Build first, then copy the exact bytes to an explicit input directory.
/// The caller passes this directory to Cargo and keeps it unchanged until
/// the compiled Fabro and its bundle have both been produced. Without an
/// explicit target, `CARGO_BUILD_TARGET` and then the host toolchain decide,
/// as they do for the Fabro build itself.
#[expect(
    clippy::disallowed_methods,
    reason = "dev tooling reads Cargo's configured target"
)]
pub(crate) fn prepare(
    root: &Path,
    target: Option<&str>,
    zigbuild: bool,
) -> Result<PreparedPlugins> {
    let target = match target {
        Some(target) => target.to_owned(),
        None => std::env::var("CARGO_BUILD_TARGET")
            .ok()
            .map_or_else(host_target, Ok)?,
    };
    let output = super::capture_command(
        root,
        &PlannedCommand::new("cargo")
            .arg("metadata")
            .arg("--locked")
            .arg("--format-version")
            .arg("1"),
    )?;
    if !output.status.success() {
        bail!(
            "resolving pinned sandbox-driver packages failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .context("reading Cargo metadata for the sandbox plugins")?;
    validate_pins(&metadata)?;
    let build_dir = metadata.target_directory.join("sandbox-plugins-build");
    let directory = metadata.target_directory.join(&target).join("plugins/bin");
    let mut build = PlannedCommand::new("cargo")
        .arg(if zigbuild { "zigbuild" } else { "build" })
        .arg("--locked")
        .arg("--release")
        .arg("--target")
        .arg(&target)
        .arg("--target-dir")
        .arg(build_dir.to_string_lossy())
        // Resolve the same dependency features as Fabro, including vendored
        // OpenSSL on musl. The explicit --bin selectors below build only the
        // plugins, before Petri's build script needs their completed bytes.
        .arg("-p")
        .arg("fabro-cli");
    for binary in SANDBOX_PLUGIN_BINARIES {
        build = build.arg("-p").arg(binary).arg("--bin").arg(binary);
    }
    super::run_command(root, &build)?;
    stage(&build_dir.join(&target).join("release"), &directory)?;
    Ok(PreparedPlugins {
        target_directory: metadata.target_directory,
        directory,
    })
}

fn validate_pins(metadata: &Metadata) -> Result<()> {
    let mut pinned: Option<&str> = None;
    for binary in SANDBOX_PLUGIN_BINARIES {
        let mut matches = metadata
            .packages
            .iter()
            .filter(|package| package.name == binary);
        let (Some(package), None) = (matches.next(), matches.next()) else {
            bail!("expected exactly one pinned {binary} package");
        };
        let source = package
            .source
            .as_deref()
            .filter(|source| source.starts_with("git+") && source.contains("?rev="))
            .with_context(|| format!("{binary} must come from a Git dependency pinned by rev"))?;
        if *pinned.get_or_insert(source) != source {
            bail!("sandbox plugins must all use the same pinned Git revision");
        }
    }
    Ok(())
}

pub(crate) fn host_target() -> Result<String> {
    let output = super::capture_command(Path::new("."), &PlannedCommand::new("rustc").arg("-vV"))?;
    if !output.status.success() {
        bail!("rustc -vV failed while detecting the plugin target");
    }
    String::from_utf8(output.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .context("rustc -vV did not report a host target")
}

/// Compile-time pins and test-time discovery must refer to the same bytes.
/// Do not use runtime SHA overrides: the release tests exercise embedded pins.
pub(crate) fn configure(mut command: PlannedCommand, directory: &Path) -> PlannedCommand {
    command = command
        .env(
            EnvVars::PETRI_SANDBOX_PLUGIN_DIR,
            directory.to_string_lossy(),
        )
        .env(EnvVars::PETRI_SANDBOX_PLUGIN_DEV, "0")
        .env(EnvVars::FABRO_REQUIRE_SANDBOX_PLUGINS, "1");
    for plugin in SANDBOX_PLUGINS {
        command = command.env_remove(plugin.sha256_var).env(
            plugin.path_var,
            directory.join(plugin.binary).to_string_lossy(),
        );
    }
    command
}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous dev tooling stages release executables"
)]
pub(crate) fn stage(source: &Path, destination: &Path) -> Result<()> {
    // Validate the entire input before modifying the destination.
    for binary in SANDBOX_PLUGIN_BINARIES {
        let path = source.join(binary);
        if !path.is_file() {
            bail!("missing sandbox plugin {}", path.display());
        }
    }
    std::fs::create_dir_all(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for binary in SANDBOX_PLUGIN_BINARIES {
        std::fs::copy(source.join(binary), destination.join(binary))
            .with_context(|| format!("staging {binary} in {}", destination.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests create disposable executable fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn mixed_plugin_revisions_are_rejected() {
        let mut metadata = Metadata {
            target_directory: PathBuf::from("target"),
            packages:         SANDBOX_PLUGIN_BINARIES
                .iter()
                .map(|name| Package {
                    name:   (*name).to_owned(),
                    source: Some("git+https://example.com/driver?rev=abc#abc".to_owned()),
                })
                .collect(),
        };
        assert!(validate_pins(&metadata).is_ok());
        metadata.packages[2].source = Some("git+https://example.com/driver?rev=def#def".to_owned());
        assert!(validate_pins(&metadata).is_err());
    }

    #[test]
    fn incomplete_build_does_not_modify_existing_bundle() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("bundle");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join(SANDBOX_PLUGIN_BINARIES[0]), "new").unwrap();
        std::fs::write(destination.join(SANDBOX_PLUGIN_BINARIES[0]), "old").unwrap();
        assert!(stage(&source, &destination).is_err());
        assert_eq!(
            std::fs::read(destination.join(SANDBOX_PLUGIN_BINARIES[0])).unwrap(),
            b"old"
        );
    }
}
