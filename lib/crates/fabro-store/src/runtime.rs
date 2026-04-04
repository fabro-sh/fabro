use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeState {
    root: PathBuf,
}

impl RuntimeState {
    #[must_use]
    pub fn new(run_dir: impl AsRef<Path>) -> Self {
        Self {
            root: run_dir.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    #[must_use]
    pub fn interview_request_path(&self) -> PathBuf {
        self.runtime_dir().join("interview_request.json")
    }

    #[must_use]
    pub fn interview_response_path(&self) -> PathBuf {
        self.runtime_dir().join("interview_response.json")
    }

    #[must_use]
    pub fn interview_claim_path(&self) -> PathBuf {
        self.runtime_dir().join("interview_request.claim")
    }

    #[must_use]
    pub fn artifact_values_dir(&self) -> PathBuf {
        self.root.join("cache").join("artifacts").join("values")
    }

    #[must_use]
    pub fn artifact_value_path(&self, artifact_id: &str) -> PathBuf {
        self.artifact_values_dir()
            .join(format!("{artifact_id}.json"))
    }

    #[must_use]
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("cache").join("artifacts").join("assets")
    }

    #[must_use]
    pub fn asset_stage_dir(&self, node_slug: &str, attempt: u32) -> PathBuf {
        self.assets_dir()
            .join(node_slug)
            .join(format!("retry_{attempt}"))
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeState;

    #[test]
    fn computes_runtime_and_cache_paths() {
        let dir = tempfile::tempdir().unwrap();
        let state = RuntimeState::new(dir.path());

        assert_eq!(state.runtime_dir(), dir.path().join("runtime"));
        assert_eq!(
            state.interview_request_path(),
            dir.path().join("runtime").join("interview_request.json")
        );
        assert_eq!(
            state.interview_response_path(),
            dir.path().join("runtime").join("interview_response.json")
        );
        assert_eq!(
            state.interview_claim_path(),
            dir.path().join("runtime").join("interview_request.claim")
        );
        assert_eq!(
            state.artifact_values_dir(),
            dir.path().join("cache").join("artifacts").join("values")
        );
        assert_eq!(
            state.artifact_value_path("response.plan"),
            dir.path()
                .join("cache")
                .join("artifacts")
                .join("values")
                .join("response.plan.json")
        );
        assert_eq!(
            state.assets_dir(),
            dir.path().join("cache").join("artifacts").join("assets")
        );
        assert_eq!(
            state.asset_stage_dir("plan", 2),
            dir.path()
                .join("cache")
                .join("artifacts")
                .join("assets")
                .join("plan")
                .join("retry_2")
        );
    }
}
