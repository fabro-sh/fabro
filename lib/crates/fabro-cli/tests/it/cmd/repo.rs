use fabro_test::{fabro_snapshot, test_context};
use predicates;

#[allow(deprecated)]
fn fabro() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("fabro").unwrap();
    cmd.env("NO_COLOR", "1");
    cmd
}

fn init_git_repo(path: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init should succeed");
}

fn init_fabro_project(path: &std::path::Path) {
    std::fs::write(path.join("fabro.toml"), "version = 1\n").unwrap();
    let workflow_dir = path.join("fabro/workflows/hello");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(workflow_dir.join("workflow.fabro"), "digraph {}").unwrap();
    std::fs::write(workflow_dir.join("workflow.toml"), "version = 1\n").unwrap();
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.repo();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Repository commands

    Usage: fabro repo [OPTIONS] <COMMAND>

    Commands:
      init    Initialize a new project
      deinit  Remove fabro.toml and fabro/ directory
      help    Print this message or the help of the given subcommand(s)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn test_repo_deinit_removes_fabro_toml_and_dir() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    init_fabro_project(tmp.path());

    assert!(tmp.path().join("fabro.toml").exists());
    assert!(tmp.path().join("fabro").exists());

    fabro()
        .args(["repo", "deinit"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(
        !tmp.path().join("fabro.toml").exists(),
        "fabro.toml should be removed"
    );
    assert!(
        !tmp.path().join("fabro").exists(),
        "fabro/ directory should be removed"
    );
}

#[test]
fn test_repo_deinit_fails_when_not_initialized() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());

    fabro()
        .args(["repo", "deinit"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("not initialized"));
}

#[test]
fn test_repo_init_skill_installs_skill_files() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());

    fabro()
        .args(["repo", "init", "--skill"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Skill files should be installed under .claude/skills/fabro-create-workflow/
    let skill_dir = tmp.path().join(".claude/skills/fabro-create-workflow");
    assert!(skill_dir.join("SKILL.md").exists(), "SKILL.md should exist");
    assert!(
        skill_dir.join("references/dot-language.md").exists(),
        "dot-language.md should exist"
    );
}

#[test]
fn test_repo_init_help_does_not_show_skill() {
    let out = fabro().args(["repo", "init", "--help"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("--skill"),
        "--skill should be hidden from help"
    );
}
