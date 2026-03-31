use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["parse", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Parse a DOT file and print its AST

    Usage: fabro parse [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to the .fabro workflow file

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
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
fn parse_valid_workflow_prints_ast_json() {
    let context = test_context!();
    context.write_temp(
        "tiny.fabro",
        "digraph Tiny {\n  graph [goal=\"Parse a tiny workflow\"]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  main [label=\"Main\", prompt=\"Do the thing\"]\n  start -> main -> exit\n}\n",
    );
    let mut cmd = context.command();
    cmd.current_dir(&context.temp_dir);
    cmd.args(["parse", "tiny.fabro"]);

    fabro_snapshot!(context.filters(), cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "name": "Tiny",
      "statements": [
        {
          "GraphAttr": [
            [
              "goal",
              {
                "Str": "Parse a tiny workflow"
              }
            ]
          ]
        },
        {
          "Node": {
            "id": "start",
            "attrs": [
              [
                "shape",
                {
                  "Ident": "Mdiamond"
                }
              ]
            ]
          }
        },
        {
          "Node": {
            "id": "exit",
            "attrs": [
              [
                "shape",
                {
                  "Ident": "Msquare"
                }
              ]
            ]
          }
        },
        {
          "Node": {
            "id": "main",
            "attrs": [
              [
                "label",
                {
                  "Str": "Main"
                }
              ],
              [
                "prompt",
                {
                  "Str": "Do the thing"
                }
              ]
            ]
          }
        },
        {
          "Edge": {
            "nodes": [
              "start",
              "main",
              "exit"
            ],
            "attrs": null
          }
        }
      ]
    }
    ----- stderr -----
    "###);
}

#[test]
fn parse_invalid_dot_fails_cleanly() {
    let context = test_context!();
    context.write_temp(
        "bad.fabro",
        "digraph Bad {\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  start -> exit\n",
    );
    let mut cmd = context.command();
    cmd.current_dir(&context.temp_dir);
    cmd.args(["parse", "bad.fabro"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Parse error: grammar error: Parsing Error: Error { input: \"\", code: Char }
    ");
}
