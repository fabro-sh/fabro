use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List and test LLM models

    Usage: fabro model [OPTIONS] [COMMAND]

    Commands:
      list  List available models
      test  Test model availability by sending a simple prompt
      help  Print this message or the help of the given subcommand(s)

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
fn bare() {
    let context = test_context!();
    fabro_snapshot!(context.filters(), context.model(), @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL                             [0m [0m[0m [0m[0m[0m[1mPROVIDER [0m [0m[0m [0m[0m[0m[1mALIASES                [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m          COST[0m [0m[0m [0m[0m[0m[1m     SPEED[0m [0m
    [0m[0m [0m[0m[0mclaude-opus-4-6                   [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mopus, claude-opus      [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m $15.0 / $75.0[0m [0m[0m [0m[0m[0m  25 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-5                 [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0m                       [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m  $3.0 / $15.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-6                 [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0msonnet, claude-sonnet  [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m  $3.0 / $15.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-haiku-4-5                  [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mhaiku, claude-haiku    [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m   $0.8 / $4.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.2                           [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt5                   [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m  65 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5-mini                        [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt5-mini              [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.2 / $2.0[0m [0m[0m [0m[0m[0m  70 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.2-codex                     [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0m                       [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex                     [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mcodex                  [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex-spark               [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mcodex-spark            [0m [0m[0m [0m[0m[0m   131k[0m [0m[0m [0m[0m[0m         - / -[0m [0m[0m [0m[0m[0m1000 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4                           [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54, gpt-54          [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.5 / $15.0[0m [0m[0m [0m[0m[0m  70 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4-pro                       [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54-pro, gpt-54-pro  [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$30.0 / $180.0[0m [0m[0m [0m[0m[0m  20 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4-mini                      [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54-mini, gpt-54-mini[0m [0m[0m [0m[0m[0m   400k[0m [0m[0m [0m[0m[0m   $0.8 / $4.5[0m [0m[0m [0m[0m[0m 140 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-pro-preview            [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-pro             [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.0 / $12.0[0m [0m[0m [0m[0m[0m  85 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-pro-preview-customtools[0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-customtools     [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.0 / $12.0[0m [0m[0m [0m[0m[0m  85 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3-flash-preview            [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-flash           [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.5 / $3.0[0m [0m[0m [0m[0m[0m 150 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-flash-lite-preview     [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-flash-lite      [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.2 / $1.5[0m [0m[0m [0m[0m[0m 200 tok/s[0m [0m
    [0m[0m [0m[0m[0mkimi-k2.5                         [0m [0m[0m [0m[0m[0mkimi     [0m [0m[0m [0m[0m[0mkimi                   [0m [0m[0m [0m[0m[0m   262k[0m [0m[0m [0m[0m[0m   $0.6 / $3.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mglm-4.7                           [0m [0m[0m [0m[0m[0mzai      [0m [0m[0m [0m[0m[0mglm, glm4              [0m [0m[0m [0m[0m[0m   203k[0m [0m[0m [0m[0m[0m   $0.6 / $2.2[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mminimax-m2.5                      [0m [0m[0m [0m[0m[0mminimax  [0m [0m[0m [0m[0m[0mminimax                [0m [0m[0m [0m[0m[0m   197k[0m [0m[0m [0m[0m[0m   $0.3 / $1.2[0m [0m[0m [0m[0m[0m  45 tok/s[0m [0m
    [0m[0m [0m[0m[0mmercury-2                         [0m [0m[0m [0m[0m[0minception[0m [0m[0m [0m[0m[0mmercury                [0m [0m[0m [0m[0m[0m   131k[0m [0m[0m [0m[0m[0m   $0.2 / $0.8[0m [0m[0m [0m[0m[0m1000 tok/s[0m [0m
    [0m----- stderr -----
    ");
}

#[test]
fn list() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.arg("list");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL                             [0m [0m[0m [0m[0m[0m[1mPROVIDER [0m [0m[0m [0m[0m[0m[1mALIASES                [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m          COST[0m [0m[0m [0m[0m[0m[1m     SPEED[0m [0m
    [0m[0m [0m[0m[0mclaude-opus-4-6                   [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mopus, claude-opus      [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m $15.0 / $75.0[0m [0m[0m [0m[0m[0m  25 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-5                 [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0m                       [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m  $3.0 / $15.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-6                 [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0msonnet, claude-sonnet  [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m  $3.0 / $15.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-haiku-4-5                  [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mhaiku, claude-haiku    [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m   $0.8 / $4.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.2                           [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt5                   [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m  65 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5-mini                        [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt5-mini              [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.2 / $2.0[0m [0m[0m [0m[0m[0m  70 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.2-codex                     [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0m                       [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex                     [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mcodex                  [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex-spark               [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mcodex-spark            [0m [0m[0m [0m[0m[0m   131k[0m [0m[0m [0m[0m[0m         - / -[0m [0m[0m [0m[0m[0m1000 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4                           [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54, gpt-54          [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.5 / $15.0[0m [0m[0m [0m[0m[0m  70 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4-pro                       [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54-pro, gpt-54-pro  [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$30.0 / $180.0[0m [0m[0m [0m[0m[0m  20 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.4-mini                      [0m [0m[0m [0m[0m[0mopenai   [0m [0m[0m [0m[0m[0mgpt54-mini, gpt-54-mini[0m [0m[0m [0m[0m[0m   400k[0m [0m[0m [0m[0m[0m   $0.8 / $4.5[0m [0m[0m [0m[0m[0m 140 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-pro-preview            [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-pro             [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.0 / $12.0[0m [0m[0m [0m[0m[0m  85 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-pro-preview-customtools[0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-customtools     [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m  $2.0 / $12.0[0m [0m[0m [0m[0m[0m  85 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3-flash-preview            [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-flash           [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.5 / $3.0[0m [0m[0m [0m[0m[0m 150 tok/s[0m [0m
    [0m[0m [0m[0m[0mgemini-3.1-flash-lite-preview     [0m [0m[0m [0m[0m[0mgemini   [0m [0m[0m [0m[0m[0mgemini-flash-lite      [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m   $0.2 / $1.5[0m [0m[0m [0m[0m[0m 200 tok/s[0m [0m
    [0m[0m [0m[0m[0mkimi-k2.5                         [0m [0m[0m [0m[0m[0mkimi     [0m [0m[0m [0m[0m[0mkimi                   [0m [0m[0m [0m[0m[0m   262k[0m [0m[0m [0m[0m[0m   $0.6 / $3.0[0m [0m[0m [0m[0m[0m  50 tok/s[0m [0m
    [0m[0m [0m[0m[0mglm-4.7                           [0m [0m[0m [0m[0m[0mzai      [0m [0m[0m [0m[0m[0mglm, glm4              [0m [0m[0m [0m[0m[0m   203k[0m [0m[0m [0m[0m[0m   $0.6 / $2.2[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mminimax-m2.5                      [0m [0m[0m [0m[0m[0mminimax  [0m [0m[0m [0m[0m[0mminimax                [0m [0m[0m [0m[0m[0m   197k[0m [0m[0m [0m[0m[0m   $0.3 / $1.2[0m [0m[0m [0m[0m[0m  45 tok/s[0m [0m
    [0m[0m [0m[0m[0mmercury-2                         [0m [0m[0m [0m[0m[0minception[0m [0m[0m [0m[0m[0mmercury                [0m [0m[0m [0m[0m[0m   131k[0m [0m[0m [0m[0m[0m   $0.2 / $0.8[0m [0m[0m [0m[0m[0m1000 tok/s[0m [0m
    [0m----- stderr -----
    ");
}

#[test]
fn list_provider() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--provider", "anthropic"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL            [0m [0m[0m [0m[0m[0m[1mPROVIDER [0m [0m[0m [0m[0m[0m[1mALIASES              [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m         COST[0m [0m[0m [0m[0m[0m[1m    SPEED[0m [0m
    [0m[0m [0m[0m[0mclaude-opus-4-6  [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mopus, claude-opus    [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$15.0 / $75.0[0m [0m[0m [0m[0m[0m 25 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-5[0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0m                     [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m $3.0 / $15.0[0m [0m[0m [0m[0m[0m 50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-sonnet-4-6[0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0msonnet, claude-sonnet[0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m $3.0 / $15.0[0m [0m[0m [0m[0m[0m 50 tok/s[0m [0m
    [0m[0m [0m[0m[0mclaude-haiku-4-5 [0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mhaiku, claude-haiku  [0m [0m[0m [0m[0m[0m   200k[0m [0m[0m [0m[0m[0m  $0.8 / $4.0[0m [0m[0m [0m[0m[0m100 tok/s[0m [0m
    [0m----- stderr -----
    ");
}

#[test]
fn list_query() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "opus"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL          [0m [0m[0m [0m[0m[0m[1mPROVIDER [0m [0m[0m [0m[0m[0m[1mALIASES          [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m         COST[0m [0m[0m [0m[0m[0m[1m   SPEED[0m [0m
    [0m[0m [0m[0m[0mclaude-opus-4-6[0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mopus, claude-opus[0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$15.0 / $75.0[0m [0m[0m [0m[0m[0m25 tok/s[0m [0m
    [0m----- stderr -----
    ");
}

#[test]
fn list_query_aliases() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "codex"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL              [0m [0m[0m [0m[0m[0m[1mPROVIDER[0m [0m[0m [0m[0m[0m[1mALIASES    [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m        COST[0m [0m[0m [0m[0m[0m[1m     SPEED[0m [0m
    [0m[0m [0m[0m[0mgpt-5.2-codex      [0m [0m[0m [0m[0m[0mopenai  [0m [0m[0m [0m[0m[0m           [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex      [0m [0m[0m [0m[0m[0mopenai  [0m [0m[0m [0m[0m[0mcodex      [0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$1.8 / $14.0[0m [0m[0m [0m[0m[0m 100 tok/s[0m [0m
    [0m[0m [0m[0m[0mgpt-5.3-codex-spark[0m [0m[0m [0m[0m[0mopenai  [0m [0m[0m [0m[0m[0mcodex-spark[0m [0m[0m [0m[0m[0m   131k[0m [0m[0m [0m[0m[0m       - / -[0m [0m[0m [0m[0m[0m1000 tok/s[0m [0m
    [0m----- stderr -----
    ");
}

#[test]
fn list_query_case_insensitive() {
    let context = test_context!();
    let mut cmd = context.model();
    cmd.args(["list", "--query", "OPUS"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [0m [0m[0m[0m[1mMODEL          [0m [0m[0m [0m[0m[0m[1mPROVIDER [0m [0m[0m [0m[0m[0m[1mALIASES          [0m [0m[0m [0m[0m[0m[1mCONTEXT[0m [0m[0m [0m[0m[0m[1m         COST[0m [0m[0m [0m[0m[0m[1m   SPEED[0m [0m
    [0m[0m [0m[0m[0mclaude-opus-4-6[0m [0m[0m [0m[0m[0manthropic[0m [0m[0m [0m[0m[0mopus, claude-opus[0m [0m[0m [0m[0m[0m     1m[0m [0m[0m [0m[0m[0m$15.0 / $75.0[0m [0m[0m [0m[0m[0m25 tok/s[0m [0m
    [0m----- stderr -----
    ");
}
