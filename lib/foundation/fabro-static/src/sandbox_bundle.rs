//! The executable bundle a packaged Fabro release ships: `fabro` and the
//! sandbox-driver plugins whose checksums Petri embeds at build time.

use std::path::Path;

use crate::EnvVars;

/// One of Petri's sandbox-driver plugins and the environment variables that
/// name its executable and checksum override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPlugin {
    /// The provider kind as Petri spells it (`host`, `docker`, `daytona`).
    pub kind:       &'static str,
    /// The plugin executable's file name.
    pub binary:     &'static str,
    /// The variable naming an explicit plugin executable path.
    pub path_var:   &'static str,
    /// The variable overriding the plugin's expected SHA-256.
    pub sha256_var: &'static str,
}

/// Every plugin a complete bundle contains, beside `fabro`.
pub const SANDBOX_PLUGINS: [SandboxPlugin; 3] = [
    SandboxPlugin {
        kind:       "host",
        binary:     "sandbox-driver-host",
        path_var:   EnvVars::PETRI_SANDBOX_HOST_PLUGIN,
        sha256_var: EnvVars::PETRI_SANDBOX_HOST_SHA256,
    },
    SandboxPlugin {
        kind:       "docker",
        binary:     "sandbox-driver-docker",
        path_var:   EnvVars::PETRI_SANDBOX_DOCKER_PLUGIN,
        sha256_var: EnvVars::PETRI_SANDBOX_DOCKER_SHA256,
    },
    SandboxPlugin {
        kind:       "daytona",
        binary:     "sandbox-driver-daytona",
        path_var:   EnvVars::PETRI_SANDBOX_DAYTONA_PLUGIN,
        sha256_var: EnvVars::PETRI_SANDBOX_DAYTONA_SHA256,
    },
];

/// The plugin executable file names, in bundle order.
pub const SANDBOX_PLUGIN_BINARIES: [&str; 3] = [
    SANDBOX_PLUGINS[0].binary,
    SANDBOX_PLUGINS[1].binary,
    SANDBOX_PLUGINS[2].binary,
];

/// Whether this build embeds plugin checksums, which only packaged builds do.
/// A bare `cargo build` has no companions to be missing.
pub const PLUGIN_PINS_EMBEDDED: bool = option_env!("PETRI_SANDBOX_PLUGIN_DIR").is_some();

/// The directory under an installation root where the updater and the shell
/// installer keep immutable, versioned bundles; the public `fabro` launcher
/// is a symlink into one of them.
pub const MANAGED_VERSIONS_DIR: &str = ".fabro-versions";

/// The installation root of a managed bundle, given the canonical path of an
/// executable inside it (`<root>/.fabro-versions/<bundle>/fabro`), or `None`
/// for a flat install.
pub fn managed_install_root(executable: &Path) -> Option<&Path> {
    let versions = executable.parent()?.parent()?;
    (versions.file_name()? == MANAGED_VERSIONS_DIR).then(|| versions.parent())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_install_root_is_the_versions_directory_parent() {
        assert_eq!(
            managed_install_root(Path::new("/opt/fabro/.fabro-versions/bundle-a/fabro")),
            Some(Path::new("/opt/fabro"))
        );
        assert_eq!(
            managed_install_root(Path::new("/opt/fabro/bin/fabro")),
            None
        );
        assert_eq!(managed_install_root(Path::new("fabro")), None);
    }
}
