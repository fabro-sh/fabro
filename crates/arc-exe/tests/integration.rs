use arc_agent::sandbox::Sandbox;
use arc_exe::{ExeSandbox, OpensshRunner};

/// Full lifecycle test against a real exe.dev account.
/// Requires SSH agent with exe.dev credentials.
///
/// Run with: cargo test -p arc-exe -- --ignored
#[tokio::test]
#[ignore]
async fn exe_sandbox_full_lifecycle() {
    let mgmt_ssh = OpensshRunner::connect_raw("exe.dev")
        .await
        .expect("SSH to exe.dev failed — is your SSH agent running?");

    let sandbox = ExeSandbox::new(Box::new(mgmt_ssh));

    // Initialize (creates VM)
    sandbox.initialize().await.unwrap();
    assert!(!sandbox.sandbox_info().is_empty());
    assert_eq!(sandbox.platform(), "linux");

    // exec_command
    let result = sandbox
        .exec_command("echo hello", 10_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.stdout.trim(), "hello");
    assert_eq!(result.exit_code, 0);

    // write_file + read_file
    sandbox
        .write_file("test.txt", "line1\nline2\nline3")
        .await
        .unwrap();
    let content = sandbox.read_file("test.txt", None, None).await.unwrap();
    assert!(content.contains("1 | line1"));
    assert!(content.contains("2 | line2"));

    // file_exists
    assert!(sandbox.file_exists("test.txt").await.unwrap());
    assert!(!sandbox.file_exists("nonexistent.txt").await.unwrap());

    // delete_file
    sandbox.delete_file("test.txt").await.unwrap();
    assert!(!sandbox.file_exists("test.txt").await.unwrap());

    // Cleanup (destroys VM)
    sandbox.cleanup().await.unwrap();
}
