//! Keep subprocesses on the server's executable bundle through an upgrade.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fabro_static::{PLUGIN_PINS_EMBEDDED, SANDBOX_PLUGIN_BINARIES, managed_install_root};
use tempfile::TempDir;

#[derive(Debug)]
pub(crate) struct ServerExecutable {
    path:    PathBuf,
    // Owned by serve_command until its workers have shut down.
    _bundle: Option<TempDir>,
}

impl ServerExecutable {
    pub(crate) fn current(storage: &Path) -> Result<Self> {
        let executable = std::env::current_exe()
            .context("resolving the server executable")?
            .canonicalize()
            .context("resolving the server executable bundle")?;
        if !PLUGIN_PINS_EMBEDDED {
            return Ok(Self {
                path:    executable,
                _bundle: None,
            });
        }
        Self::retain(&executable, storage)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "startup snapshots executable files on a blocking thread"
    )]
    fn retain(executable: &Path, storage: &Path) -> Result<Self> {
        let source = executable
            .parent()
            .context("server executable has no directory")?;
        if managed_install_root(executable).is_some() {
            // Managed bundles are already immutable and retained by the updater.
            return Ok(Self {
                path:    executable.to_owned(),
                _bundle: None,
            });
        }

        // A flat install's public executable path changes during its first upgrade.
        // Copy its bytes, rather than retaining a path or a hard link that a build
        // or package manager can overwrite. Storage is writable even when bin isn't.
        let bundle = tempfile::Builder::new()
            .prefix(".server-bundle-")
            .tempdir_in(storage)
            .context("retaining the server executable bundle")?;
        fs::copy(executable, bundle.path().join("fabro"))
            .context("retaining the server executable")?;
        for name in SANDBOX_PLUGIN_BINARIES {
            let plugin = source.join(name);
            // Incomplete installations can still use explicit plugin paths or PATH.
            // Preserve those fallbacks instead of making startup require companions.
            match fs::copy(&plugin, bundle.path().join(name)) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("retaining {name}")),
            }
        }
        let path = bundle
            .path()
            .join("fabro")
            .canonicalize()
            .context("resolving the retained server executable")?;
        tracing::debug!(source = %executable.display(), path = %path.display(), "Retained server executable bundle");
        Ok(Self {
            path,
            _bundle: Some(bundle),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests create disposable executable fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn flat_bundle_survives_replacement_and_is_removed_when_server_drops() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("install");
        fs::create_dir(&install).unwrap();
        let names = std::iter::once("fabro").chain(SANDBOX_PLUGIN_BINARIES);
        for name in names.clone() {
            fs::write(install.join(name), format!("old {name}")).unwrap();
        }
        let retained = ServerExecutable::retain(&install.join("fabro"), root.path()).unwrap();
        let bundle = retained.path().parent().unwrap().to_owned();
        for name in names {
            fs::write(install.join(name), b"new").unwrap();
            assert_eq!(
                fs::read(bundle.join(name)).unwrap(),
                format!("old {name}").as_bytes()
            );
        }
        drop(retained);
        assert!(!bundle.exists());
    }

    #[test]
    fn managed_bundle_is_reused_without_writing_to_storage() {
        let root = tempfile::tempdir().unwrap();
        let executable = root
            .path()
            .join(fabro_static::MANAGED_VERSIONS_DIR)
            .join("bundle-first/fabro");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"old").unwrap();
        let retained = ServerExecutable::retain(&executable, &root.path().join("absent")).unwrap();
        assert_eq!(retained.path(), executable);
        drop(retained);
        assert!(executable.exists());
    }

    #[test]
    fn incomplete_bundle_keeps_missing_plugins_missing() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fabro");
        fs::write(&executable, b"fabro").unwrap();
        let retained = ServerExecutable::retain(&executable, root.path()).unwrap();
        assert_eq!(fs::read(retained.path()).unwrap(), b"fabro");
        assert!(
            !retained
                .path()
                .with_file_name("sandbox-driver-host")
                .exists()
        );
    }

    #[test]
    fn failed_copy_preserves_its_io_error_and_removes_partial_bundle() {
        let root = tempfile::tempdir().unwrap();
        let error = ServerExecutable::retain(&root.path().join("absent"), root.path()).unwrap_err();
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|cause| cause.kind() == std::io::ErrorKind::NotFound)
        }));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
