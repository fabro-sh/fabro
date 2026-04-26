//! E2E tests for `fabro cp` against local and Docker sandbox backends.
//!
//! Local tests run without `#[ignore]` (no external dependencies).
//! Docker tests require a Docker daemon and are marked `#[ignore]`.
//! Run Docker tests with: `cargo test --package arc-workflows --test
//! cp_integration -- --ignored`

#![allow(
    clippy::ignore_without_reason,
    reason = "This integration module intentionally uses concise ignored-test markers."
)]
#![expect(
    clippy::disallowed_methods,
    reason = "This integration test stages sandbox fixtures with sync std::fs."
)]

use fabro_sandbox::SandboxRecord;
use fabro_sandbox::reconnect::reconnect;

// ---------------------------------------------------------------------------
// Local sandbox
// ---------------------------------------------------------------------------

fn local_record(working_directory: &std::path::Path) -> SandboxRecord {
    SandboxRecord {
        provider:               "local".to_string(),
        working_directory:      working_directory.to_string_lossy().to_string(),
        identifier:             None,
        host_working_directory: None,
        container_mount_point:  None,
        repo_cloned:            None,
        clone_origin_url:       None,
        clone_branch:           None,
    }
}

#[tokio::test]
async fn local_cp_upload_download_round_trip() {
    let sandbox_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = local_record(sandbox_dir.path());
    let sandbox = reconnect(&record, None).await.expect("reconnect local");

    // Upload a text file
    let content = b"hello from local cp test\n";
    let local_src = scratch.path().join("upload.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "cp_test.txt")
        .await
        .expect("upload text");

    // Verify it landed in the sandbox working directory
    assert!(sandbox_dir.path().join("cp_test.txt").exists());

    // Download it back
    let local_dst = scratch.path().join("download.txt");
    sandbox
        .download_file_to_local("cp_test.txt", &local_dst)
        .await
        .expect("download text");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}

#[tokio::test]
async fn local_cp_binary_round_trip() {
    let sandbox_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = local_record(sandbox_dir.path());
    let sandbox = reconnect(&record, None).await.expect("reconnect local");

    // All 256 byte values
    let binary: Vec<u8> = (0..=255).collect();
    let local_src = scratch.path().join("binary.bin");
    std::fs::write(&local_src, &binary).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "binary.bin")
        .await
        .expect("upload binary");

    let local_dst = scratch.path().join("binary_dl.bin");
    sandbox
        .download_file_to_local("binary.bin", &local_dst)
        .await
        .expect("download binary");

    assert_eq!(std::fs::read(&local_dst).unwrap(), binary);
}

#[tokio::test]
async fn local_cp_creates_parent_dirs() {
    let sandbox_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = local_record(sandbox_dir.path());
    let sandbox = reconnect(&record, None).await.expect("reconnect local");

    let content = b"nested file\n";
    let local_src = scratch.path().join("nested.txt");
    std::fs::write(&local_src, content).unwrap();

    // Upload to a nested path that doesn't exist yet
    sandbox
        .upload_file_from_local(&local_src, "a/b/c/nested.txt")
        .await
        .expect("upload to nested path");

    assert!(sandbox_dir.path().join("a/b/c/nested.txt").exists());

    // Download to a nested local path that doesn't exist yet
    let local_dst = scratch.path().join("x/y/z/nested.txt");
    sandbox
        .download_file_to_local("a/b/c/nested.txt", &local_dst)
        .await
        .expect("download to nested path");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}

// ---------------------------------------------------------------------------
// Docker sandbox
// ---------------------------------------------------------------------------

fn docker_record(container_id: &str) -> SandboxRecord {
    SandboxRecord {
        provider:               "docker".to_string(),
        working_directory:      "/workspace".to_string(),
        identifier:             Some(container_id.to_string()),
        host_working_directory: None,
        container_mount_point:  None,
        repo_cloned:            Some(false),
        clone_origin_url:       None,
        clone_branch:           None,
    }
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_upload_download_round_trip() {
    let container_id = std::env::var("FABRO_DOCKER_CP_CONTAINER")
        .expect("set FABRO_DOCKER_CP_CONTAINER to an initialized Fabro-managed container ID");
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(&container_id);
    let sandbox = reconnect(&record, None).await.expect("reconnect docker");

    // Upload a text file
    let content = b"hello from docker cp test\n";
    let local_src = scratch.path().join("upload.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "cp_test.txt")
        .await
        .expect("upload text");

    // Download it back
    let local_dst = scratch.path().join("download.txt");
    sandbox
        .download_file_to_local("cp_test.txt", &local_dst)
        .await
        .expect("download text");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_binary_round_trip() {
    let container_id = std::env::var("FABRO_DOCKER_CP_CONTAINER")
        .expect("set FABRO_DOCKER_CP_CONTAINER to an initialized Fabro-managed container ID");
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(&container_id);
    let sandbox = reconnect(&record, None).await.expect("reconnect docker");

    let binary: Vec<u8> = (0..=255).collect();
    let local_src = scratch.path().join("binary.bin");
    std::fs::write(&local_src, &binary).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "binary.bin")
        .await
        .expect("upload binary");

    let local_dst = scratch.path().join("binary_dl.bin");
    sandbox
        .download_file_to_local("binary.bin", &local_dst)
        .await
        .expect("download binary");

    assert_eq!(std::fs::read(&local_dst).unwrap(), binary);
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_creates_parent_dirs() {
    let container_id = std::env::var("FABRO_DOCKER_CP_CONTAINER")
        .expect("set FABRO_DOCKER_CP_CONTAINER to an initialized Fabro-managed container ID");
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(&container_id);
    let sandbox = reconnect(&record, None).await.expect("reconnect docker");

    let content = b"nested docker file\n";
    let local_src = scratch.path().join("nested.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "deep/nested/file.txt")
        .await
        .expect("upload to nested path");

    let local_dst = scratch.path().join("p/q/file.txt");
    sandbox
        .download_file_to_local("deep/nested/file.txt", &local_dst)
        .await
        .expect("download to nested path");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}
