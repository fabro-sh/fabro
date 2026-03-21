use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunSpec {
    pub run_id: String,
    pub workflow_path: PathBuf,
    pub dot_source: String,
    pub working_directory: PathBuf,
    pub goal: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub sandbox_provider: String,
    pub labels: HashMap<String, String>,
    pub verbose: bool,
    pub no_retro: bool,
    pub ssh: bool,
    pub preserve_sandbox: bool,
    pub dry_run: bool,
    pub auto_approve: bool,
}

impl RunSpec {
    pub fn save(&self, run_dir: &Path) -> anyhow::Result<()> {
        let path = run_dir.join("spec.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(run_dir: &Path) -> anyhow::Result<Self> {
        let path = run_dir.join("spec.json");
        let json = std::fs::read_to_string(path)?;
        let spec = serde_json::from_str(&json)?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> RunSpec {
        let mut labels = HashMap::new();
        labels.insert("env".to_string(), "test".to_string());
        labels.insert("team".to_string(), "platform".to_string());

        RunSpec {
            run_id: "run-abc123".to_string(),
            workflow_path: PathBuf::from("/home/user/workflows/deploy/workflow.toml"),
            dot_source: "digraph { a -> b }".to_string(),
            working_directory: PathBuf::from("/home/user/project"),
            goal: Some("Deploy to staging".to_string()),
            model: "claude-sonnet-4-20250514".to_string(),
            provider: Some("anthropic".to_string()),
            sandbox_provider: "local".to_string(),
            labels,
            verbose: true,
            no_retro: false,
            ssh: true,
            preserve_sandbox: false,
            dry_run: false,
            auto_approve: true,
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sample_spec();

        spec.save(dir.path()).unwrap();
        let loaded = RunSpec::load(dir.path()).unwrap();

        assert_eq!(loaded, spec);
    }

    #[test]
    fn load_nonexistent() {
        let dir = PathBuf::from("/tmp/nonexistent-run-spec-dir-that-does-not-exist");
        assert!(RunSpec::load(&dir).is_err());
    }
}
