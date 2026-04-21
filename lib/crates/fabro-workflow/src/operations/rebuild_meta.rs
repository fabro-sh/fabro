use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fabro_checkpoint::branch::BranchStore;
use fabro_checkpoint::git::Store as GitStore;
use fabro_store::{Database as DurableStore, RunDatabase as DurableRunStore, RunProjection};
use fabro_types::{EventBody, RunEvent, RunId};
use git2::{Repository, Signature};
use tokio::task::spawn_blocking;
use ulid::Ulid;

use super::rewind::{self, RunTimeline, build_timeline};
use crate::git::MetadataStore;
use crate::records::Checkpoint;
use crate::run_dump::RunDump;

pub async fn rebuild_metadata_branch(
    git_store: &GitStore,
    run_store: &DurableRunStore,
    run_id: &RunId,
) -> Result<()> {
    let branch = MetadataStore::branch_name(&run_id.to_string());
    if git_store.resolve_ref(&branch)?.is_some() {
        bail!("metadata branch already exists for run {run_id}");
    }

    let events = run_store.list_events().await?;
    if events.is_empty() {
        bail!("run spec not found for {run_id}");
    }

    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;
    let scratch_branch = format!("fabro/meta-rebuild/{run_id}/{}", Ulid::new());
    let bs = BranchStore::new(git_store, &scratch_branch, &sig);

    let result = async {
        bs.ensure_branch()?;
        let mut projection = RunProjection::default();
        let mut latest_init_snapshot = None;
        let mut init_written = false;
        let mut checkpoint_snapshots: Vec<(u32, RunProjection)> = Vec::new();

        for event in &events {
            let stored = RunEvent::from_ref(event.payload.as_value())
                .map_err(|err| anyhow::anyhow!("invalid stored event: {err}"))?;
            let is_checkpoint = matches!(stored.body, EventBody::CheckpointCompleted(_));

            if !is_checkpoint && projection.spec.is_some() {
                latest_init_snapshot = Some(projection.clone());
            }

            projection.apply_event(event)?;

            if is_checkpoint {
                if !init_written {
                    let init_snapshot = latest_init_snapshot.clone().unwrap_or_else(|| {
                        let mut snapshot = projection.clone();
                        snapshot.checkpoint = None;
                        snapshot.checkpoints.clear();
                        snapshot
                    });
                    write_projection_snapshot(&bs, &init_snapshot, "init run")?;
                    init_written = true;
                }
                checkpoint_snapshots.push((event.seq, projection.clone()));
            }
        }

        if projection.spec.is_none() {
            bail!("run spec not found for {run_id}");
        }

        if !init_written {
            write_projection_snapshot(&bs, &projection, "init run")?;
        }

        let mut checkpoints: Vec<(u32, Checkpoint)> = checkpoint_snapshots
            .iter()
            .map(|(seq, snapshot)| {
                let checkpoint = snapshot
                    .checkpoint
                    .clone()
                    .expect("checkpoint snapshots must include projection.checkpoint");
                (*seq, checkpoint)
            })
            .collect();
        backfill_missing_checkpoint_shas(git_store, run_id, &mut checkpoints);

        for ((_, snapshot), (_, checkpoint)) in checkpoint_snapshots.iter_mut().zip(checkpoints) {
            snapshot.checkpoint = Some(checkpoint);
            write_projection_snapshot(&bs, snapshot, "checkpoint")?;
        }

        if projection.conclusion.is_some()
            || projection.retro.is_some()
            || projection.retro_prompt.is_some()
            || projection.retro_response.is_some()
        {
            write_projection_snapshot(&bs, &projection, "finalize run")?;
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
    run_store: Option<&DurableRunStore>,
    run_id: &RunId,
) -> Result<RunTimeline> {
    let branch = MetadataStore::branch_name(&run_id.to_string());
    if git_store.resolve_ref(&branch)?.is_some() {
        return build_timeline(git_store, &run_id.to_string());
    }

    if let Some(run_store) = run_store {
        rebuild_metadata_branch(git_store, run_store, run_id).await?;
        return build_timeline(git_store, &run_id.to_string());
    }

    Ok(RunTimeline {
        entries:      Vec::new(),
        parallel_map: HashMap::new(),
    })
}

pub async fn find_run_id_by_prefix_or_store(
    repo: &Repository,
    fabro_store: &DurableStore,
    prefix: &str,
) -> Result<RunId> {
    if let Some(run_id) = find_run_id_by_prefix_in_refs(repo, prefix)? {
        return Ok(run_id);
    }

    let current_repo_root = {
        let repo_root = repo_root_path(repo);
        spawn_blocking(move || canonical_repo_root(&repo_root))
            .await
            .map_err(|err| anyhow::anyhow!("repo root canonicalize task failed: {err}"))??
    };
    let mut matches = Vec::new();
    for summary in fabro_store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await?
    {
        if summary.run_id.to_string() == prefix {
            if summary.host_repo_path.is_none() {
                return Ok(summary.run_id);
            }

            let Some(host_repo_path) = summary.host_repo_path.as_deref() else {
                continue;
            };
            let host_repo_path = host_repo_path.to_string();
            let host_repo_root =
                match spawn_blocking(move || canonical_repo_root_for_path(&host_repo_path)).await {
                    Ok(Ok(root)) => root,
                    Ok(Err(_)) => continue,
                    Err(err) => {
                        return Err(anyhow::anyhow!("host repo canonicalize task failed: {err}"));
                    }
                };
            if host_repo_root == current_repo_root {
                return Ok(summary.run_id);
            }
            continue;
        }

        let Some(host_repo_path) = summary.host_repo_path.as_deref() else {
            continue;
        };
        let host_repo_path = host_repo_path.to_string();
        let host_repo_root =
            match spawn_blocking(move || canonical_repo_root_for_path(&host_repo_path)).await {
                Ok(Ok(root)) => root,
                Ok(Err(_)) => continue,
                Err(err) => {
                    return Err(anyhow::anyhow!("host repo canonicalize task failed: {err}"));
                }
            };
        if host_repo_root == current_repo_root && summary.run_id.to_string().starts_with(prefix) {
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

fn write_projection_snapshot(
    branch_store: &BranchStore<'_>,
    projection: &RunProjection,
    message: &str,
) -> Result<()> {
    let entries = RunDump::from_projection(projection)
        .git_entries()
        .context("failed to serialize metadata projection snapshot")?;
    write_entries(branch_store, &entries, message)
}

fn backfill_missing_checkpoint_shas(
    git_store: &GitStore,
    run_id: &RunId,
    checkpoints: &mut [(u32, Checkpoint)],
) {
    if !checkpoints
        .iter()
        .any(|(_, checkpoint)| checkpoint.git_commit_sha.is_none())
    {
        return;
    }

    let node_commits = rewind::run_commit_shas_by_node(git_store, &run_id.to_string());
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

fn find_run_id_by_prefix_in_refs(repo: &Repository, prefix: &str) -> Result<Option<RunId>> {
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
        let Ok(run_id) = run_id.parse::<RunId>() else {
            continue;
        };

        if run_id.to_string() == prefix {
            return Ok(Some(run_id));
        }
        if run_id.to_string().starts_with(prefix) {
            matches.push(run_id);
        }
    }

    if matches.is_empty() {
        return Ok(None);
    }

    resolve_prefix_matches(prefix, matches).map(Some)
}

fn repo_root_path(repo: &Repository) -> PathBuf {
    repo.workdir()
        .or_else(|| repo.path().parent())
        .unwrap_or(repo.path())
        .to_path_buf()
}

#[expect(
    clippy::disallowed_methods,
    reason = "sync repo-root canonicalize helper; async callers wrap it in spawn_blocking"
)]
fn canonical_repo_root(root: &std::path::Path) -> Result<PathBuf> {
    std::fs::canonicalize(root)
        .with_context(|| format!("failed to canonicalize repo root {}", root.display()))
}

fn canonical_repo_root_for_path(path: &str) -> Result<PathBuf> {
    let repo = Repository::discover(path)?;
    let root = repo_root_path(&repo);
    canonical_repo_root(&root)
}

fn resolve_prefix_matches(prefix: &str, matches: Vec<RunId>) -> Result<RunId> {
    match matches.len() {
        0 => bail!("no run found matching '{prefix}'"),
        1 => Ok(matches
            .into_iter()
            .next()
            .expect("exactly one run should match when len is 1")),
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
    use std::time::Duration;

    use chrono::{TimeZone, Utc};
    use fabro_graphviz::graph::Graph;
    use fabro_store::{Database, RunProjection, StageId};
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{RunId, RunSpec, SandboxRecord, StartRecord, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::{Event, append_event};
    use crate::operations::test_support::{temp_repo, test_sig};
    use crate::records::Checkpoint;

    fn created_at() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()
    }

    fn parse_run_id(value: &str) -> RunId {
        value.parse().unwrap()
    }

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    fn sample_run_spec(run_id: RunId, host_repo_path: Option<&str>) -> RunSpec {
        RunSpec {
            run_id,
            settings: SettingsLayer::default(),
            graph: Graph::new("test"),
            workflow_slug: None,
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: host_repo_path.map(ToOwned::to_owned),
            repo_origin_url: None,
            base_branch: None,
            labels: HashMap::new(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
        }
    }

    fn sample_start_record(run_id: RunId) -> StartRecord {
        StartRecord {
            run_id,
            start_time: created_at(),
            run_branch: Some(format!("fabro/run/{run_id}")),
            base_sha: Some("base-sha".to_string()),
        }
    }

    fn sample_sandbox_record() -> SandboxRecord {
        SandboxRecord {
            provider:               "local".to_string(),
            working_directory:      "/tmp/project".to_string(),
            identifier:             None,
            host_working_directory: None,
            container_mount_point:  None,
        }
    }

    fn sample_checkpoint(
        current_node: &str,
        completed_nodes: &[&str],
        node_visits: &[(&str, usize)],
        git_commit_sha: Option<&str>,
    ) -> Checkpoint {
        Checkpoint {
            timestamp:                  created_at(),
            current_node:               current_node.to_string(),
            completed_nodes:            completed_nodes
                .iter()
                .map(|node| (*node).to_string())
                .collect(),
            node_retries:               HashMap::new(),
            context_values:             HashMap::new(),
            node_outcomes:              HashMap::new(),
            next_node_id:               None,
            git_commit_sha:             git_commit_sha.map(ToOwned::to_owned),
            loop_failure_signatures:    HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits:                node_visits
                .iter()
                .map(|(node, visit)| ((*node).to_string(), *visit))
                .collect(),
        }
    }

    async fn create_run_store(
        store: &Database,
        run_id: RunId,
        host_repo_path: Option<&str>,
    ) -> DurableRunStore {
        let run_store = store.create_run(&run_id).await.unwrap();
        let run_spec = sample_run_spec(run_id, host_repo_path);
        append_event(&run_store, &run_id, &Event::RunCreated {
            run_id,
            settings: serde_json::to_value(&run_spec.settings).unwrap(),
            graph: serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: run_spec.labels.clone().into_iter().collect(),
            run_dir: String::new(),
            working_directory: run_spec.working_directory.display().to_string(),
            host_repo_path: run_spec.host_repo_path.clone(),
            repo_origin_url: run_spec.repo_origin_url.clone(),
            base_branch: run_spec.base_branch.clone(),
            workflow_slug: run_spec.workflow_slug.clone(),
            db_prefix: None,
            provenance: run_spec.provenance.clone(),
            manifest_blob: None,
        })
        .await
        .unwrap();
        run_store
    }

    async fn append_start_event(run_store: &DurableRunStore, run_id: RunId) {
        let start = sample_start_record(run_id);
        append_event(run_store, &run_id, &Event::WorkflowRunStarted {
            name: "test".to_string(),
            run_id,
            base_branch: None,
            base_sha: start.base_sha,
            run_branch: start.run_branch,
            worktree_dir: None,
            goal: None,
        })
        .await
        .unwrap();
    }

    async fn append_sandbox_event(run_store: &DurableRunStore, run_id: RunId) {
        let sandbox = sample_sandbox_record();
        append_event(run_store, &run_id, &Event::SandboxInitialized {
            provider:               sandbox.provider,
            working_directory:      sandbox.working_directory,
            identifier:             sandbox.identifier,
            host_working_directory: sandbox.host_working_directory,
            container_mount_point:  sandbox.container_mount_point,
        })
        .await
        .unwrap();
    }

    async fn append_checkpoint_event(
        run_store: &DurableRunStore,
        run_id: RunId,
        checkpoint: Checkpoint,
    ) {
        append_event(run_store, &run_id, &Event::CheckpointCompleted {
            node_id: checkpoint.current_node.clone(),
            status: "success".to_string(),
            current_node: checkpoint.current_node.clone(),
            completed_nodes: checkpoint.completed_nodes.clone(),
            node_retries: checkpoint.node_retries.clone().into_iter().collect(),
            context_values: checkpoint.context_values.clone().into_iter().collect(),
            node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
            next_node_id: checkpoint.next_node_id.clone(),
            git_commit_sha: checkpoint.git_commit_sha.clone(),
            loop_failure_signatures: checkpoint
                .loop_failure_signatures
                .clone()
                .into_iter()
                .map(|(signature, count)| (signature.to_string(), count))
                .collect(),
            restart_failure_signatures: checkpoint
                .restart_failure_signatures
                .clone()
                .into_iter()
                .map(|(signature, count)| (signature.to_string(), count))
                .collect(),
            node_visits: checkpoint.node_visits.clone().into_iter().collect(),
            diff: None,
        })
        .await
        .unwrap();
    }

    async fn append_prompt_event(
        run_store: &DurableRunStore,
        run_id: RunId,
        node: &StageId,
        text: &str,
    ) {
        append_event(run_store, &run_id, &Event::Prompt {
            stage:    node.node_id().to_string(),
            visit:    node.visit(),
            text:     text.to_string(),
            mode:     None,
            provider: None,
            model:    None,
        })
        .await
        .unwrap();
    }

    fn seed_run_branch(git_store: &GitStore, run_id: RunId, nodes: &[&str]) -> Vec<String> {
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
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;
        append_start_event(&run_store, test_run_id()).await;
        append_sandbox_event(&run_store, test_run_id()).await;

        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("start", &["start"], &[("start", 1)], Some("aaa")),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                Some("bbb"),
            ),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 2)],
                Some("ccc"),
            ),
        )
        .await;

        rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap();

        let timeline = build_timeline(&git_store, &test_run_id().to_string()).unwrap();
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
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;

        let build_v1 = StageId::new("build", 1);
        append_prompt_event(&run_store, test_run_id(), &build_v1, "visit one").await;

        let build_v2 = StageId::new("build", 2);
        append_prompt_event(&run_store, test_run_id(), &build_v2, "visit two").await;

        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("build", &["build"], &[("build", 1)], Some("aaa")),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("build", &["build"], &[("build", 2)], Some("bbb")),
        )
        .await;

        rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap();

        let sig = test_sig();
        let branch = MetadataStore::branch_name(&test_run_id().to_string());
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
                .read_blob_at(checkpoint_commits[0], "stages/build@1/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit one".as_bytes())
        );
        let first_projection: RunProjection = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[0], "run.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            first_projection
                .checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.node_visits.get("build"))
                .copied(),
            Some(1)
        );
        assert_eq!(
            git_store
                .read_blob_at(checkpoint_commits[1], "stages/build@1/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit one".as_bytes())
        );
        assert_eq!(
            git_store
                .read_blob_at(checkpoint_commits[1], "stages/build@2/prompt.md")
                .unwrap()
                .as_deref(),
            Some("visit two".as_bytes())
        );
        let second_projection: RunProjection = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[1], "run.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            second_projection
                .checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.node_visits.get("build"))
                .copied(),
            Some(2)
        );
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_refuses_to_overwrite_existing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;

        let sig = test_sig();
        let branch = MetadataStore::branch_name(&test_run_id().to_string());
        let bs = BranchStore::new(&git_store, &branch, &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("run.json", b"{}", "init run").unwrap();

        let err = rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("metadata branch already exists"));
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_rebuilds_missing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("start", &["start"], &[("start", 1)], Some("aaa")),
        )
        .await;

        let timeline = build_timeline_or_rebuild(&git_store, Some(&run_store), &test_run_id())
            .await
            .unwrap();

        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(timeline.entries[0].node_name, "start");
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_preserves_existing_branch() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("start", &["start"], &[("start", 1)], Some("aaa")),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                Some("bbb"),
            ),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(
                "test",
                &["start", "build", "test"],
                &[("start", 1), ("build", 1), ("test", 1)],
                Some("ccc"),
            ),
        )
        .await;

        let sig = test_sig();
        let branch = MetadataStore::branch_name(&test_run_id().to_string());
        let bs = BranchStore::new(&git_store, &branch, &sig);
        bs.ensure_branch().unwrap();
        let mut init_projection = RunProjection::default();
        init_projection.spec = Some(sample_run_spec(test_run_id(), None));
        init_projection.start = Some(sample_start_record(test_run_id()));
        bs.write_entry(
            "run.json",
            &serde_json::to_vec_pretty(&init_projection).unwrap(),
            "init run",
        )
        .unwrap();

        let mut first_checkpoint_projection = init_projection.clone();
        first_checkpoint_projection.checkpoint = Some(sample_checkpoint(
            "start",
            &["start"],
            &[("start", 1)],
            Some("aaa"),
        ));
        bs.write_entry(
            "run.json",
            &serde_json::to_vec_pretty(&first_checkpoint_projection).unwrap(),
            "checkpoint",
        )
        .unwrap();

        let mut second_checkpoint_projection = init_projection;
        second_checkpoint_projection.checkpoint = Some(sample_checkpoint(
            "build",
            &["start", "build"],
            &[("start", 1), ("build", 1)],
            Some("bbb"),
        ));
        bs.write_entry(
            "run.json",
            &serde_json::to_vec_pretty(&second_checkpoint_projection).unwrap(),
            "checkpoint",
        )
        .unwrap();

        let timeline = build_timeline_or_rebuild(&git_store, Some(&run_store), &test_run_id())
            .await
            .unwrap();

        assert_eq!(timeline.entries.len(), 2);
        assert_eq!(timeline.entries[0].node_name, "start");
        assert_eq!(timeline.entries[1].node_name, "build");
    }

    #[tokio::test]
    async fn build_timeline_or_rebuild_returns_empty_without_store() {
        let (_dir, git_store) = temp_repo();
        let timeline = build_timeline_or_rebuild(&git_store, None, &test_run_id())
            .await
            .unwrap();
        assert!(timeline.entries.is_empty());
        assert!(timeline.parallel_map.is_empty());
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_errors_when_run_spec_is_missing() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let run_store = durable_store.create_run(&test_run_id()).await.unwrap();

        let err = rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("run spec not found"));
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_falls_back_to_store() {
        let (dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let repo_path = dir.path().to_string_lossy().to_string();
        let repo_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let _run_store = create_run_store(&durable_store, repo_run_id, Some(&repo_path)).await;
        let prefix = &repo_run_id.to_string()[..6];

        let run_id = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, prefix)
            .await
            .unwrap();

        assert_eq!(run_id, repo_run_id);
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_excludes_other_repos() {
        let (_dir, git_store) = temp_repo();
        let (other_dir, _other_git_store) = temp_repo();
        let durable_store = memory_store();
        let other_repo_path = other_dir.path().to_string_lossy().to_string();
        let other_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let _run_store =
            create_run_store(&durable_store, other_run_id, Some(&other_repo_path)).await;
        let prefix = &other_run_id.to_string()[..6];

        let err = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, prefix)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no run found matching"));
    }

    #[tokio::test]
    async fn find_run_id_by_prefix_or_store_requires_exact_match_without_repo_path() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let repo_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let _run_store = create_run_store(&durable_store, repo_run_id, None).await;
        let prefix = &repo_run_id.to_string()[..6];

        let prefix_err = find_run_id_by_prefix_or_store(git_store.repo(), &durable_store, prefix)
            .await
            .unwrap_err();
        assert!(prefix_err.to_string().contains("no run found matching"));

        let exact = find_run_id_by_prefix_or_store(
            git_store.repo(),
            &durable_store,
            &repo_run_id.to_string(),
        )
        .await
        .unwrap();
        assert_eq!(exact, repo_run_id);
    }

    #[tokio::test]
    async fn exact_match_wins_over_prefix_ambiguity() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let repo_path = git_store.repo_dir().to_string_lossy().to_string();
        let exact_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let other_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAW");
        let _exact = create_run_store(&durable_store, exact_run_id, Some(&repo_path)).await;
        let _other = create_run_store(&durable_store, other_run_id, Some(&repo_path)).await;

        let from_store = find_run_id_by_prefix_or_store(
            git_store.repo(),
            &durable_store,
            &exact_run_id.to_string(),
        )
        .await
        .unwrap();
        assert_eq!(from_store, exact_run_id);

        let sig = test_sig();
        let exact_branch = BranchStore::new(
            &git_store,
            &MetadataStore::branch_name(&exact_run_id.to_string()),
            &sig,
        );
        exact_branch.ensure_branch().unwrap();

        let other_branch = BranchStore::new(
            &git_store,
            &MetadataStore::branch_name(&other_run_id.to_string()),
            &sig,
        );
        other_branch.ensure_branch().unwrap();

        let from_refs = find_run_id_by_prefix_or_store(
            git_store.repo(),
            &durable_store,
            &exact_run_id.to_string(),
        )
        .await
        .unwrap();
        assert_eq!(from_refs, exact_run_id);
    }

    #[tokio::test]
    async fn rebuild_metadata_branch_persists_backfilled_run_shas_in_checkpoint_blobs() {
        let (_dir, git_store) = temp_repo();
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;

        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint("start", &["start"], &[("start", 1)], None),
        )
        .await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(
                "build",
                &["start", "build"],
                &[("start", 1), ("build", 1)],
                None,
            ),
        )
        .await;

        let expected_shas = seed_run_branch(&git_store, test_run_id(), &["start", "build"]);

        rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap();

        let sig = test_sig();
        let branch = MetadataStore::branch_name(&test_run_id().to_string());
        let bs = BranchStore::new(&git_store, &branch, &sig);
        let checkpoint_commits: Vec<_> = bs
            .log(100)
            .unwrap()
            .iter()
            .rev()
            .filter(|commit| commit.message.starts_with("checkpoint"))
            .map(|commit| commit.oid)
            .collect();

        let first_projection: RunProjection = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[0], "run.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let second_projection: RunProjection = serde_json::from_slice(
            &git_store
                .read_blob_at(checkpoint_commits[1], "run.json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let first = first_projection.checkpoint.unwrap();
        let second = second_projection.checkpoint.unwrap();

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
        let durable_store = memory_store();
        let run_store = create_run_store(&durable_store, test_run_id(), None).await;

        let bad_node = "bad\0node";
        let bad_visit = StageId::new(bad_node, 1);
        append_prompt_event(&run_store, test_run_id(), &bad_visit, "prompt").await;
        append_checkpoint_event(
            &run_store,
            test_run_id(),
            sample_checkpoint(bad_node, &[bad_node], &[(bad_node, 1)], None),
        )
        .await;

        let err = rebuild_metadata_branch(&git_store, &run_store, &test_run_id())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nul") || err.to_string().contains("NUL"));
        assert!(
            git_store
                .resolve_ref(&MetadataStore::branch_name(&test_run_id().to_string()))
                .unwrap()
                .is_none()
        );

        let scratch_refs: Vec<_> = git_store
            .repo()
            .references()
            .unwrap()
            .flatten()
            .filter_map(|reference| reference.name().map(ToOwned::to_owned))
            .filter(|name| {
                name.starts_with(&format!("refs/heads/fabro/meta-rebuild/{}/", test_run_id()))
            })
            .collect();
        assert!(
            scratch_refs.is_empty(),
            "leftover scratch refs: {scratch_refs:?}"
        );
    }
}
