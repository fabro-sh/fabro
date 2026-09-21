//! Install immutable executable bundles and atomically switch the launcher.
//! Retained bundles keep a running server's plugin fingerprints valid.

#![expect(
    clippy::disallowed_methods,
    reason = "the synchronous upgrade installation stages and activates executable files"
)]

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const VERSIONS_DIR: &str = ".fabro-versions";
pub(super) const EXECUTABLES: [&str; 4] = [
    "fabro",
    "sandbox-driver-host",
    "sandbox-driver-docker",
    "sandbox-driver-daytona",
];

/// macOS can report the launcher symlink from `current_exe`. Enter its actual
/// bundle before starting threads, so this process and all later workers keep
/// discovering plugins beside this version even after the launcher changes.
#[cfg(unix)]
pub(crate) fn enter_managed_bundle() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let executable = std::env::current_exe()?;
    let canonical = executable.canonicalize()?;
    let managed = canonical
        .parent()
        .and_then(Path::parent)
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == VERSIONS_DIR));
    if managed && canonical != executable {
        return Err(Command::new(canonical)
            .args(std::env::args_os().skip(1))
            .exec());
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn enter_managed_bundle() -> std::io::Result<()> {
    Ok(())
}

/// Locate the public launcher from either a managed bundle or a flat install.
pub(super) fn launcher(current_exe: &Path) -> Result<PathBuf> {
    let parent = current_exe
        .parent()
        .context("Fabro executable has no parent directory")?;
    if let Some(versions) = parent
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == VERSIONS_DIR))
    {
        let directory = versions
            .parent()
            .context("bundle directory has no installation root")?;
        let launcher = directory.join("fabro");
        if launcher
            .canonicalize()
            .context("resolving the active Fabro bundle")?
            != current_exe
        {
            bail!(
                "this Fabro bundle is no longer active; retry upgrade using {}",
                launcher.display()
            );
        }
        Ok(launcher)
    } else {
        Ok(current_exe.to_path_buf())
    }
}

#[cfg(unix)]
pub(super) fn install(source: &Path, launcher: &Path) -> Result<()> {
    use std::os::unix::fs::{self as unix_fs, PermissionsExt};

    for name in EXECUTABLES {
        let path = source.join(name);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("release bundle is missing {name}; install a release containing the complete sandbox plugin bundle"))?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
            bail!(
                "release bundle entry {} must be a regular executable file",
                path.display()
            );
        }
    }
    let directory = launcher
        .parent()
        .context("Fabro launcher has no parent directory")?;
    let versions = directory.join(VERSIONS_DIR);
    fs::create_dir_all(&versions).with_context(|| format!("creating {}", versions.display()))?;
    let staged = tempfile::Builder::new()
        .prefix("bundle-")
        .tempdir_in(&versions)
        .context("staging the new Fabro bundle")?;
    for name in EXECUTABLES {
        fs::copy(source.join(name), staged.path().join(name))
            .with_context(|| format!("staging {name}"))?;
    }
    // TempDir starts private. A system-wide install must remain executable by
    // other users after activation, just like its public launcher directory.
    fs::set_permissions(staged.path(), fs::Permissions::from_mode(0o755))
        .context("setting bundle directory permissions")?;
    // A flat installation has no retained version yet. Preserve its old
    // executable before activation; existing plugin siblings remain untouched.
    if fs::symlink_metadata(launcher).is_ok_and(|metadata| metadata.file_type().is_file()) {
        let previous = tempfile::Builder::new()
            .prefix("previous-")
            .tempdir_in(&versions)
            .context("creating the previous executable backup")?;
        fs::hard_link(launcher, previous.path().join("fabro"))
            .context("preserving the previous Fabro executable")?;
        let _ = previous.keep();
    }
    let link_directory = tempfile::Builder::new()
        .prefix(".fabro-activate-")
        .tempdir_in(directory)
        .context("staging the Fabro launcher")?;
    let link = link_directory.path().join("fabro");
    let relative = Path::new(VERSIONS_DIR)
        .join(
            staged
                .path()
                .file_name()
                .context("staged bundle has no name")?,
        )
        .join("fabro");
    unix_fs::symlink(relative, &link).context("creating the new Fabro launcher")?;
    // Keep before activation: an interruption must never leave a launcher
    // referring to a directory a TempDir destructor subsequently removes.
    let retained = staged.keep();
    if let Err(error) = fs::rename(&link, launcher) {
        let _ = fs::remove_dir_all(&retained);
        return Err(error)
            .context("activating the new Fabro bundle; the previous installation is unchanged");
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn install(_source: &Path, _launcher: &Path) -> Result<()> {
    bail!("Fabro bundle installation requires a supported Unix platform")
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{self as unix_fs, PermissionsExt};

    use super::*;

    fn fixture(path: &Path, content: &[u8]) {
        fs::create_dir_all(path).expect("create bundle fixture directory");
        for name in EXECUTABLES {
            fs::write(path.join(name), content).expect("write bundle fixture executable");
            fs::set_permissions(path.join(name), fs::Permissions::from_mode(0o755))
                .expect("make bundle fixture executable");
        }
    }

    #[test]
    fn successive_upgrades_retain_matching_executables_for_running_servers() {
        let root = tempfile::tempdir().unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let source = canonical_root.join("download");
        let destination = canonical_root.join("bin");
        fixture(&destination, b"old");
        fixture(&source, b"first");
        let entry = destination.join("fabro");
        install(&source, &entry).unwrap();
        let first_exe = entry.canonicalize().unwrap();
        assert_eq!(
            fs::metadata(first_exe.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(launcher(&first_exe).unwrap(), entry);
        fixture(&source, b"second");
        install(&source, &entry).unwrap();
        let second_exe = entry.canonicalize().unwrap();
        assert_ne!(first_exe, second_exe);
        for name in EXECUTABLES {
            assert_eq!(
                fs::read(first_exe.parent().unwrap().join(name)).unwrap(),
                b"first"
            );
            assert_eq!(
                fs::read(second_exe.parent().unwrap().join(name)).unwrap(),
                b"second"
            );
        }
        assert!(launcher(&first_exe).is_err());
        assert_eq!(launcher(&second_exe).unwrap(), entry);
    }

    #[test]
    fn missing_or_symlinked_plugin_leaves_old_installation_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("download");
        let destination = root.path().join("bin");
        fixture(&source, b"new");
        fixture(&destination, b"old");
        let plugin = source.join("sandbox-driver-daytona");
        fs::remove_file(&plugin).unwrap();
        assert!(install(&source, &destination.join("fabro")).is_err());
        unix_fs::symlink(source.join("fabro"), plugin).unwrap();
        assert!(install(&source, &destination.join("fabro")).is_err());
        for name in EXECUTABLES {
            assert_eq!(fs::read(destination.join(name)).unwrap(), b"old");
        }
        assert!(!destination.join(VERSIONS_DIR).exists());
    }

    #[test]
    fn activation_failure_does_not_remove_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("download");
        fixture(&source, b"new");
        let entry = root.path().join("fabro");
        fs::create_dir(&entry).unwrap();
        fs::write(entry.join("keep"), b"old").unwrap();
        assert!(install(&source, &entry).is_err());
        assert_eq!(fs::read(entry.join("keep")).unwrap(), b"old");
    }
}
