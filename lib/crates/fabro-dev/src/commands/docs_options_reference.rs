use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_options_metadata::{OptionField, OptionSet};

use super::{markdown_cell, replace_generated_region};

const OPTIONS_REFERENCE_PATH: &str = "docs/public/reference/user-configuration.mdx";
const FENCE_START: &str = "{/* generated:options */}";
const FENCE_END: &str = "{/* /generated:options */}";

#[expect(
    clippy::print_stdout,
    clippy::disallowed_methods,
    reason = "dev generator reports the generated docs path directly and intentionally uses sync filesystem I/O"
)]
pub(crate) fn docs_options_reference_root(root: &Path, check: bool) -> Result<()> {
    let path = root.join(OPTIONS_REFERENCE_PATH);
    let current =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let generated = render_options_reference();
    let updated = replace_generated_region(
        &current,
        &generated,
        OPTIONS_REFERENCE_PATH,
        FENCE_START,
        FENCE_END,
    )?;

    if check {
        if current != updated {
            bail!("{OPTIONS_REFERENCE_PATH} is stale; run `cargo dev docs refresh`");
        }
        println!("{OPTIONS_REFERENCE_PATH} is up to date.");
        return Ok(());
    }

    if current != updated {
        std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    }
    println!("Generated {OPTIONS_REFERENCE_PATH}.");
    Ok(())
}

struct Section {
    path:    &'static str,
    set:     OptionSet,
    example: &'static str,
}

impl Section {
    fn of<T>(path: &'static str, example: &'static str) -> Self
    where
        T: fabro_options_metadata::OptionsMetadata + 'static,
    {
        Self {
            path,
            set: OptionSet::of::<T>(),
            example,
        }
    }
}

fn render_options_reference() -> String {
    let mut output = String::new();
    render_manual_cli_target(&mut output);

    for section in metadata_sections() {
        render_section(&mut output, &section);
    }

    render_manual_mcp(&mut output);
    output.trim_end().to_string()
}

fn metadata_sections() -> Vec<Section> {
    vec![
        Section::of::<fabro_config::CliUpdatesLayer>(
            "[cli.updates]",
            r"[cli.updates]
check = true",
        ),
        Section::of::<fabro_config::CliOutputLayer>(
            "[cli.output]",
            r#"[cli.output]
format = "text"
verbosity = "verbose""#,
        ),
        Section::of::<fabro_config::CliExecLayer>(
            "[cli.exec]",
            r"[cli.exec]
prevent_idle_sleep = true",
        ),
        Section::of::<fabro_config::CliExecModelLayer>(
            "[cli.exec.model]",
            r#"[cli.exec.model]
provider = "anthropic"
name = "claude-opus-4-6""#,
        ),
        Section::of::<fabro_config::CliExecAgentLayer>(
            "[cli.exec.agent]",
            r#"[cli.exec.agent]
permissions = "read-write""#,
        ),
        Section::of::<fabro_config::RunModelLayer>(
            "[run.model]",
            r#"[run.model]
provider = "anthropic"
name = "claude-sonnet-4-5"
fallbacks = ["openai", "gpt-5.4"]"#,
        ),
        Section::of::<fabro_config::CliLoggingLayer>(
            "[cli.logging]",
            r#"[cli.logging]
level = "info""#,
        ),
        Section::of::<fabro_config::GitAuthorLayer>(
            "[run.git.author]",
            r#"[run.git.author]
name = "fabro-bot"
email = "fabro-bot@company.com""#,
        ),
        Section::of::<fabro_config::RunPullRequestLayer>(
            "[run.pull_request]",
            r"[run.pull_request]
enabled = true",
        ),
        Section::of::<fabro_config::RunAgentLayer>(
            "[run.agent]",
            r#"[run.agent]
permissions = "read-write""#,
        ),
    ]
}

fn render_section(output: &mut String, section: &Section) {
    output.push_str("## `");
    output.push_str(section.path);
    output.push_str("`\n\n");

    if let Some(doc) = section.set.documentation() {
        output.push_str(&normalize_doc(doc));
        output.push_str("\n\n");
    }

    output.push_str("```toml title=\"settings.toml\"\n");
    output.push_str(section.example);
    output.push_str("\n```\n\n");
    render_field_table(output, section.set.fields());
}

fn render_field_table(output: &mut String, fields: BTreeMap<String, OptionField>) {
    output.push_str("| Key | Type / values | Default | Description |\n");
    output.push_str("|---|---|---|---|\n");
    for (name, field) in fields {
        output.push_str("| `");
        output.push_str(&name);
        output.push_str("` | ");
        output.push_str(&field_type(&field));
        output.push_str(" | ");
        output.push_str(field.default.unwrap_or("None"));
        output.push_str(" | ");
        output.push_str(&markdown_cell(
            field.doc.unwrap_or("TODO: add settings help text."),
        ));
        output.push_str(" |\n");
    }
    output.push('\n');
}

fn field_type(field: &OptionField) -> String {
    if let Some(possible_values) = field
        .possible_values
        .as_ref()
        .filter(|values| !values.is_empty())
    {
        possible_values
            .iter()
            .map(|value| format!("`{}`", value.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        field
            .value_type
            .map_or_else(|| "inferred".to_string(), markdown_cell)
    }
}

fn render_manual_cli_target(output: &mut String) {
    output.push_str(
        r#"## `[cli.target]`

Connection info for commands that target a remote Fabro server.

```toml title="settings.toml"
[cli.target]
type = "http"
url = "https://fabro.example.com/api/v1"
```

| Key | Type / values | Default | Description |
|---|---|---|---|
| `type` | `"http"` \| `"unix"` | None | Explicit transport selection. |
| `url` | string | None | Required for `type = "http"`; the API base URL. |
| `path` | string | None | Required for `type = "unix"`; the absolute Unix socket path. |

"#,
    );
}

fn render_manual_mcp(output: &mut String) {
    output.push_str(
        r#"## `[run.agent.mcps.<name>]`

Configure MCP servers for workflow agents. For `fabro exec`-only MCPs, use `[cli.exec.agent.mcps.<name>]` with the same shape.

```toml title="settings.toml"
[run.agent.mcps.filesystem]
type = "stdio"
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
startup_timeout = "15s"
tool_timeout = "90s"
```

| Key | Type / values | Default | Description |
|---|---|---|---|
| `type` | `"stdio"` \| `"http"` \| `"sandbox"` | None | MCP transport type. |
| `command` | array<string> | None | Command and arguments for `stdio` or `sandbox` transports. |
| `script` | string | None | Shell script alternative to `command` for process-launching transports. |
| `url` | string | None | Remote MCP URL for `http` transport. |
| `port` | integer | None | Sandbox port for `sandbox` transport. |
| `env` | table | `{}` | Additional environment variables for process-launching transports. |
| `headers` | table | `{}` | HTTP headers for `http` transport. |
| `startup_timeout` | duration | `"10s"` | Max duration for startup and MCP handshake. |
| `tool_timeout` | duration | `"60s"` | Max duration for a single MCP tool call. |

See [MCP](/agents/mcp) for transport-specific examples.
"#,
    );
}

fn normalize_doc(doc: &str) -> String {
    doc.trim().trim_end_matches('.').to_string()
}
