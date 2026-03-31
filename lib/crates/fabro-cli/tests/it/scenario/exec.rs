use std::time::Duration;

use fabro_test::test_context;

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn test_exec_creates_file() {
    let context = test_context!();

    let mut cmd = context.exec_cmd();
    cmd.args([
        "--auto-approve",
        "--permissions",
        "full",
        "--provider",
        "anthropic",
        "--model",
        "claude-haiku-4-5",
        "Create a file called hello.txt containing exactly 'Hello from exec scenario'",
    ]);
    cmd.timeout(Duration::from_secs(120));
    cmd.assert().success();

    let hello = context.temp_dir.join("hello.txt");
    assert!(hello.exists(), "hello.txt should exist after exec");
    let content = std::fs::read_to_string(&hello).unwrap();
    assert!(
        content.contains("Hello from exec scenario"),
        "hello.txt should contain greeting, got: {content}"
    );
}
