use std::path::PathBuf;
use std::process::Command;

#[test]
fn symbolic_branch_tracks_head_and_active_ref() {
    let repo = git_repo();

    let metadata = fabro_build_support::collect_from(repo.path());

    assert_eq!(metadata.rerun_paths, vec![
        PathBuf::from(".git/HEAD"),
        PathBuf::from(".git/refs/heads/main"),
    ]);
    assert_eq!(metadata.short_sha.len(), 7);
}

#[test]
fn detached_head_tracks_only_head() {
    let repo = git_repo();
    git(repo.path(), ["checkout", "--detach", "HEAD"]);

    let metadata = fabro_build_support::collect_from(repo.path());

    assert_eq!(metadata.rerun_paths, vec![PathBuf::from(".git/HEAD")]);
    assert_eq!(metadata.short_sha.len(), 7);
}

#[test]
fn non_git_directory_has_no_metadata() {
    let dir = tempfile::tempdir().expect("temp dir should create");

    let metadata = fabro_build_support::collect_from(dir.path());

    assert!(metadata.rerun_paths.is_empty());
    assert!(metadata.short_sha.is_empty());
}

#[test]
fn packed_refs_are_not_tracked() {
    let repo = git_repo();
    git(repo.path(), ["pack-refs", "--all", "--prune"]);

    let metadata = fabro_build_support::collect_from(repo.path());

    // A packed ref has no loose file, and Cargo reruns on every build for a
    // missing path, so the directory its next loose ref is written to is watched.
    assert_eq!(metadata.rerun_paths, vec![
        PathBuf::from(".git/HEAD"),
        PathBuf::from(".git/refs/heads"),
    ]);
    assert!(
        !metadata
            .rerun_paths
            .iter()
            .any(|path| path.ends_with("packed-refs"))
    );

    git(repo.path(), ["checkout", "-b", "codex/foo"]);
    git(repo.path(), ["pack-refs", "--all", "--prune"]);

    let metadata = fabro_build_support::collect_from(repo.path());

    assert_eq!(metadata.rerun_paths, vec![
        PathBuf::from(".git/HEAD"),
        PathBuf::from(".git/refs/heads"),
    ]);
}

#[expect(
    clippy::disallowed_methods,
    reason = "This test creates a small synchronous Git fixture before exercising build metadata collection."
)]
fn git_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo should create");
    git(repo.path(), ["init"]);
    git(repo.path(), ["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(repo.path(), ["config", "user.email", "fabro@example.test"]);
    git(repo.path(), ["config", "user.name", "Fabro Test"]);
    std::fs::write(repo.path().join("README.md"), "test\n").expect("readme should write");
    git(repo.path(), ["add", "README.md"]);
    git(repo.path(), ["commit", "-m", "initial"]);
    repo
}

#[expect(
    clippy::disallowed_methods,
    reason = "This test fixture uses synchronous Git commands to set up repository states."
)]
fn git<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
