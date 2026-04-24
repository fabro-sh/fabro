use anyhow::{Context, Result};
use fabro_checkpoint::branch::BranchStore;
use fabro_checkpoint::git::Store;
use fabro_store::{Database, RunProjection};
use fabro_types::RunId;
use git2::{Oid, Signature};

use super::run_git;
use super::timeline::{ForkTarget, RunTimeline, TimelineEntry, build_timeline};
use crate::error::Error;
use crate::event::{self, Event};
use crate::git::{MetadataStore, RUN_BRANCH_PREFIX, push_run_branches};
use crate::records::{Checkpoint, RunSpec, StartRecord};
use crate::run_dump::RunDump;

#[derive(Debug, Clone)]
pub struct ForkRunInput {
    pub source_run_id: RunId,
    pub target:        Option<ForkTarget>,
    pub push:          bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedForkTarget {
    pub checkpoint_ordinal: usize,
    pub node_id:            String,
    pub visit:              usize,
}

impl ResolvedForkTarget {
    #[must_use]
    pub fn response_target(&self) -> String {
        format!("@{}", self.checkpoint_ordinal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub source_run_id: RunId,
    pub new_run_id:    RunId,
    pub target:        ResolvedForkTarget,
}

#[derive(Debug)]
struct ForkedRun {
    new_run_id: RunId,
    projection: RunProjection,
}

/// Create a new run that branches from an existing run at a specific
/// checkpoint.
///
/// Returns the new run ID.
#[cfg(test)]
fn fork(store: &Store, input: &ForkRunInput) -> Result<RunId> {
    let timeline = build_timeline(store, &input.source_run_id.to_string())?;
    let entry = resolve_fork_entry(&timeline, &input.source_run_id, input.target.as_ref())?;
    Ok(fork_from_entry(store, &input.source_run_id, entry, input.push)?.new_run_id)
}

pub async fn fork_run(store: &Database, input: &ForkRunInput) -> Result<ForkOutcome, Error> {
    let source_run_id = input.source_run_id;
    let target = input.target.clone();
    let push = input.push;

    let (outcome, projection) =
        run_git::with_run_git_store(store, source_run_id, move |git_store| {
            let timeline = build_timeline(&git_store, &source_run_id.to_string())
                .map_err(|err| Error::engine(err.to_string()))?;
            let entry = resolve_fork_entry(&timeline, &source_run_id, target.as_ref())
                .map_err(|err| Error::Validation(err.to_string()))?;
            let resolved = ResolvedForkTarget {
                checkpoint_ordinal: entry.ordinal,
                node_id:            entry.node_name.clone(),
                visit:              entry.visit,
            };
            let forked = fork_from_entry(&git_store, &source_run_id, entry, push)
                .map_err(|err| Error::engine(err.to_string()))?;
            let outcome = ForkOutcome {
                source_run_id,
                new_run_id: forked.new_run_id,
                target: resolved,
            };
            Ok((outcome, forked.projection))
        })
        .await?;

    persist_forked_run(store, &projection).await?;

    Ok(outcome)
}

fn resolve_fork_entry<'a>(
    timeline: &'a RunTimeline,
    source_run_id: &RunId,
    target: Option<&ForkTarget>,
) -> Result<&'a TimelineEntry> {
    match target {
        Some(target) => timeline.resolve(target),
        None => timeline
            .entries
            .last()
            .ok_or_else(|| anyhow::anyhow!("no checkpoints found for run {source_run_id}")),
    }
}

fn fork_from_entry(
    store: &Store,
    source_run_id: &RunId,
    entry: &TimelineEntry,
    push: bool,
) -> Result<ForkedRun> {
    let new_run_id = RunId::new();
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;

    let new_run_branch = format!("{RUN_BRANCH_PREFIX}{new_run_id}");
    match &entry.run_commit_sha {
        Some(sha) => {
            let oid =
                Oid::from_str(sha).with_context(|| format!("invalid run commit SHA: {sha}"))?;
            store
                .update_ref(&new_run_branch, oid)
                .map_err(|e| anyhow::anyhow!("failed to create run branch ref: {e}"))?;
        }
        None => {
            anyhow::bail!(
                "checkpoint @{} has no git_commit_sha; cannot fork",
                entry.ordinal
            );
        }
    }

    let source_meta_branch = MetadataStore::branch_name(&source_run_id.to_string());
    let new_meta_branch = MetadataStore::branch_name(&new_run_id.to_string());
    let source_bs = BranchStore::new(store, &source_meta_branch, &sig);
    let new_bs = BranchStore::new(store, &new_meta_branch, &sig);

    new_bs
        .ensure_branch()
        .map_err(|e| anyhow::anyhow!("failed to create metadata branch: {e}"))?;

    let source_projection = source_bs
        .read_entry("run.json")
        .map_err(|e| anyhow::anyhow!("failed to read source metadata: {e}"))?
        .context("source run has no run.json")
        .and_then(|bytes| {
            serde_json::from_slice::<RunProjection>(&bytes)
                .context("failed to parse source run.json")
        })?;

    let mut run_spec: RunSpec = source_projection
        .spec
        .clone()
        .context("source run projection has no spec")?;
    run_spec.run_id = new_run_id;

    let start_record = StartRecord {
        run_id:     new_run_id,
        start_time: new_run_id.created_at(),
        run_branch: Some(new_run_branch.clone()),
        base_sha:   entry.run_commit_sha.clone(),
    };

    let mut init_projection = RunProjection::default();
    init_projection.spec = Some(run_spec.clone());
    init_projection
        .graph_source
        .clone_from(&source_projection.graph_source);
    init_projection.start = Some(start_record.clone());
    init_projection
        .sandbox
        .clone_from(&source_projection.sandbox);

    let checkpoint_bytes = store
        .read_blob_at(entry.metadata_commit_oid, "run.json")
        .map_err(|e| anyhow::anyhow!("failed to read checkpoint snapshot: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no run.json at metadata commit {}",
                entry.metadata_commit_oid
            )
        })?;
    let mut checkpoint_projection: RunProjection = serde_json::from_slice(&checkpoint_bytes)
        .context("failed to parse source checkpoint snapshot")?;
    let mut checkpoint: Checkpoint = checkpoint_projection
        .checkpoint
        .clone()
        .context("source checkpoint snapshot has no checkpoint")?;
    checkpoint.git_commit_sha.clone_from(&entry.run_commit_sha);
    checkpoint_projection.spec = Some(run_spec);
    checkpoint_projection.graph_source = source_projection.graph_source;
    checkpoint_projection.start = Some(start_record);
    checkpoint_projection.sandbox = source_projection.sandbox;
    checkpoint_projection.checkpoint = Some(checkpoint);

    let init_entries = RunDump::from_projection(&init_projection)
        .git_entries()
        .context("failed to build init metadata snapshot")?;
    let init_refs: Vec<(&str, &[u8])> = init_entries
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    new_bs
        .write_entries(&init_refs, "init run")
        .map_err(|e| anyhow::anyhow!("failed to write init metadata snapshot: {e}"))?;

    let checkpoint_entries = RunDump::from_projection(&checkpoint_projection)
        .git_entries()
        .context("failed to build checkpoint metadata snapshot")?;
    let checkpoint_refs: Vec<(&str, &[u8])> = checkpoint_entries
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    new_bs
        .write_entries(&checkpoint_refs, "checkpoint")
        .map_err(|e| anyhow::anyhow!("failed to write metadata snapshot: {e}"))?;

    if push {
        let source_run_branch = format!("{RUN_BRANCH_PREFIX}{source_run_id}");
        let run_refspec = format!("refs/heads/{new_run_branch}:refs/heads/{new_run_branch}");
        let meta_refspec = format!("refs/heads/{new_meta_branch}:refs/heads/{new_meta_branch}");
        push_run_branches(
            store,
            &source_run_branch,
            Some(&run_refspec),
            &meta_refspec,
            "new",
        )?;
    }

    Ok(ForkedRun {
        new_run_id,
        projection: checkpoint_projection,
    })
}

async fn persist_forked_run(store: &Database, projection: &RunProjection) -> Result<(), Error> {
    let spec = projection
        .spec
        .as_ref()
        .ok_or_else(|| Error::engine("forked run projection has no spec"))?;
    let start = projection
        .start
        .as_ref()
        .ok_or_else(|| Error::engine("forked run projection has no start record"))?;
    let checkpoint = projection
        .checkpoint
        .as_ref()
        .ok_or_else(|| Error::engine("forked run projection has no checkpoint"))?;

    let run_store = store
        .create_run(&spec.run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    event::append_event(&run_store, &spec.run_id, &Event::RunCreated {
        run_id:            spec.run_id,
        settings:          serde_json::to_value(&spec.settings)
            .map_err(|err| Error::engine(err.to_string()))?,
        graph:             serde_json::to_value(&spec.graph)
            .map_err(|err| Error::engine(err.to_string()))?,
        workflow_source:   projection.graph_source.clone(),
        workflow_config:   None,
        labels:            spec.labels.clone().into_iter().collect(),
        run_dir:           String::new(),
        working_directory: spec.working_directory.display().to_string(),
        host_repo_path:    spec.host_repo_path.clone(),
        repo_origin_url:   spec.repo_origin_url.clone(),
        base_branch:       spec.base_branch.clone(),
        workflow_slug:     spec.workflow_slug.clone(),
        db_prefix:         None,
        provenance:        spec.provenance.clone(),
        manifest_blob:     spec.manifest_blob,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    event::append_event(&run_store, &spec.run_id, &Event::WorkflowRunStarted {
        name:         spec.graph.name.clone(),
        run_id:       spec.run_id,
        base_branch:  spec.base_branch.clone(),
        base_sha:     start.base_sha.clone(),
        run_branch:   start.run_branch.clone(),
        worktree_dir: None,
        goal:         None,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    if let Some(sandbox) = projection.sandbox.as_ref() {
        event::append_event(&run_store, &spec.run_id, &Event::SandboxInitialized {
            provider:               sandbox.provider.clone(),
            working_directory:      sandbox.working_directory.clone(),
            identifier:             sandbox.identifier.clone(),
            host_working_directory: sandbox.host_working_directory.clone(),
            container_mount_point:  sandbox.container_mount_point.clone(),
        })
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    }

    event::append_event(
        &run_store,
        &spec.run_id,
        &checkpoint_completed_event(checkpoint),
    )
    .await
    .map_err(|err| Error::engine(err.to_string()))?;
    event::append_event(&run_store, &spec.run_id, &Event::RunSubmitted {
        definition_blob: spec.definition_blob,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))
}

fn checkpoint_completed_event(checkpoint: &Checkpoint) -> Event {
    let status = checkpoint
        .node_outcomes
        .get(&checkpoint.current_node)
        .map_or_else(
            || "success".to_string(),
            |outcome| outcome.status.to_string(),
        );

    Event::CheckpointCompleted {
        node_id: checkpoint.current_node.clone(),
        status,
        current_node: checkpoint.current_node.clone(),
        completed_nodes: checkpoint.completed_nodes.clone(),
        node_retries: checkpoint.node_retries.clone().into_iter().collect(),
        context_values: checkpoint.context_values.clone().into_iter().collect(),
        node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
        next_node_id: checkpoint.next_node_id.clone(),
        git_commit_sha: checkpoint.git_commit_sha.clone(),
        loop_failure_signatures: checkpoint
            .loop_failure_signatures
            .iter()
            .map(|(signature, count)| (signature.to_string(), *count))
            .collect(),
        restart_failure_signatures: checkpoint
            .restart_failure_signatures
            .iter()
            .map(|(signature, count)| (signature.to_string(), *count))
            .collect(),
        node_visits: checkpoint.node_visits.clone().into_iter().collect(),
        diff: None,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use fabro_store::RunProjection;
    use fabro_types::{RunId, WorkflowSettings};
    use git2::Oid;

    use super::super::test_support::*;
    use super::*;
    use crate::operations::find_run_id_by_prefix;

    fn parse_run_id(value: &str) -> RunId {
        value.parse().unwrap()
    }

    fn make_run_projection(run_id: &RunId) -> RunProjection {
        let mut projection = RunProjection::default();
        let settings = serde_json::to_value(WorkflowSettings::default()).unwrap();
        projection.spec = Some(
            serde_json::from_value(serde_json::json!({
                "run_id": run_id.to_string(),
                "created_at": "2025-01-01T00:00:00Z",
                "settings": settings,
                "graph": {
                    "name": "test_workflow",
                    "nodes": {
                        "start": {"id": "start", "attrs": {}},
                        "build": {"id": "build", "attrs": {}},
                        "test": {"id": "test", "attrs": {}}
                    },
                    "edges": [
                        {"from": "start", "to": "build", "attrs": {}},
                        {"from": "build", "to": "test", "attrs": {}}
                    ],
                    "attrs": {}
                },
                "working_directory": "/tmp/test",
            }))
            .unwrap(),
        );
        projection
    }

    fn make_start_record_json(run_id: &RunId) -> Vec<u8> {
        let record = serde_json::json!({
            "run_id": run_id.to_string(),
            "start_time": "2025-01-01T00:00:00Z",
            "run_branch": format!("{}{}", RUN_BRANCH_PREFIX, run_id),
        });
        serde_json::to_vec_pretty(&record).unwrap()
    }

    fn setup_source_run(store: &Store, run_id: &RunId, nodes: &[&str]) -> Vec<Oid> {
        let sig = test_sig();

        let run_branch = format!("{RUN_BRANCH_PREFIX}{run_id}");
        let empty_tree = store.write_empty_tree().unwrap();
        let mut run_oids = Vec::new();
        let mut parent: Option<Oid> = None;

        for node in nodes {
            let parents = match parent {
                Some(p) => vec![p],
                None => vec![],
            };
            let oid = store
                .write_commit(
                    empty_tree,
                    &parents,
                    &format!("fabro({run_id}): {node} (completed)"),
                    &sig,
                )
                .unwrap();
            store.update_ref(&run_branch, oid).unwrap();
            run_oids.push(oid);
            parent = Some(oid);
        }

        let meta_branch = MetadataStore::branch_name(&run_id.to_string());
        let bs = BranchStore::new(store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();

        let mut init_projection = make_run_projection(run_id);
        init_projection.start =
            Some(serde_json::from_slice(&make_start_record_json(run_id)).unwrap());
        let init_json = serde_json::to_vec_pretty(&init_projection).unwrap();
        bs.write_entries(&[("run.json", &init_json)], "init run")
            .unwrap();

        for (i, node) in nodes.iter().enumerate() {
            let mut projection = init_projection.clone();
            projection.checkpoint = Some(
                serde_json::from_slice(&make_checkpoint_bytes(
                    node,
                    1,
                    Some(&run_oids[i].to_string()),
                ))
                .unwrap(),
            );
            let projection_json = serde_json::to_vec_pretty(&projection).unwrap();
            bs.write_entry("run.json", &projection_json, "checkpoint")
                .unwrap();
        }

        run_oids
    }

    #[test]
    fn fork_creates_new_run_and_metadata_branches() {
        let (_dir, store) = temp_repo();
        let source_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let run_oids = setup_source_run(&store, &source_run_id, &["start", "build", "test"]);

        let new_run_id = fork(&store, &ForkRunInput {
            source_run_id,
            target: Some(ForkTarget::from_str("@2").unwrap()),
            push: false,
        })
        .unwrap();

        let new_run_branch = format!("{RUN_BRANCH_PREFIX}{new_run_id}");
        let new_meta_branch = MetadataStore::branch_name(&new_run_id.to_string());

        assert!(store.resolve_ref(&new_run_branch).unwrap().is_some());
        assert!(store.resolve_ref(&new_meta_branch).unwrap().is_some());

        let sig = test_sig();
        let bs = BranchStore::new(&store, &new_meta_branch, &sig);
        let run_json = bs.read_entry("run.json").unwrap().unwrap();
        let run_spec: RunProjection = serde_json::from_slice(&run_json).unwrap();
        assert_eq!(
            run_spec.spec.as_ref().map(|run| run.run_id),
            Some(new_run_id)
        );

        let timeline = build_timeline(&store, &new_run_id.to_string()).unwrap();
        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(timeline.entries[0].node_name, "build");
        assert_eq!(
            timeline.entries[0].run_commit_sha,
            Some(run_oids[1].to_string())
        );
    }

    #[test]
    fn fork_rejects_checkpoint_without_run_sha() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAW");
        let meta_branch = MetadataStore::branch_name(&run_id.to_string());
        let bs = BranchStore::new(&store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();
        let init_projection = serde_json::to_vec_pretty(&make_run_projection(&run_id)).unwrap();
        bs.write_entry("run.json", &init_projection, "init")
            .unwrap();

        let mut checkpoint_projection = make_run_projection(&run_id);
        checkpoint_projection.checkpoint =
            Some(serde_json::from_slice(&make_checkpoint_bytes("start", 1, None)).unwrap());
        let cp = serde_json::to_vec_pretty(&checkpoint_projection).unwrap();
        let oid = bs.write_entry("run.json", &cp, "checkpoint").unwrap();
        let entry = TimelineEntry {
            ordinal:             1,
            node_name:           "start".to_string(),
            visit:               1,
            metadata_commit_oid: oid,
            run_commit_sha:      None,
        };

        let err = fork_from_entry(&store, &run_id, &entry, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot fork"));
    }

    #[test]
    fn fork_supports_prefix_resolved_source_run_ids() {
        let (_dir, store) = temp_repo();
        let source_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
        setup_source_run(&store, &source_run_id, &["start", "build"]);

        let resolved = find_run_id_by_prefix(store.repo(), "01ARZ3").unwrap();
        assert_eq!(resolved, source_run_id);
    }
}
