use std::collections::BTreeSet;
use std::path::Path;

use fabro_checkpoint::branch::BranchStore;
use fabro_checkpoint::git::Store as GitStore;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::Checkpoint;
use git2::{Repository, Signature};

fn list_metadata_run_ids(repo_dir: &Path) -> BTreeSet<String> {
    let repo = Repository::discover(repo_dir).unwrap();
    repo.references()
        .unwrap()
        .flatten()
        .filter_map(|reference| reference.name().map(ToOwned::to_owned))
        .filter_map(|name| {
            name.strip_prefix("refs/heads/fabro/meta/")
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn metadata_checkpoints(repo_dir: &Path, run_id: &str) -> Vec<Checkpoint> {
    let repo = Repository::discover(repo_dir).unwrap();
    let store = GitStore::new(repo);
    let sig = Signature::now("Fabro", "noreply@fabro.sh").unwrap();
    let branch = format!("fabro/meta/{run_id}");
    let bs = BranchStore::new(&store, &branch, &sig);

    bs.log(100)
        .unwrap()
        .iter()
        .rev()
        .filter(|commit| commit.message.starts_with("checkpoint"))
        .map(|commit| {
            serde_json::from_slice::<Checkpoint>(
                &store
                    .read_blob_at(commit.oid, "checkpoint.json")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
        })
        .collect()
}

fn latest_metadata_checkpoint(repo_dir: &Path, run_id: &str) -> Checkpoint {
    let repo = Repository::discover(repo_dir).unwrap();
    let store = GitStore::new(repo);
    let tip = store
        .resolve_ref(&format!("fabro/meta/{run_id}"))
        .unwrap()
        .unwrap();
    serde_json::from_slice(&store.read_blob_at(tip, "checkpoint.json").unwrap().unwrap()).unwrap()
}

fn init_repo_with_workflow(repo_dir: &Path) {
    std::fs::write(repo_dir.join("README.md"), "recovery test\n").unwrap();
    std::fs::write(
        repo_dir.join("workflow.fabro"),
        "\
digraph Recovery {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  plan  [label=\"Plan\", shape=parallelogram, script=\"echo plan\"]
  build [label=\"Build\", shape=parallelogram, script=\"echo build\"]
  start -> plan -> build -> exit
}
",
    )
    .unwrap();

    let init = std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo_dir)
        .status()
        .unwrap();
    assert!(init.success(), "git init should succeed");

    let add = std::process::Command::new("git")
        .args(["add", "README.md", "workflow.fabro"])
        .current_dir(repo_dir)
        .status()
        .unwrap();
    assert!(add.success(), "git add should succeed");

    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fabro",
            "-c",
            "user.email=noreply@fabro.sh",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(repo_dir)
        .status()
        .unwrap();
    assert!(commit.success(), "git commit should succeed");
}

#[test]
fn rewind_and_fork_recover_missing_metadata_from_real_run_state() {
    let context = test_context!();
    let repo_dir = tempfile::tempdir().unwrap();
    let source_run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAN";

    init_repo_with_workflow(repo_dir.path());

    context
        .command()
        .current_dir(repo_dir.path())
        .args([
            "run",
            "--dry-run",
            "--no-retro",
            "--sandbox",
            "local",
            "--run-id",
            source_run_id,
            "workflow.fabro",
        ])
        .assert()
        .success();

    let mut filters = Vec::new();
    filters.push((r"\b[0-9a-f]{7,40}\b".to_string(), "[SHA]".to_string()));
    filters.extend(context.filters());

    Repository::discover(repo_dir.path())
        .unwrap()
        .find_reference(&format!("refs/heads/fabro/meta/{source_run_id}"))
        .unwrap()
        .delete()
        .unwrap();

    assert!(
        list_metadata_run_ids(repo_dir.path()).is_empty(),
        "metadata branch should start missing"
    );

    let mut rewind_list = context.command();
    rewind_list.current_dir(repo_dir.path());
    rewind_list.args(["rewind", source_run_id, "--list"]);
    rewind_list.timeout(std::time::Duration::from_secs(15));
    fabro_snapshot!(filters.clone(), rewind_list, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    @   Node   Details         
     @1  start  (no run commit) 
     @2  plan                   
     @3  build
    ");

    let rebuilt_checkpoints = metadata_checkpoints(repo_dir.path(), source_run_id);
    assert_eq!(
        rebuilt_checkpoints
            .first()
            .and_then(|c| c.git_commit_sha.clone()),
        None
    );
    assert!(rebuilt_checkpoints.len() >= 2);
    let plan_sha = rebuilt_checkpoints[rebuilt_checkpoints.len() - 2]
        .git_commit_sha
        .clone();
    let build_sha = rebuilt_checkpoints
        .last()
        .and_then(|checkpoint| checkpoint.git_commit_sha.clone());
    assert!(build_sha.is_some());

    let before_child = list_metadata_run_ids(repo_dir.path());
    context
        .command()
        .current_dir(repo_dir.path())
        .args(["fork", source_run_id, "--no-push"])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .success();
    let after_child = list_metadata_run_ids(repo_dir.path());
    let child_run_ids: Vec<_> = after_child.difference(&before_child).cloned().collect();
    assert_eq!(child_run_ids.len(), 1, "expected one child run");
    let child_run_id = &child_run_ids[0];

    let child_checkpoint = latest_metadata_checkpoint(repo_dir.path(), child_run_id);
    assert_eq!(child_checkpoint.git_commit_sha, build_sha);

    let mut rewind_filters = filters.clone();
    rewind_filters.push((
        regex::escape(&source_run_id[..8]),
        "[RUN_PREFIX]".to_string(),
    ));

    let mut source_rewind = context.command();
    source_rewind.current_dir(repo_dir.path());
    source_rewind.args(["rewind", source_run_id, "@2", "--no-push"]);
    source_rewind.timeout(std::time::Duration::from_secs(15));
    fabro_snapshot!(rewind_filters, source_rewind, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Rewound metadata branch to @2 (plan)
    Rewound run branch fabro/run/[ULID] to [SHA]

    To resume: fabro resume [RUN_PREFIX]
    ");

    let rewound_child = latest_metadata_checkpoint(repo_dir.path(), source_run_id);
    assert_eq!(rewound_child.git_commit_sha, plan_sha);

    let before_grandchild = list_metadata_run_ids(repo_dir.path());
    context
        .command()
        .current_dir(repo_dir.path())
        .args(["fork", source_run_id, "--no-push"])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .success();
    let after_grandchild = list_metadata_run_ids(repo_dir.path());
    let grandchild_run_ids: Vec<_> = after_grandchild
        .difference(&before_grandchild)
        .cloned()
        .collect();
    assert_eq!(grandchild_run_ids.len(), 1, "expected one grandchild run");

    let grandchild_checkpoint = latest_metadata_checkpoint(repo_dir.path(), &grandchild_run_ids[0]);
    assert_eq!(grandchild_checkpoint.git_commit_sha, plan_sha);
}
