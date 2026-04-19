use std::path::{Path, PathBuf};

use chrono::Local;
use fabro_types::RunId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerState {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunScratch {
    root: PathBuf,
}

impl Storage {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    #[must_use]
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    #[must_use]
    pub fn secrets_path(&self) -> PathBuf {
        self.root
            .join("vaults")
            .join("default")
            .join("secrets.json")
    }

    #[must_use]
    pub fn server_state(&self) -> ServerState {
        ServerState::new(self.root.clone())
    }

    #[must_use]
    pub fn run_scratch(&self, run_id: &RunId) -> RunScratch {
        RunScratch::for_run(&self.scratch_dir(), run_id)
    }

    #[must_use]
    pub fn scratch_dir(&self) -> PathBuf {
        self.root.join("scratch")
    }

    #[must_use]
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    #[must_use]
    pub fn slatedb_dir(&self) -> PathBuf {
        self.objects_dir().join("slatedb")
    }

    #[must_use]
    pub fn artifacts_dir(&self) -> PathBuf {
        self.objects_dir().join("artifacts")
    }
}

impl ServerState {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        self.root.join("server.json")
    }

    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("server.lock")
    }

    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.root.join("logs").join("server.log")
    }

    #[must_use]
    pub fn env_path(&self) -> PathBuf {
        self.root.join("server.env")
    }

    #[must_use]
    pub fn dev_token_path(&self) -> PathBuf {
        self.root.join("server.dev-token")
    }
}

impl RunScratch {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create a `RunScratch` for a given run under a scratch directory.
    #[must_use]
    pub fn for_run(scratch_dir: &Path, run_id: &RunId) -> Self {
        let local_dt = run_id.created_at().with_timezone(&Local);
        Self::new(scratch_dir.join(format!("{}-{run_id}", local_dt.format("%Y%m%d"))))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn worktree_dir(&self) -> PathBuf {
        self.root.join("worktree")
    }

    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    pub fn create(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.worktree_dir())?;
        std::fs::create_dir_all(self.runtime_dir())?;
        Ok(())
    }

    pub fn remove(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Local;
    use fabro_types::RunId;

    use super::{RunScratch, Storage};

    #[test]
    fn storage_accessors_are_relative_to_root() {
        let storage = Storage::new("/tmp/fabro-data");

        assert_eq!(storage.root(), std::path::Path::new("/tmp/fabro-data"));
        assert_eq!(
            storage.logs_dir(),
            std::path::Path::new("/tmp/fabro-data/logs")
        );
        assert_eq!(
            storage.cache_dir(),
            std::path::Path::new("/tmp/fabro-data/cache")
        );
        assert_eq!(
            storage.secrets_path(),
            std::path::Path::new("/tmp/fabro-data/vaults/default/secrets.json")
        );
        assert_eq!(
            storage.objects_dir(),
            std::path::Path::new("/tmp/fabro-data/objects")
        );
        assert_eq!(
            storage.slatedb_dir(),
            std::path::Path::new("/tmp/fabro-data/objects/slatedb")
        );
        assert_eq!(
            storage.artifacts_dir(),
            std::path::Path::new("/tmp/fabro-data/objects/artifacts")
        );
        assert_eq!(
            storage.server_state().record_path(),
            std::path::Path::new("/tmp/fabro-data/server.json")
        );
        assert_eq!(
            storage.server_state().lock_path(),
            std::path::Path::new("/tmp/fabro-data/server.lock")
        );
        assert_eq!(
            storage.server_state().log_path(),
            std::path::Path::new("/tmp/fabro-data/logs/server.log")
        );
        assert_eq!(
            storage.server_state().env_path(),
            std::path::Path::new("/tmp/fabro-data/server.env")
        );
    }

    #[test]
    fn run_scratch_uses_run_id_local_date() {
        let storage = Storage::new("/tmp/fabro-data");
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();

        let expected_date = run_id
            .created_at()
            .with_timezone(&Local)
            .format("%Y%m%d")
            .to_string();

        assert_eq!(
            storage.run_scratch(&run_id).root(),
            std::path::Path::new("/tmp/fabro-data/scratch")
                .join(format!("{expected_date}-{run_id}"))
        );
    }

    #[test]
    fn run_scratch_accessors_and_lifecycle_are_relative_to_root() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = RunScratch::new(dir.path().join("20260327-01TEST"));

        assert_eq!(scratch.worktree_dir(), scratch.root().join("worktree"));
        assert_eq!(scratch.runtime_dir(), scratch.root().join("runtime"));

        scratch.create().unwrap();
        assert!(scratch.root().exists());
        assert!(scratch.worktree_dir().exists());
        assert!(scratch.runtime_dir().exists());
        assert!(!scratch.root().join("cache").join("artifacts").exists());
        scratch.remove().unwrap();
        assert!(!scratch.root().exists());
    }
}
