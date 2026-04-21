use std::path::{Path, PathBuf};

use fabro_store::RunProjection;
use fabro_types::{Checkpoint, RunSpec, StartRecord};
use git2::{Repository, Signature};

use crate::META_BRANCH_PREFIX;
use crate::author::GitAuthor;
use crate::branch::BranchStore;
use crate::error::{Error, MetadataError};
use crate::git::Store;

/// Git-native metadata storage for pipeline runs.
///
/// Stores checkpoint data, run specs, and metadata on an orphan branch
/// (`fabro/meta/{run_id}`) so that runs can be resumed from git alone.
pub struct MetadataStore {
    repo_path: PathBuf,
    author:    GitAuthor,
}

impl MetadataStore {
    pub fn new(repo_path: impl Into<PathBuf>, author: &GitAuthor) -> Self {
        Self {
            repo_path: repo_path.into(),
            author:    author.clone(),
        }
    }

    /// Returns the branch name for a run: `fabro/meta/{run_id}`.
    pub fn branch_name(run_id: &str) -> String {
        format!("{META_BRANCH_PREFIX}{run_id}")
    }

    /// Format a commit message with the standard Fabro footer appended.
    fn commit_message(&self, subject: &str) -> String {
        let mut msg = format!("{subject}\n");
        self.author.append_footer(&mut msg);
        msg
    }

    fn open_store(&self) -> Result<(Store, Signature<'static>), MetadataError> {
        let repo = Repository::discover(&self.repo_path).map_err(Error::from)?;
        let store = Store::new(repo);
        let sig = Signature::now(&self.author.name, &self.author.email).map_err(Error::from)?;
        Ok((store, sig))
    }

    /// Initialize a run's metadata branch with the given files.
    pub fn init_run(&self, run_id: &str, files: &[(&str, &[u8])]) -> Result<(), MetadataError> {
        let (store, sig) = self.open_store()?;
        let branch = Self::branch_name(run_id);
        let branch_store = BranchStore::new(&store, &branch, &sig);
        branch_store.ensure_branch()?;
        let message = self.commit_message("init run");
        branch_store.write_entries(files, &message)?;
        Ok(())
    }

    /// Write arbitrary files to the metadata branch.
    pub fn write_files(
        &self,
        run_id: &str,
        entries: &[(&str, &[u8])],
        message: &str,
    ) -> Result<(), MetadataError> {
        let (store, sig) = self.open_store()?;
        let branch = Self::branch_name(run_id);
        let branch_store = BranchStore::new(&store, &branch, &sig);
        let message = self.commit_message(message);
        branch_store.write_entries(entries, &message)?;
        Ok(())
    }

    /// Write a projection snapshot commit to the metadata branch and return
    /// the new commit SHA.
    pub fn write_snapshot(
        &self,
        run_id: &str,
        entries: &[(&str, &[u8])],
        message: &str,
    ) -> Result<String, MetadataError> {
        let (store, sig) = self.open_store()?;
        let branch = Self::branch_name(run_id);
        let branch_store = BranchStore::new(&store, &branch, &sig);
        let message = self.commit_message(message);
        let oid = branch_store.write_entries(entries, &message)?;
        Ok(oid.to_string())
    }

    /// Read a single file from the metadata branch. Returns `None` if branch or
    /// path doesn't exist.
    fn read_file(
        repo_path: &Path,
        run_id: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, MetadataError> {
        let Ok(repo) = Repository::discover(repo_path) else {
            return Ok(None);
        };
        let store = Store::new(repo);
        let sig = Signature::now("Fabro", "noreply@fabro.sh").map_err(Error::from)?;
        let branch = Self::branch_name(run_id);
        let branch_store = BranchStore::new(&store, &branch, &sig);
        Ok(branch_store.read_entry(path)?)
    }

    /// Read the projection snapshot from the metadata branch tip. Returns
    /// `None` if branch or file doesn't exist.
    pub fn read_run_projection(
        repo_path: &Path,
        run_id: &str,
    ) -> Result<Option<RunProjection>, MetadataError> {
        let branch = Self::branch_name(run_id);
        match Self::read_file(repo_path, run_id, "run.json")? {
            Some(bytes) => {
                let projection: RunProjection =
                    serde_json::from_slice(&bytes).map_err(|source| {
                        MetadataError::Deserialize {
                            entity: "run projection",
                            branch: branch.clone(),
                            source,
                        }
                    })?;
                let has_projection_data = projection.spec.is_some()
                    || projection.start.is_some()
                    || projection.status.is_some()
                    || projection.checkpoint.is_some()
                    || projection.conclusion.is_some()
                    || projection.sandbox.is_some()
                    || projection.retro.is_some()
                    || projection.graph_source.is_some()
                    || projection.iter_nodes().next().is_some();
                if !has_projection_data {
                    return Err(MetadataError::Deserialize {
                        entity: "run projection",
                        branch,
                        source: serde_json::Error::io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "run.json does not contain a serialized projection snapshot",
                        )),
                    });
                }
                Ok(Some(projection))
            }
            None => Ok(None),
        }
    }

    /// Read a checkpoint from the metadata branch. Returns `None` if branch or
    /// file doesn't exist.
    pub fn read_checkpoint(
        repo_path: &Path,
        run_id: &str,
    ) -> Result<Option<Checkpoint>, MetadataError> {
        Ok(Self::read_run_projection(repo_path, run_id)?
            .and_then(|projection| projection.checkpoint))
    }

    /// Read the run spec from the metadata branch. Returns `None` if not
    /// found.
    pub fn read_run_spec(repo_path: &Path, run_id: &str) -> Result<Option<RunSpec>, MetadataError> {
        Ok(Self::read_run_projection(repo_path, run_id)?.and_then(|projection| projection.spec))
    }

    /// Read the start record from the metadata branch. Returns `None` if not
    /// found.
    pub fn read_start_record(
        repo_path: &Path,
        run_id: &str,
    ) -> Result<Option<StartRecord>, MetadataError> {
        Ok(Self::read_run_projection(repo_path, run_id)?.and_then(|projection| projection.start))
    }

    /// Read an artifact from the metadata branch. Returns `None` if not found.
    pub fn read_artifact(
        repo_path: &Path,
        run_id: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, MetadataError> {
        Self::read_file(repo_path, run_id, &format!("artifacts/{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "These unit tests use the real git CLI to validate metadata branch behavior."
    )]

    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{Graph, fixtures};

    use super::*;

    /// Create a temporary git repo with an initial commit.
    fn init_repo(dir: &Path) {
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    fn test_run_spec(run_id: fabro_types::RunId) -> RunSpec {
        RunSpec {
            run_id,
            settings: SettingsLayer::default(),
            graph: Graph::new("test"),
            workflow_slug: None,
            working_directory: PathBuf::from("/tmp"),
            host_repo_path: None,
            repo_origin_url: None,
            base_branch: None,
            labels: HashMap::new(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
        }
    }

    fn test_checkpoint(
        current_node: &str,
        completed_nodes: Vec<String>,
        next_node_id: Option<String>,
    ) -> Checkpoint {
        Checkpoint {
            timestamp: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).single().unwrap(),
            current_node: current_node.to_string(),
            completed_nodes,
            node_retries: HashMap::new(),
            context_values: HashMap::new(),
            node_outcomes: HashMap::new(),
            next_node_id,
            git_commit_sha: None,
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::new(),
        }
    }

    fn test_projection(run_id: fabro_types::RunId) -> RunProjection {
        let mut projection = RunProjection::default();
        projection.spec = Some(test_run_spec(run_id));
        projection
    }

    fn projection_bytes(projection: &RunProjection) -> Vec<u8> {
        serde_json::to_vec_pretty(projection).unwrap()
    }

    fn branch_entry(repo_dir: &Path, run_id: &str, path: &str) -> Vec<u8> {
        let repo = Repository::discover(repo_dir).unwrap();
        let store = Store::new(repo);
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let branch = MetadataStore::branch_name(run_id);
        let branch_store = BranchStore::new(&store, &branch, &sig);
        branch_store.read_entry(path).unwrap().unwrap()
    }

    #[test]
    fn metadata_store_init_run_and_read() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let run_id = fixtures::RUN_1.to_string();
        let projection = projection_bytes(&test_projection(fixtures::RUN_1));
        store
            .init_run(&run_id, &[("run.json", &projection)])
            .unwrap();

        let read_spec = MetadataStore::read_run_spec(dir.path(), &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(read_spec.run_id, fixtures::RUN_1);
        assert_eq!(read_spec.graph.name, "test");
    }

    #[test]
    fn metadata_store_write_and_read_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_2.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let init_projection = projection_bytes(&test_projection(fixtures::RUN_2));
        store
            .init_run(&run_id, &[("run.json", &init_projection)])
            .unwrap();

        let mut checkpoint = test_checkpoint(
            "node_a",
            vec!["start".to_string()],
            Some("node_b".to_string()),
        );
        checkpoint
            .context_values
            .insert("goal".to_string(), serde_json::json!("test"));
        let mut snapshot = test_projection(fixtures::RUN_2);
        snapshot.checkpoint = Some(checkpoint);
        let snapshot_json = projection_bytes(&snapshot);
        store
            .write_snapshot(&run_id, &[("run.json", &snapshot_json)], "checkpoint")
            .unwrap();

        let loaded = MetadataStore::read_checkpoint(dir.path(), &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.current_node, "node_a");
        assert_eq!(loaded.completed_nodes, vec!["start"]);
        assert_eq!(loaded.next_node_id.as_deref(), Some("node_b"));
        assert_eq!(
            loaded.context_values.get("goal"),
            Some(&serde_json::json!("test"))
        );
    }

    #[test]
    fn metadata_store_write_checkpoint_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_3.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let init_projection = projection_bytes(&test_projection(fixtures::RUN_3));
        store
            .init_run(&run_id, &[("run.json", &init_projection)])
            .unwrap();

        let mut snapshot_one = test_projection(fixtures::RUN_3);
        snapshot_one.checkpoint = Some(test_checkpoint("node_a", vec!["start".to_string()], None));
        let checkpoint_one = projection_bytes(&snapshot_one);
        store
            .write_snapshot(&run_id, &[("run.json", &checkpoint_one)], "checkpoint")
            .unwrap();

        let mut snapshot_two = test_projection(fixtures::RUN_3);
        snapshot_two.checkpoint = Some(test_checkpoint(
            "node_b",
            vec!["start".to_string(), "node_a".to_string()],
            Some("node_c".to_string()),
        ));
        let checkpoint_two = projection_bytes(&snapshot_two);
        store
            .write_snapshot(&run_id, &[("run.json", &checkpoint_two)], "checkpoint")
            .unwrap();

        let loaded = MetadataStore::read_checkpoint(dir.path(), &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.current_node, "node_b");
        assert_eq!(loaded.completed_nodes.len(), 2);
    }

    #[test]
    fn metadata_store_read_checkpoint_missing_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let result = MetadataStore::read_checkpoint(dir.path(), "NONEXISTENT").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn metadata_store_artifact_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_4.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let init_projection = projection_bytes(&test_projection(fixtures::RUN_4));
        store
            .init_run(&run_id, &[("run.json", &init_projection)])
            .unwrap();

        let artifact_data = br#"{"large_output":"some data"}"#;
        let mut snapshot = test_projection(fixtures::RUN_4);
        snapshot.checkpoint = Some(test_checkpoint("node_a", Vec::new(), None));
        let snapshot_json = projection_bytes(&snapshot);
        store
            .write_snapshot(
                &run_id,
                &[
                    ("run.json", &snapshot_json),
                    ("artifacts/response.plan.json", artifact_data.as_slice()),
                ],
                "checkpoint",
            )
            .unwrap();

        let read_back = MetadataStore::read_artifact(dir.path(), &run_id, "response.plan")
            .unwrap()
            .unwrap();
        assert_eq!(read_back, artifact_data);
    }

    #[test]
    fn metadata_store_write_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_5.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let projection = projection_bytes(&test_projection(fixtures::RUN_5));
        store
            .init_run(&run_id, &[("run.json", &projection)])
            .unwrap();

        store
            .write_files(
                &run_id,
                &[("retro/prompt.md", b"how did it go?")],
                "finalize run",
            )
            .unwrap();

        let data = branch_entry(dir.path(), &run_id, "retro/prompt.md");
        assert_eq!(data, b"how did it go?");

        let spec = MetadataStore::read_run_spec(dir.path(), &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(spec.run_id, fixtures::RUN_5);
    }

    #[test]
    fn metadata_store_init_run_with_extra_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_6.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        store
            .init_run(&run_id, &[("graph.fabro", b"digraph Test {}")])
            .unwrap();

        let data = branch_entry(dir.path(), &run_id, "graph.fabro");
        assert_eq!(data, b"digraph Test {}");
    }

    #[test]
    fn metadata_store_read_start_record_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let run_id = fixtures::RUN_6.to_string();
        let store = MetadataStore::new(dir.path(), &GitAuthor::default());
        let start_record = StartRecord {
            run_id:     fixtures::RUN_6,
            start_time: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).single().unwrap(),
            run_branch: Some("fabro/run/test".to_string()),
            base_sha:   None,
        };
        let mut projection = test_projection(fixtures::RUN_6);
        projection.start = Some(start_record);
        let bytes = projection_bytes(&projection);
        store.init_run(&run_id, &[("run.json", &bytes)]).unwrap();

        let loaded = MetadataStore::read_start_record(dir.path(), &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.run_id, fixtures::RUN_6);
        assert_eq!(loaded.run_branch.as_deref(), Some("fabro/run/test"));
    }
}
