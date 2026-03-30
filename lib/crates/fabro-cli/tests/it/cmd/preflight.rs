use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["preflight", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Validate run configuration without executing

    Usage: fabro preflight [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --model <MODEL>              Override default LLM model
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --provider <PROVIDER>        Override default LLM provider
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona, ssh]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
