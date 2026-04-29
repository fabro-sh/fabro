# Code-to-Doc Mapping

Which source files affect which doc pages. Use this as guidance — also apply judgment for unmapped files that clearly affect user-facing behavior.

| Source | Docs |
|--------|------|
| `lib/crates/fabro-cli/src/main.rs`, `lib/crates/fabro-workflow/src/cli/mod.rs`, `lib/crates/fabro-workflow/src/cli/run.rs` | `docs/public/reference/cli.mdx` |
| `lib/crates/fabro-cli/src/cli_config.rs` | `docs/public/reference/cli-configuration.mdx` |
| `lib/crates/fabro-llm/src/cli.rs` | `docs/public/reference/cli.mdx` |
| `lib/crates/fabro-api/src/serve.rs` | `docs/public/reference/cli.mdx` |
| `lib/crates/fabro-workflow/src/parser/*.rs` | `docs/public/reference/dot-language.mdx` |
| `lib/crates/fabro-workflow/src/condition.rs` | `docs/public/reference/dot-language.mdx` |
| `lib/crates/fabro-workflow/src/cli/validate.rs` | `docs/public/reference/dot-language.mdx` |
| `lib/crates/fabro-workflow/src/stylesheet.rs` | `docs/public/workflows/stylesheets.mdx` |
| `lib/crates/fabro-workflow/src/transform.rs` | `docs/public/workflows/variables.mdx` |
| `lib/crates/fabro-workflow/src/handler/*.rs` | `docs/public/workflows/stages-and-nodes.mdx`, `docs/public/reference/dot-language.mdx` |
| `lib/crates/fabro-workflow/src/handler/human.rs` | `docs/public/workflows/human-in-the-loop.mdx` |
| `lib/crates/fabro-workflow/src/cli/run_config.rs` | `docs/public/execution/run-configuration.mdx` |
| `lib/crates/fabro-workflow/src/engine.rs` | `docs/public/core-concepts/how-arc-works.mdx` |
| `lib/crates/fabro-workflow/src/context/*.rs` | `docs/public/execution/context.mdx` |
| `lib/crates/fabro-workflow/src/checkpoint.rs` | `docs/public/execution/checkpoints.mdx` |
| `lib/crates/fabro-workflow/src/retro.rs`, `lib/crates/fabro-workflow/src/retro_agent.rs` | `docs/public/execution/retros.mdx` |
| `lib/crates/fabro-workflow/src/interviewer/*.rs` | `docs/public/execution/interviews.mdx` |
| `lib/crates/fabro-workflow/src/hook/*.rs` | `docs/public/agents/hooks.mdx` |
| `lib/crates/fabro-workflow/src/daytona_sandbox.rs` | `docs/public/integrations/daytona.mdx`, `docs/public/execution/environments.mdx` |
| `lib/crates/fabro-agent/src/tools.rs`, `lib/crates/fabro-agent/src/tool_registry.rs`, `lib/crates/fabro-agent/src/tool_execution.rs` | `docs/public/agents/tools.mdx` |
| `lib/crates/fabro-agent/src/v4a_patch.rs` | `docs/public/agents/tools.mdx` |
| `lib/crates/fabro-agent/src/cli.rs` | `docs/public/agents/permissions.mdx` |
| `lib/crates/fabro-agent/src/subagent.rs` | `docs/public/agents/subagents.mdx` |
| `lib/crates/fabro-agent/src/mcp_integration.rs` | `docs/public/agents/mcp.mdx` |
| `lib/crates/fabro-llm/src/catalog.rs`, `lib/crates/fabro-llm/src/providers/*.rs` | `docs/public/core-concepts/models.mdx` |
| `lib/crates/fabro-devcontainer/src/*.rs` | `docs/public/execution/devcontainers.mdx` |
| `lib/crates/fabro-slack/src/*.rs` | `docs/public/integrations/slack.mdx` |
| `lib/crates/fabro-mcp/src/*.rs` | `docs/public/agents/mcp.mdx` |
| `lib/crates/fabro-api/src/*.rs` | `docs/public/api-reference/overview.mdx`, `docs/public/api-reference/demo-mode.mdx` |
| `lib/crates/fabro-api/src/server_config.rs` | `docs/public/administration/server-configuration.mdx` |
