use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use serde::Serialize;

use crate::checkpoint::Checkpoint;
use crate::cli::runs::{default_runs_base, resolve_run, RunStatus};
use crate::conclusion::Conclusion;
use crate::manifest::Manifest;
use crate::sandbox_record::SandboxRecord;

#[derive(Args)]
pub struct InspectArgs {
    /// Run ID prefix or workflow name (most recent run)
    pub run: String,
}

#[derive(Debug, Serialize)]
pub struct InspectOutput {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub status: RunStatus,
    pub manifest: Option<serde_json::Value>,
    pub conclusion: Option<serde_json::Value>,
    pub checkpoint: Option<serde_json::Value>,
    pub sandbox: Option<serde_json::Value>,
}

pub fn inspect_command(args: &InspectArgs) -> Result<()> {
    let base = default_runs_base();
    let run = resolve_run(&base, &args.run)?;
    let output = inspect_run_dir(&run.run_id, &run.path, run.status)?;
    let json = serde_json::to_string_pretty(&[output])?;
    println!("{json}");
    Ok(())
}

fn inspect_run_dir(run_id: &str, run_dir: &Path, status: RunStatus) -> Result<InspectOutput> {
    let manifest = Manifest::load(&run_dir.join("manifest.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let conclusion = Conclusion::load(&run_dir.join("conclusion.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let checkpoint = Checkpoint::load(&run_dir.join("checkpoint.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());
    let sandbox = SandboxRecord::load(&run_dir.join("sandbox.json"))
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());

    Ok(InspectOutput {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        status,
        manifest,
        conclusion,
        checkpoint,
        sandbox,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::runs::RunStatus;
    use crate::outcome::StageStatus;

    #[test]
    fn nonexistent_run_returns_error() {
        let args = InspectArgs {
            run: "nonexistent-run-id".to_string(),
        };
        assert!(inspect_command(&args).is_err());
    }

    #[test]
    fn inspect_complete_run_has_all_sections() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Write all four JSON files
        let manifest = Manifest {
            run_id: "test-run".to_string(),
            workflow_name: "test".to_string(),
            goal: "test goal".to_string(),
            start_time: chrono::Utc::now(),
            node_count: 2,
            edge_count: 1,
            run_branch: None,
            base_sha: None,
            labels: Default::default(),
            base_branch: None,
            workflow_slug: None,
            host_repo_path: None,
        };
        manifest.save(&run_dir.join("manifest.json")).unwrap();

        let conclusion = Conclusion {
            timestamp: chrono::Utc::now(),
            status: StageStatus::Success,
            duration_ms: 1000,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: None,
            total_retries: 0,
        };
        conclusion.save(&run_dir.join("conclusion.json")).unwrap();

        let checkpoint = Checkpoint {
            timestamp: chrono::Utc::now(),
            current_node: "end".to_string(),
            completed_nodes: vec!["start".to_string()],
            node_retries: Default::default(),
            context_values: Default::default(),
            logs: vec![],
            node_outcomes: Default::default(),
            next_node_id: None,
            git_commit_sha: None,
            loop_failure_signatures: Default::default(),
            restart_failure_signatures: Default::default(),
            node_visits: Default::default(),
        };
        checkpoint.save(&run_dir.join("checkpoint.json")).unwrap();

        let sandbox = SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp/work".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        sandbox.save(&run_dir.join("sandbox.json")).unwrap();

        let output = inspect_run_dir("test-run", &run_dir, RunStatus::Succeeded).unwrap();

        assert_eq!(output.run_id, "test-run");
        assert_eq!(output.run_dir, run_dir);
        assert!(output.manifest.is_some());
        assert!(output.conclusion.is_some());
        assert!(output.checkpoint.is_some());
        assert!(output.sandbox.is_some());
    }

    #[test]
    fn inspect_partial_run_has_null_sections() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Only write manifest
        let manifest = Manifest {
            run_id: "partial-run".to_string(),
            workflow_name: "test".to_string(),
            goal: "test goal".to_string(),
            start_time: chrono::Utc::now(),
            node_count: 1,
            edge_count: 0,
            run_branch: None,
            base_sha: None,
            labels: Default::default(),
            base_branch: None,
            workflow_slug: None,
            host_repo_path: None,
        };
        manifest.save(&run_dir.join("manifest.json")).unwrap();

        let output = inspect_run_dir("partial-run", &run_dir, RunStatus::Running).unwrap();

        assert_eq!(output.run_id, "partial-run");
        assert!(output.manifest.is_some());
        assert!(output.conclusion.is_none());
        assert!(output.checkpoint.is_none());
        assert!(output.sandbox.is_none());
    }

    #[test]
    fn output_json_has_expected_keys() {
        let output = InspectOutput {
            run_id: "id-1".to_string(),
            run_dir: PathBuf::from("/tmp/run"),
            status: RunStatus::Dead,
            manifest: None,
            conclusion: None,
            checkpoint: None,
            sandbox: None,
        };

        let json: serde_json::Value = serde_json::to_value(&[output]).unwrap();
        let obj = json.as_array().unwrap()[0].as_object().unwrap();
        let keys: Vec<&String> = obj.keys().collect();
        assert!(keys.contains(&&"run_id".to_string()));
        assert!(keys.contains(&&"run_dir".to_string()));
        assert!(keys.contains(&&"status".to_string()));
        assert!(keys.contains(&&"manifest".to_string()));
        assert!(keys.contains(&&"conclusion".to_string()));
        assert!(keys.contains(&&"checkpoint".to_string()));
        assert!(keys.contains(&&"sandbox".to_string()));
        assert_eq!(keys.len(), 7);
    }
}
