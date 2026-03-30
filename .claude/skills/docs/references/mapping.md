# Code-to-Doc Mapping

Which source files affect which doc pages. Use this as guidance — also apply judgment for unmapped files that clearly affect user-facing behavior.

| Source | Docs |
|--------|------|
| `lib/crates/fabro-cli/src/main.rs`, `lib/crates/fabro-workflows/src/cli/mod.rs`, `lib/crates/fabro-workflows/src/cli/run.rs` | `docs/reference/cli.mdx` |
| `lib/crates/fabro-cli/src/cli_config.rs` | `docs/reference/cli-configuration.mdx` |
| `lib/crates/fabro-llm/src/cli.rs` | `docs/reference/cli.mdx` |
| `lib/crates/fabro-api/src/serve.rs` | `docs/reference/cli.mdx` |
| `lib/crates/fabro-workflows/src/parser/*.rs` | `docs/reference/dot-language.mdx` |
| `lib/crates/fabro-workflows/src/condition.rs` | `docs/reference/dot-language.mdx` |
| `lib/crates/fabro-workflows/src/cli/validate.rs` | `docs/reference/dot-language.mdx` |
| `lib/crates/fabro-workflows/src/stylesheet.rs` | `docs/workflows/stylesheets.mdx` |
| `lib/crates/fabro-workflows/src/transform.rs` | `docs/workflows/variables.mdx` |
| `lib/crates/fabro-workflows/src/handler/*.rs` | `docs/workflows/stages-and-nodes.mdx`, `docs/reference/dot-language.mdx` |
| `lib/crates/fabro-workflows/src/handler/human.rs` | `docs/workflows/human-in-the-loop.mdx` |
| `lib/crates/fabro-workflows/src/cli/run_config.rs` | `docs/execution/run-configuration.mdx` |
| `lib/crates/fabro-workflows/src/engine.rs` | `docs/core-concepts/how-arc-works.mdx` |
| `lib/crates/fabro-workflows/src/context/*.rs` | `docs/execution/context.mdx` |
| `lib/crates/fabro-workflows/src/checkpoint.rs` | `docs/execution/checkpoints.mdx` |
| `lib/crates/fabro-workflows/src/retro.rs`, `lib/crates/fabro-workflows/src/retro_agent.rs` | `docs/execution/retros.mdx` |
| `lib/crates/fabro-workflows/src/interviewer/*.rs` | `docs/execution/interviews.mdx` |
| `lib/crates/fabro-workflows/src/hook/*.rs` | `docs/agents/hooks.mdx` |
| `lib/crates/fabro-workflows/src/daytona_sandbox.rs` | `docs/integrations/daytona.mdx`, `docs/execution/environments.mdx` |
| `lib/crates/fabro-agent/src/tools.rs`, `lib/crates/fabro-agent/src/tool_registry.rs`, `lib/crates/fabro-agent/src/tool_execution.rs` | `docs/agents/tools.mdx` |
| `lib/crates/fabro-agent/src/v4a_patch.rs` | `docs/agents/tools.mdx` |
| `lib/crates/fabro-agent/src/cli.rs` | `docs/agents/permissions.mdx` |
| `lib/crates/fabro-agent/src/subagent.rs` | `docs/agents/subagents.mdx` |
| `lib/crates/fabro-agent/src/mcp_integration.rs` | `docs/agents/mcp.mdx` |
| `lib/crates/fabro-llm/src/catalog.rs`, `lib/crates/fabro-llm/src/providers/*.rs` | `docs/core-concepts/models.mdx` |
| `lib/crates/fabro-devcontainer/src/*.rs` | `docs/execution/devcontainers.mdx` |
| `lib/crates/fabro-slack/src/*.rs` | `docs/integrations/slack.mdx` |
| `lib/crates/fabro-mcp/src/*.rs` | `docs/agents/mcp.mdx` |
| `lib/crates/fabro-api/src/*.rs` | `docs/api-reference/overview.mdx`, `docs/api-reference/demo-mode.mdx` |
| `lib/crates/fabro-api/src/server_config.rs` | `docs/administration/server-configuration.mdx` |
