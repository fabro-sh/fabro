use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store as GitStore;
use fabro_store::{
    ListRunsQuery, NodeVisitRef, RunStore as DurableRunStore, Store as DurableStore,
};
use git2::{Repository, Signature};
use ulid::Ulid;

use super::rewind::{self, RunTimeline, build_timeline};
use crate::git::MetadataStore;
use crate::records::Checkpoint;

pub async fn rebuild_metadata_branch(
    git_store: &GitStore,
    run_store: &dyn DurableRunStore,
    run_id: &str,
) -> Result<()> {
    let branch = MetadataStore::branch_name(run_id);
    if git_store.resolve_ref(&branch)?.is_some() {
        bail!("metadata branch already exists for run {run_id}");
    }

    let run_record = run_store
        .get_run()
        .await?
        .ok_or_else(|| anyhow::anyhow!("run record not found for {run_id}"))?;

    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;
    let scratch_branch = format!("fabro/meta-rebuild/{run_id}/{}", Ulid::new());
    let bs = BranchStore::new(git_store, &scratch_branch, &sig);

    let result = async {
        bs.ensure_branch()?;

        let mut init_entries = Vec::new();
        init_entries.push((
            "run.json".to_string(),
            serde_json::to_vec_pretty(&run_record)?,
        ));
        if let Some(start) = run_store.get_start().await? {
            init_entries.push(("start.json".to_string(), serde_json::to_vec_pretty(&start)?));
        }
        if let Some(sandbox) = run_store.get_sandbox().await? {
            init_entries.push((
                "sandbox.json".to_string(),
                serde_json::to_vec_pretty(&sandbox)?,
            ));
        }
        write_entries(&bs, &init_entries, "init run")?;

        let mut checkpoints = run_store.list_checkpoints().await?;
        backfill_missing_checkpoint_shas(git_store, run_id, &mut checkpoints);

        for (_seq, checkpoint) in checkpoints {
            let mut entries = Vec::new();
            entries.push((
                "checkpoint.json".to_string(),
                serde_json::to_vec_pretty(&checkpoint)?,
            ));

            for node_id in &checkpoint.completed_nodes {
                let max_visit = checkpoint.node_visits.get(node_id).copied().unwrap_or(1);
                for visit in 1..=max_visit {
                    let visit = u32::try_from(visit)
                        .with_context(|| format!("visit {visit} for node {node_id} exceeds u32"))?;
                    let node = run_store.get_node(&NodeVisitRef { node_id, visit }).await?;

                    if let Some(prompt) = node.prompt {
                        entries.push((
                            node_file_path(node_id, visit, "prompt.md"),
                            prompt.into_bytes(),
                        ));
                    }
                    if let Some(response) = node.response {
                        entries.push((
                            node_file_path(node_id, visit, "response.md"),
                            response.into_bytes(),
                        ));
                    }
                    if let Some(status) = node.status {
                        entries.push((
                            node_file_path(node_id, visit, "status.json"),
                            serde_json::to_vec_pretty(&status)?,
                        ));
                    }
                }
            }

            write_entries(&bs, &entries, "checkpoint")?;
        }

        if let Some(retro) = run_store.get_retro().await? {
            let entries = vec![("retro.json".to_string(), serde_json::to_vec_pretty(&retro)?)];
            write_entries(&bs, &entries, "finalize run")?;
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    let final_result = match result {
        Ok(()) => {
            let scratch_tip = git_store
                .resolve_ref(&scratch_branch)?
                .ok_or_else(|| anyhow::anyhow!("scratch metadata branch missing after rebuild"))?;
            git_store.update_ref(&branch, scratch_tip)?;
            Ok(())
        }
        Err(err) => Err(err),
    };

    let _ = git_store.delete_ref(&scratch_branch);
    final_result
}

pub async fn build_timeline_or_rebuild(
    git_store: &GitStore,
    run_store: Option<&dyn DurableRunStore>,
    run_id: &str,
) -> Result<RunTimeline> {
    let branch = MetadataStore::branch_name(run_id);
    if git_store.resolve_ref(&branch)?.is_some() {
        return build_timeline(git_store, run_id);
    }

    if let Some(run_store) = run_store {
        rebuild_metadata_branch(git_store, run_store, run_id).await?;
        return build_timeline(git_store, run_id);
    }

    Ok(RunTimeline {
        entries: Vec::new(),
        parallel_map: HashMap::new(),
    })
}

pub async fn find_run_id_by_prefix_or_store(
    repo: &Repository,
    fabro_store: &dyn DurableStore,
    prefix: &str,
) -> Result<String> {
    if let Some(run_id) = find_run_id_by_prefix_in_refs(repo, prefix)? {
        return Ok(run_id);
    }

    let current_repo_root = canonical_repo_root(repo)?;
    let mut matches = Vec::new();
    for summary in fabro_store.list_runs(&ListRunsQuery::default()).await? {
        if summary.run_id == prefix {
            if summary.host_repo_path.is_none() {
                return Ok(summary.run_id);
            }

            let Some(host_repo_path) = summary.host_repo_path.as_deref() else {
                continue;
            };
            let Ok(host_repo) = Repository::discover(host_repo_path) else {
                continue;
            };
            let Ok(host_repo_root) = canonical_repo_root(&host_repo) else {
                continue;
            };
            if host_repo_root == current_repo_root {
                return Ok(summary.run_id);
            }
            continue;
        }

        let Some(host_repo_path) = summary.host_repo_path.as_deref() else {
            continue;
        };
        let Ok(host_repo) = Repository::discover(host_repo_path) else {
            continue;
        };
        let Ok(host_repo_root) = canonical_repo_root(&host_repo) else {
            continue;
        };
        if host_repo_root == current_repo_root && summary.run_id.starts_with(prefix) {
            matches.push(summary.run_id);
        }
    }

    resolve_prefix_matches(prefix, matches)
}

fn write_entries(
    branch_store: &BranchStore<'_>,
    entries: &[(String, Vec<u8>)],
    message: &str,
) -> Result<()> {
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    branch_store.write_entries(&refs, message)?;
    Ok(())
}

fn backfill_missing_checkpoint_shas(
    git_store: &GitStore,
    run_id: &str,
    checkpoints: &mut [(u32, Checkpoint)],
) {
    if !checkpoints
        .iter()
        .any(|(_, checkpoint)| checkpoint.git_commit_sha.is_none())
    {
        return;
    }

    let node_commits = rewind::run_commit_shas_by_node(git_store, run_id);
    let mut node_indices: HashMap<String, usize> = HashMap::new();

    for (_seq, checkpoint) in checkpoints.iter_mut() {
        if checkpoint.git_commit_sha.is_some() {
            continue;
        }

        if let Some(shas) = node_commits.get(&checkpoint.current_node) {
            let idx = node_indices
                .entry(checkpoint.current_node.clone())
                .or_insert(0);
            if *idx < shas.len() {
                checkpoint.git_commit_sha = Some(shas[*idx].clone());
                *idx += 1;
            }
        }
    }
}

fn node_file_path(node_id: &str, visit: u32, filename: &str) -> String {
    if visit <= 1 {
        format!("nodes/{node_id}/{filename}")
    } else {
        format!("nodes/{node_id}-visit_{visit}/{filename}")
    }
}

fn find_run_id_by_prefix_in_refs(repo: &Repository, prefix: &str) -> Result<Option<String>> {
    let refs = repo.references()?;
    let pattern = "refs/heads/fabro/meta/";
    let mut matches = Vec::new();

    for reference in refs.flatten() {
        let Some(name) = reference.name() else {
            continue;
        };
        let Some(run_id) = name.strip_prefix(pattern) else {
            continue;
        };

        if run_id == prefix {
            return Ok(Some(run_id.to_string()));
        }
        if run_id.starts_with(prefix) {
            matches.push(run_id.to_string());
        }
    }

    if matches.is_empty() {
        return Ok(None);
    }

    resolve_prefix_matches(prefix, matches).map(Some)
}

fn canonical_repo_root(repo: &Repository) -> Result<PathBuf> {
    let root = repo
        .workdir()
        .or_else(|| repo.path().parent())
        .unwrap_or(repo.path());
    std::fs::canonicalize(root)
        .with_context(|| format!("failed to canonicalize repo root {}", root.display()))
}

fn resolve_prefix_matches(prefix: &str, matches: Vec<String>) -> Result<String> {
    match matches.len() {
        0 => bail!("no run found matching '{prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let mut msg = format!("ambiguous run ID prefix '{prefix}', matches:\n");
            for run_id in &matches {
                let _ = writeln!(msg, "  {run_id}");
            }
            bail!("{msg}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use fabro_config::FabroSettings;
    use fabro_graphviz::graph::Graph;
    use fabro_store::{InMemoryStore, Store as _};
    use fabro_types::{NodeStatusRecord, RunRecord, SandboxRecord, StageStatus, StartRecord};

    use super::*;
    use crate::operations::test_support::{make_checkpoint_json, temp_repo, test_sig};
    use crate::records::Checkpoint;

    fn created_at() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()
    }

    fn sample_run_record(run_id: &str, host_repo_path: Option<&str>) -> RunRecord {
        RunRecord {
            run_id: run_id.to_string(),
            created_at: created_at(),
            settings: FabroSettings::default(),
            graph: Graph::new("test"),
            workflow_slug: None,
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: host_repo_path.map(ToOwned::to_owned),
            base_branch: None,
            labels: HashMap::new(),
        }
    }

    fn sample_start_record(run_id: &str) -> StartRecord {
        StartRecord {
            run_id: run_id.to_string(),
            start_time: created_at(),
            run_branch: Some(format!("fabro/run/{run_id}")),
            base_sha: Some("base-sha".to_string()),
        }
    }

    fn sample_sandbox_record() -> SandboxRecord {
        SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp/project".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        }
    }

    fn sample_checkpoint(
        current_node: &str,
        completed_nodes: &[&str],
        node_visits: &[(&str, usize)],
        git_commit_sha: Option<&str>,
    ) -> Checkpoint {
        Checkpoint {
            timestamp: created_at(),
            current_node: current_node.to_string(),
            completed_nodes: completed_nodes
                .iter()
                .map(|node| (*node).to_string())
                .collect(),
            node_retries: HashMap::new(),
            context_values: HashMap::new(),
            node_outcomes: HashMap::new(),
            next_node_id: None,
            git_commit_sha: git_commit_sha.map(ToOwned::to_owned),
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: node_visits
                .iter()
                .map(|(node, visit)| ((*node).to_string(), *visit))
                .collect(),
        }
    }

    fn sample_node_status() -> NodeStatusRecord {
        NodeStatusRecord {
            status: StageStatus::Success,
            notes: Some("done".to_string()),
            failure_reason: None,
            timestamp: created_at(),
        }
    }

    async fn create_run_store(
        store: &InMemoryStore,
        run_id: &str,
        host_repo_path: Option<&str>,
    ) -> Arc<dyn DurableRunStore> {
        let run_store = store.create_run(run_id, created_at(), None).await.unwrap();
        run_store
            .put_run(&sample_run_record(run_id, host_repo_path))
            .await
            .unwrap();
        run_store
    }

    fn seed_run_branch(git_store: &GitStore, run_id: &str, nodes: &[&str]) -> Vec<String> {
        let sig = test_sig();
        let run_branch = format!("fabro/run/{run_id}");
        let empty_tree = git_store.write_empty_tree().unwrap();
        let mut shas = Vec::new();
        let mut parent = None;

        for node in nodes {
            let parents = parent.into_iter().collect::<Vec<_>>();
            let oid = git_store
                .write_commit(
                    empty_tree,
                    &parents,
                    &format!("fabro({run_id}): {node} (completed)"),
                    &sig,
                )
                .unwrap();
            git_store.update_ref(&run_branch, oid).unwrap();
            shas.push(oid.to_string());
            parent = Some(oid);
        }

        shas
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_round_trips_timeline() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;
        run_store
            .put_start(&sample_start_record("run-1"))
            .await
            .unwrap();
        run_store
            .put_sandbox(&sample_sandbox_record())
            .await
            .unwrap();

        run_store
            .append_checkpoint(&sample_checkpoint(
                "start",
                &["start"],
                &[("start", 1)],
                Some("aaa"),
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                Some("bbb"),
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 2)],
                Some("ccc"),
            ))
            .await
            .unwrap();

        rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap();

        let timeline = build_timeline(&git_store, "run-1").unwrap();
        assert_eq!(timeline.entries.len(), 3);
        assert_eq!(timeline.entries[0].node_name, "start");
        assert_eq!(timeline.entries[0].visit, 1);
        assert_eq!(timeline.entries[0].run_commit_sha.as_deref(), Some("aaa"));
        assert_eq!(timeline.entries[1].node_name, "build");
        assert_eq!(timeline.entries[1].visit, 1);
        assert_eq!(timeline.entries[1].run_commit_sha.as_deref(), Some("bbb"));
        assert_eq!(timeline.entries[2].node_name, "build");
        assert_eq!(timeline.entries[2].visit, 2);
        assert_eq!(timeline.entries[2].run_commit_sha.as_deref(), Some("ccc"));
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_preserves_historical_node_visits() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;

        let build_v1 = NodeVisitRef {
            node_id: "build",
            visit: 1,
        };
        run_store
            .put_node_prompt(&build_v1, "visit one")
            .await
            .unwrap();
        run_store
            .put_node_status(&build_v1, &sample_node_status())
            .await
            .unwrap();

        let build_v2 = NodeVisitRef {
            node_id: "build",
            visit: 2,
        };
        run_store
            .put_node_prompt(&build_v2, "visit two")
            .await
            .unwrap();

        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["build"],
                &[("build", 1)],
                Some("aaa"),
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["build"],
                &[("build", 2)],
                Some("bbb"),
            ))
            .await
            .unwrap();

        rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap();

        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-1");
        let bs = BranchStore::new(&git_store, &branch, &sig);
        let checkpoint_commits: Vec<_> = bs
            .log(100)
            .unwrap()
            .iter()
            .rev()
            .filter(|commit| commit.message.starts_with("checkpoint"))
            .map(|commit| commit.oid)
            .collect();

        assert_eq!(checkpoint_commits.len(), 2);
        assert_eq!(
            git_store
                .read_blob_at(checkpoint_commits[0], "nodes/build/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit one".as_bytes())
        );
        assert!(
            git_store
                .read_blob_at(checkpoint_commits[0], "nodes/build-visit_2/prompt.md")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            git_store
                .read_blob_at(checkpoint_commits[1], "nodes/build/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit one".as_bytes())
        );
        assert_eq!(
            git_store
                .read_blob_at(checkpoint_commits[1], "nodes/build-visit_2/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit two".as_bytes())
        );
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_refuses_to_overwrite_existing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;

        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-1");
        let bs = BranchStore::new(&git_store, &branch, &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("run.json", b"{}", "init run").unwrap();

        let err = rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("metadata branch already exists"));
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_rebuilds_missing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;
        run_store
            .append_checkpoint(&sample_checkpoint(
                "start",
                &["start"],
                &[("start", 1)],
                Some("aaa"),
            ))
            .await
            .unwrap();

        let timeline = build_timeline_or_rebuild(&git_store, Some(run_store.as_ref()), "run-1")
            .await
            .unwrap();

        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(timeline.entries[0].node_name, "start");
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_preserves_existing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;
        run_store
            .append_checkpoint(&sample_checkpoint(
                "start",
                &["start"],
                &[("start", 1)],
                Some("aaa"),
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                Some("bbb"),
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "test",
                &["start", "build", "test"],
                &[("start", 1), ("build", 1), ("test", 1)],
                Some("ccc"),
            ))
            .await
            .unwrap();

        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-1");
        let bs = BranchStore::new(&git_store, &branch, &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("run.json", b"{}", "init run").unwrap();
        bs.write_entry(
            "checkpoint.json",
            &make_checkpoint_json("start", 1, Some("aaa")),
            "checkpoint",
        )
        .unwrap();
        bs.write_entry(
            "checkpoint.json",
            &make_checkpoint_json("build", 1, Some("bbb")),
            "checkpoint",
        )
        .unwrap();

        let timeline = build_timeline_or_rebuild(&git_store, Some(run_store.as_ref()), "run-1")
            .await
            .unwrap();

        assert_eq!(timeline.entries.len(), 2);
        assert_eq!(timeline.entries[0].node_name, "start");
        assert_eq!(timeline.entries[1].node_name, "build");
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_returns_empty_without_store() {
        let (_dir, git_store) = temp_repo();
        let timeline = build_timeline_or_rebuild(&git_store, None, "run-1")
            .await
            .unwrap();
        assert!(timeline.entries.is_empty());
        assert!(timeline.parallel_map.is_empty());
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_errors_when_run_record_is_missing() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = durable_store
            .create_run("run-1", created_at(), None)
            .await
            .unwrap();

        let err = rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("run record not found"));
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_falls_back_to_store() {
        let (dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let repo_path = dir.path().to_string_lossy().to_string();
        let _run_store = create_run_store(&durable_store, "abc-123-long", Some(&repo_path)).await;

        let run_id = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123")
            .await
            .unwrap();

        assert_eq!(run_id, "abc-123-long");
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_excludes_other_repos() {
        let (_dir, git_store) = temp_repo();
        let (other_dir, _other_git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let other_repo_path = other_dir.path().to_string_lossy().to_string();
        let _run_store =
            create_run_store(&durable_store, "abc-123-long", Some(&other_repo_path)).await;

        let err = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no run found matching"));
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_requires_exact_match_without_repo_path() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let _run_store = create_run_store(&durable_store, "abc-123-long", None).await;

        let prefix_err =
            find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123")
                .await
                .unwrap_err();
        assert!(prefix_err.to_string().contains("no run found matching"));

        let exact =
            find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123-long")
                .await
                .unwrap();
        assert_eq!(exact, "abc-123-long");
    }

    #[tokio::test]
    async fn exact_match_wins_over_prefix_ambiguity() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let repo_path = git_store.repo_dir().to_string_lossy().to_string();
        let _short = create_run_store(&durable_store, "abc-123", Some(&repo_path)).await;
        let _long = create_run_store(&durable_store, "abc-123-long", Some(&repo_path)).await;

        let from_store =
            find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123")
                .await
                .unwrap();
        assert_eq!(from_store, "abc-123");

        let sig = test_sig();
        let short_branch =
            BranchStore::new(&git_store, MetadataStore::branch_name("abc-123"), &sig);
        short_branch.ensure_branch().unwrap();

        let long_branch =
            BranchStore::new(&git_store, MetadataStore::branch_name("abc-123-long"), &sig);
        long_branch.ensure_branch().unwrap();

        let from_refs = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, "abc-123")
            .await
            .unwrap();
        assert_eq!(from_refs, "abc-123");
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_persists_backfilled_run_shas_in_checkpoint_blobs() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;

        run_store
            .append_checkpoint(&sample_checkpoint(
                "start",
                &["start"],
                &[("start", 1)],
                None,
            ))
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                None,
            ))
            .await
            .unwrap();

        let expected_shas = seed_run_branch(&git_store, "run-1", &["start", "build"]);

        rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap();

        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-1");
        let bs = BranchStore::new(&git_store, &branch, &sig);
        let checkpoint_commits: Vec<_> = bs
            .log(100)
            .unwrap()
            .iter()
            .rev()
            .filter(|commit| commit.message.starts_with("checkpoint"))
            .map(|commit| commit.oid)
            .collect();

        let first: Checkpoint = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[0], "checkpoint.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let second: Checkpoint = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[1], "checkpoint.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            first.git_commit_sha.as_deref(),
            Some(expected_shas[0].as_str())
        );
        assert_eq!(
            second.git_commit_sha.as_deref(),
            Some(expected_shas[1].as_str())
        );
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_is_atomic_on_failure() {
        let (_dir, git_store) = temp_repo();
        let durable_store = InMemoryStore::default();
        let run_store = create_run_store(&durable_store, "run-1", None).await;

        let bad_node = "bad\0node";
        let bad_visit = NodeVisitRef {
            node_id: bad_node,
            visit: 1,
        };
        run_store
            .put_node_prompt(&bad_visit, "prompt")
            .await
            .unwrap();
        run_store
            .append_checkpoint(&sample_checkpoint(
                bad_node,
                &[bad_node],
                &[(bad_node, 1)],
                None,
            ))
            .await
            .unwrap();

        let err = rebuild_metadata_branch(&git_store, run_store.as_ref(), "run-1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nul") || err.to_string().contains("NUL"));
        assert!(
            git_store
                .resolve_ref(&MetadataStore::branch_name("run-1"))
                .unwrap()
                .is_none()
        );

        let scratch_refs: Vec<_> = git_store
            .repo()
            .references()
            .unwrap()
            .flatten()
            .filter_map(|reference| reference.name().map(ToOwned::to_owned))
            .filter(|name| name.starts_with("refs/heads/fabro/meta-rebuild/run-1/"))
            .collect();
        assert!(
            scratch_refs.is_empty(),
            "leftover scratch refs: {scratch_refs:?}"
        );
    }
}
