use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub resume: Option<PathBuf>,
    pub run_branch: Option<String>,
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
            resume: Some(PathBuf::from("/tmp/checkpoint")),
            run_branch: Some("fabro/run/abc123".to_string()),
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sample_spec();

        spec.save(dir.path()).unwrap();
        let loaded = RunSpec::load(dir.path()).unwrap();

        assert_eq!(loaded.run_id, spec.run_id);
        assert_eq!(loaded.workflow_path, spec.workflow_path);
        assert_eq!(loaded.dot_source, spec.dot_source);
        assert_eq!(loaded.working_directory, spec.working_directory);
        assert_eq!(loaded.goal, spec.goal);
        assert_eq!(loaded.model, spec.model);
        assert_eq!(loaded.provider, spec.provider);
        assert_eq!(loaded.sandbox_provider, spec.sandbox_provider);
        assert_eq!(loaded.labels, spec.labels);
        assert_eq!(loaded.verbose, spec.verbose);
        assert_eq!(loaded.no_retro, spec.no_retro);
        assert_eq!(loaded.ssh, spec.ssh);
        assert_eq!(loaded.preserve_sandbox, spec.preserve_sandbox);
        assert_eq!(loaded.dry_run, spec.dry_run);
        assert_eq!(loaded.auto_approve, spec.auto_approve);
        assert_eq!(loaded.resume, spec.resume);
        assert_eq!(loaded.run_branch, spec.run_branch);
    }

    #[test]
    fn load_nonexistent() {
        let dir = PathBuf::from("/tmp/nonexistent-run-spec-dir-that-does-not-exist");
        assert!(RunSpec::load(&dir).is_err());
    }
}
