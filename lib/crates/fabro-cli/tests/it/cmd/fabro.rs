use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Usage: fabro [OPTIONS] <COMMAND>

    Commands:
      run         Launch a workflow run
      create      Create a workflow run (allocate run dir, persist spec)
      start       Start a created workflow run on the server
      attach      Attach to a running or finished workflow run
      logs        View the event log of a workflow run
      resume      Resume an interrupted workflow run
      rewind      Rewind a workflow run to an earlier checkpoint
      fork        Fork a workflow run from an earlier checkpoint into a new run
      wait        Block until a workflow run completes
      preflight   Validate run configuration without executing
      validate    Validate a workflow
      graph       Render a workflow graph as SVG
      artifact    Inspect and copy run artifacts (screenshots, reports, traces)
      store       Export store-backed run state for debugging
      rm          Remove one or more workflow runs
      inspect     Show detailed information about a workflow run
      model       List and test LLM models
      server      Server operations
      doctor      Check environment and integration health
      install     Set up the Fabro environment (LLMs, certs, GitHub)
      uninstall   Uninstall Fabro from this machine
      pr          Pull request operations
      secret      Manage server-owned secrets
      settings    Inspect effective settings
      workflow    Workflow operations
      discord     Open the Discord community in the browser
      docs        Open the docs website in the browser
      upgrade     Upgrade fabro to the latest version
      repo        Repository commands
      provider    Provider operations
      sandbox     Sandbox operations (cp, ssh, preview)
      completion  Generate shell completions
      system      System maintenance commands
      help        Print this message or the help of the given subcommand(s)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
      -V, --version           Print version
    ----- stderr -----
    ");
}

#[test]
fn llm_namespace_is_not_available() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.arg("llm");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: unrecognized subcommand 'llm'

    Usage: fabro [OPTIONS] <COMMAND>

    For more information, try '--help'.
    ");
}
