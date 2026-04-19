use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_lookup(|name| std::env::var_os(name), dirs::home_dir())
    }

    #[must_use]
    fn from_env_with_lookup(
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
        fallback_home: Option<PathBuf>,
    ) -> Self {
        if let Some(root) = lookup("FABRO_HOME") {
            return Self::new(root);
        }

        if let Some(home) = lookup("HOME") {
            return Self::new(PathBuf::from(home).join(".fabro"));
        }

        let root =
            fallback_home.map_or_else(|| PathBuf::from(".fabro"), |home| home.join(".fabro"));
        Self::new(root)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn user_config(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    #[must_use]
    pub fn server_config(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    #[must_use]
    pub fn certs_dir(&self) -> PathBuf {
        self.root.join("certs")
    }

    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.root.join("storage")
    }

    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("fabro.sock")
    }

    #[must_use]
    pub fn dev_token_path(&self) -> PathBuf {
        self.root.join("dev-token")
    }

    #[must_use]
    pub fn workflows_dir(&self) -> PathBuf {
        self.root.join("workflows")
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    #[must_use]
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }
}

#[cfg(test)]
mod tests {
    use super::Home;

    #[test]
    fn accessors_are_relative_to_root() {
        let home = Home::new("/tmp/fabro-home");

        assert_eq!(home.root(), std::path::Path::new("/tmp/fabro-home"));
        assert_eq!(
            home.user_config(),
            std::path::Path::new("/tmp/fabro-home/settings.toml")
        );
        assert_eq!(
            home.server_config(),
            std::path::Path::new("/tmp/fabro-home/settings.toml")
        );
        assert_eq!(
            home.certs_dir(),
            std::path::Path::new("/tmp/fabro-home/certs")
        );
        assert_eq!(
            home.skills_dir(),
            std::path::Path::new("/tmp/fabro-home/skills")
        );
        assert_eq!(
            home.storage_dir(),
            std::path::Path::new("/tmp/fabro-home/storage")
        );
        assert_eq!(
            home.socket_path(),
            std::path::Path::new("/tmp/fabro-home/fabro.sock")
        );
        assert_eq!(
            home.dev_token_path(),
            std::path::Path::new("/tmp/fabro-home/dev-token")
        );
        assert_eq!(
            home.workflows_dir(),
            std::path::Path::new("/tmp/fabro-home/workflows")
        );
        assert_eq!(
            home.logs_dir(),
            std::path::Path::new("/tmp/fabro-home/logs")
        );
        assert_eq!(home.tmp_dir(), std::path::Path::new("/tmp/fabro-home/tmp"));
    }

    #[test]
    fn from_env_prefers_home_env_when_fabro_home_is_absent() {
        let home = Home::from_env_with_lookup(
            |name| match name {
                "HOME" => Some(std::ffi::OsString::from("/tmp/fabro-home-env")),
                _ => None,
            },
            None,
        );

        assert_eq!(
            home.root(),
            std::path::Path::new("/tmp/fabro-home-env/.fabro")
        );
    }
}
