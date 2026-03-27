# Run Configuration (TOML) Reference

The TOML file configures how a workflow is executed. It is separate from the DOT graph which defines the workflow structure.

## Minimal Config

```toml
version = 1
graph = "workflow.fabro"
goal = "Implement the login feature"
```

**Required:** `version` (must be 1), `graph` (path relative to TOML file's directory).

## All Sections

### Top-level

| Field | Type | Description |
|---|---|---|
| `version` | Integer | Must be `1` |
| `graph` | String | Path to DOT file (relative to TOML) |
| `goal` | String | Override workflow goal |
| `directory` | String | Working directory for the run |

### `[llm]`

```toml
[llm]
model = "claude-opus-4-6"
provider = "anthropic"

[llm.fallbacks]
anthropic = ["gemini-3.1-pro-preview", "gpt-5.2"]
openai = ["claude-sonnet-4-6"]
```

### `[setup]`

Sequential shell commands run before the workflow starts:

```toml
[setup]
commands = ["npm install", "npm run build"]
timeout_ms = 300000
```

### `[sandbox]`

```toml
[sandbox]
provider = "local"     # local, docker, daytona, exe
preserve = false
devcontainer = false

[sandbox.local]
worktree_mode = "always"  # always, auto, never

[sandbox.daytona]
auto_stop_interval = "30m"

[sandbox.daytona.snapshot]
name = "my-snapshot"
cpu = 4
memory = 8
disk = 20
dockerfile = { path = "Dockerfile" }

[sandbox.env]
NODE_ENV = "test"
API_KEY = "${env.API_KEY}"   # passthrough from host
```

### `[vars]`

Variables expanded into DOT source before parsing:

```toml
[vars]
language = "typescript"
framework = "react"
```

Used in DOT as `$language`, `$framework`.

### `[checkpoint]`

```toml
[checkpoint]
exclude_globs = ["node_modules/**", ".git/**"]
```

### `[assets]`

```toml
[assets]
include = ["output/**", "*.png"]
```

### `[pull_request]`

```toml
[pull_request]
enabled = true
draft = true
```

### `[[hooks]]`

Lifecycle event hooks:

```toml
[[hooks]]
event = "stage_complete"
type = "command"
command = "echo 'Stage done'"

[[hooks]]
event = "run_complete"
type = "http"
url = "https://example.com/webhook"
```

Events: run_start, run_complete, run_failed, stage_start, stage_complete, stage_failed, stage_retrying, edge_selected, parallel_start, parallel_complete, sandbox_ready, sandbox_cleanup, checkpoint_saved, pre_tool_use, post_tool_use, post_tool_use_failure.

### `[mcp_servers]`

```toml
[mcp_servers.my-server]
transport = "stdio"
command = "node"
args = ["server.js"]
```

## Precedence (first match wins)

Node-level attribute > Stylesheet > TOML config > CLI flags > Server defaults > DOT graph attributes > Built-in defaults

## Validation

```bash
fabro preflight workflow.toml   # validate without executing
```
