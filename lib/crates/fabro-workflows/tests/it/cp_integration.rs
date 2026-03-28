//! E2E tests for `fabro cp` against local and Docker sandbox backends.
//!
//! Local tests run without `#[ignore]` (no external dependencies).
//! Docker tests require a Docker daemon and are marked `#[ignore]`.
//! Run Docker tests with: `cargo test --package arc-workflows --test cp_integration -- --ignored`

use fabro_sandbox::reconnect::reconnect;
use fabro_sandbox::{SandboxRecord, SandboxRecordExt};

// ---------------------------------------------------------------------------
// Local sandbox
// ---------------------------------------------------------------------------

fn local_record(working_directory: &std::path::Path) -> SandboxRecord {
    SandboxRecord {
        provider: "local".to_string(),
        working_directory: working_directory.to_string_lossy().to_string(),
        identifier: None,
        host_working_directory: None,
        container_mount_point: None,
        data_host: None,
    }
}

#[tokio::test]
async fn local_cp_upload_download_round_trip() {
    let sandbox_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = local_record(sandbox_dir.path());
    let sandbox = reconnect(&record).await.expect("reconnect local");

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
    let sandbox = reconnect(&record).await.expect("reconnect local");

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
    let sandbox = reconnect(&record).await.expect("reconnect local");

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

#[tokio::test]
async fn local_cp_record_save_load_round_trip() {
    let sandbox_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = local_record(sandbox_dir.path());
    let path = scratch.path().join("sandbox.json");
    record.save(&path).unwrap();

    let loaded = SandboxRecord::load(&path).unwrap();
    assert_eq!(loaded.provider, "local");
    assert_eq!(loaded.working_directory, record.working_directory);
    assert!(loaded.identifier.is_none());

    // Reconnect from the loaded record still works
    let sandbox = reconnect(&loaded).await.expect("reconnect from loaded");
    assert_eq!(sandbox.working_directory(), record.working_directory);
}

// ---------------------------------------------------------------------------
// Docker sandbox
// ---------------------------------------------------------------------------

fn docker_record(host_dir: &std::path::Path, mount_point: &str) -> SandboxRecord {
    SandboxRecord {
        provider: "docker".to_string(),
        working_directory: mount_point.to_string(),
        identifier: None,
        host_working_directory: Some(host_dir.to_string_lossy().to_string()),
        container_mount_point: Some(mount_point.to_string()),
        data_host: None,
    }
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_upload_download_round_trip() {
    let host_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(host_dir.path(), "/workspace");
    let sandbox = reconnect(&record).await.expect("reconnect docker");

    // Upload a text file
    let content = b"hello from docker cp test\n";
    let local_src = scratch.path().join("upload.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "cp_test.txt")
        .await
        .expect("upload text");

    // Verify it landed on the host filesystem (bind mount path)
    assert!(host_dir.path().join("cp_test.txt").exists());
    assert_eq!(
        std::fs::read(host_dir.path().join("cp_test.txt")).unwrap(),
        content
    );

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
    let host_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(host_dir.path(), "/workspace");
    let sandbox = reconnect(&record).await.expect("reconnect docker");

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
    let host_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(host_dir.path(), "/workspace");
    let sandbox = reconnect(&record).await.expect("reconnect docker");

    let content = b"nested docker file\n";
    let local_src = scratch.path().join("nested.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "deep/nested/file.txt")
        .await
        .expect("upload to nested path");

    assert!(host_dir.path().join("deep/nested/file.txt").exists());

    let local_dst = scratch.path().join("p/q/file.txt");
    sandbox
        .download_file_to_local("deep/nested/file.txt", &local_dst)
        .await
        .expect("download to nested path");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_custom_mount_point() {
    let host_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    // Use a non-default mount point
    let record = docker_record(host_dir.path(), "/app");
    let sandbox = reconnect(&record).await.expect("reconnect docker");

    let content = b"custom mount\n";
    let local_src = scratch.path().join("mount.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "mount.txt")
        .await
        .expect("upload with custom mount");

    assert!(host_dir.path().join("mount.txt").exists());

    let local_dst = scratch.path().join("mount_dl.txt");
    sandbox
        .download_file_to_local("mount.txt", &local_dst)
        .await
        .expect("download with custom mount");

    assert_eq!(std::fs::read(&local_dst).unwrap(), content);
}

#[tokio::test]
#[ignore] // requires Docker daemon
async fn docker_cp_record_save_load_round_trip() {
    let host_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let record = docker_record(host_dir.path(), "/workspace");
    let path = scratch.path().join("sandbox.json");
    record.save(&path).unwrap();

    let loaded = SandboxRecord::load(&path).unwrap();
    assert_eq!(loaded.provider, "docker");
    assert_eq!(
        loaded.host_working_directory.as_deref(),
        Some(host_dir.path().to_str().unwrap())
    );
    assert_eq!(loaded.container_mount_point.as_deref(), Some("/workspace"));

    // Reconnect from the loaded record still works
    let sandbox = reconnect(&loaded).await.expect("reconnect from loaded");

    let content = b"from loaded record\n";
    let local_src = scratch.path().join("loaded.txt");
    std::fs::write(&local_src, content).unwrap();

    sandbox
        .upload_file_from_local(&local_src, "loaded.txt")
        .await
        .expect("upload via loaded record");

    assert_eq!(
        std::fs::read(host_dir.path().join("loaded.txt")).unwrap(),
        content
    );
}
