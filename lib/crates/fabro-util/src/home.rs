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
        if let Some(root) = std::env::var_os("FABRO_HOME") {
            return Self::new(root);
        }

        let root =
            dirs::home_dir().map_or_else(|| PathBuf::from(".fabro"), |home| home.join(".fabro"));
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
            home.workflows_dir(),
            std::path::Path::new("/tmp/fabro-home/workflows")
        );
        assert_eq!(
            home.logs_dir(),
            std::path::Path::new("/tmp/fabro-home/logs")
        );
        assert_eq!(home.tmp_dir(), std::path::Path::new("/tmp/fabro-home/tmp"));
    }
}
