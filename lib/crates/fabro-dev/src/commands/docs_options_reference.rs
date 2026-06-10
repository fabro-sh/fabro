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
    render_manual_llm_catalog(&mut output);

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
fabro_tools = true
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

fn render_manual_llm_catalog(output: &mut String) {
    output.push_str(
        r#"## `[llm.providers.<id>]`

Define or override an LLM provider. Provider IDs are strings, so custom
providers can be added when they use an adapter Fabro already supports.

```toml title="settings.toml"
[llm.providers.proxy]
display_name = "Acme Gateway"
adapter = "openai_compatible"
base_url = "https://llm-gateway.example.com/v1"
priority = 50
enabled = true
aliases = ["gateway"]

[llm.providers.proxy.auth]
credentials = ["env:ACME_GATEWAY_API_KEY", "vault:ACME_GATEWAY_API_KEY"]

[llm.providers.proxy.extra_headers]
x-portkey-api-key = { env = "PORTKEY_API_KEY" }
x-portkey-config = { literal = "@bedrock-prod" }
x-team-secret = { vault = "gateway_team_secret" }
```

| Key | Type / values | Default | Description |
|---|---|---|---|
| `display_name` | string | provider ID | Human-readable provider name. |
| `adapter` | string | built-in value | Adapter registry key, such as `"anthropic"`, `"openai"`, `"gemini"`, or `"openai_compatible"`. Required for new providers. |
| `agent_profile` | `"anthropic"` \| `"openai"` \| `"gemini"` | derived from `adapter` | Agent profile used for project memory, CLI/ACP command selection, and native session routing. Override only when a provider needs profile behavior different from its adapter. |
| `billing_policy` | `"openai"` \| `"anthropic"` \| `"gemini"` \| `"none"` | derived from `adapter` | Provider-owned billing algorithm for usage estimates. Override for exceptional providers such as local no-billing runtimes. |
| `base_url` | string | built-in value or adapter runtime default | Provider API base URL. Required for most custom OpenAI-compatible providers. |
| `auth` | table | omitted | API-key auth config. Omit the table entirely for providers that need no API key; any `extra_headers` are still attached. |
| `auth.credentials` | array<string> | required when `auth` present | Ordered credential refs. Accepted forms are `vault:<NAME>` and `env:<NAME>`. Literal secret strings are rejected. |
| `auth.header` | `"bearer"` or `{ custom = "Header-Name" }` | `"bearer"` | Primary API-key header policy. Omit when the provider uses a standard bearer token. |
| `extra_headers` | table | `{}` | Additional headers attached to provider requests. Values must be typed refs: `{ literal = "..." }`, `{ env = "NAME" }`, or `{ vault = "NAME" }`. |
| `priority` | integer | `0` | Higher-priority configured providers win default selection; ties use canonical provider ID. |
| `enabled` | boolean | `true` | Set `false` to disable a provider after lower-precedence layers define it. |
| `aliases` | array<string> | `[]` | Additional provider names accepted by model routing and fallback config. |

## `[llm.models.<id>]`

Define or override a model in the catalog. The table key is the canonical
model ID Fabro users reference; `api_id` is the model string sent to the
provider API.

```toml title="settings.toml"
[llm.models."team-code-large"]
provider = "proxy"
api_id = "provider-wire-model-name"
agent_profile = "anthropic"
display_name = "Team Code Large"
family = "team-code"
default = true
probe = true
enabled = true
aliases = ["team-code"]
estimated_output_tps = 80

[llm.models."team-code-large".limits]
context_window = 200000
max_output = 32000

[llm.models."team-code-large".features]
tools = true
vision = false
reasoning = true
reasoning_effort = "levels"
prompt_cache = true

[llm.models."team-code-large".controls]
reasoning_effort = ["low", "medium", "high"]
speed = ["fast"]

[llm.models."team-code-large".costs]
input_cost_per_mtok = 1.50
output_cost_per_mtok = 8.00
cache_input_cost_per_mtok = 0.30

[llm.models."team-code-large".costs.speed.fast]
input_cost_per_mtok = 3.00
output_cost_per_mtok = 16.00
cache_input_cost_per_mtok = 0.60
```

| Key | Type / values | Default | Description |
|---|---|---|---|
| `provider` | string | None | Provider ID this model belongs to. |
| `api_id` | string | model ID | Identifier sent to the provider API. |
| `agent_profile` | `"anthropic"` \| `"openai"` \| `"gemini"` | provider profile | Agent profile override for this model. Model overrides take precedence over provider overrides. |
| `display_name` | string | model ID | Human-readable model name. |
| `family` | string | model ID | Family label used for catalog display and matching. |
| `training` | string | None | Training data cutoff label. |
| `knowledge_cutoff` | string or TOML date | None | Public knowledge cutoff label; TOML dates normalize to `YYYY-MM-DD`. |
| `default` | boolean | `false` | Whether this is the provider default model. |
| `probe` | boolean | `false` | Whether this model should be preferred for provider connectivity probes. Set `false` in a higher-precedence layer to clear an inherited probe marker. |
| `enabled` | boolean | `true` | Set `false` to disable a model after lower-precedence layers define it. |
| `aliases` | array<string> | `[]` | Additional model names accepted by routing and fallback config. |
| `estimated_output_tps` | number | None | Estimated output tokens per second for catalog display and planning. |

## `[llm.models.<id>.limits]`

| Key | Type / values | Default | Description |
|---|---|---|---|
| `context_window` | integer | None | Maximum context window size in tokens. |
| `max_output` | integer | None | Maximum output tokens, if known. |

## `[llm.models.<id>.features]`

| Key | Type / values | Default | Description |
|---|---|---|---|
| `tools` | boolean | `false` | Whether the model supports tool calls. |
| `vision` | boolean | `false` | Whether the model accepts image inputs. |
| `reasoning` | boolean | `false` | Whether the model has reasoning behavior. |
| `reasoning_effort` | `"levels"` \| `"always_adaptive"` \| `"none"` | `"none"` | Whether the model endpoint supports a native reasoning-effort parameter. `levels` accepts discrete effort levels; `always_adaptive` accepts effort levels with natively always-on adaptive thinking; `none` has no native effort parameter. |
| `prompt_cache` | boolean | `false` | Whether prompt cache pricing/usage applies. |
| `sampling_params` | boolean | `true` | Whether the model accepts classic sampling parameters (`temperature`, `top_p`). |

## `[llm.models.<id>.controls]`

| Key | Type / values | Default | Description |
|---|---|---|---|
| `reasoning_effort` | array<string> | all standard levels when feature is `"levels"` or `"always_adaptive"` | User-facing reasoning effort values Fabro may send for this model. Can be set explicitly for reasoning models whose provider adapter maps effort to a non-native API shape. |
| `speed` | array<string> | `[]` | Additional speeds beyond implicit `standard`; do not list `standard`. |

## `[llm.models.<id>.costs]`

| Key | Type / values | Default | Description |
|---|---|---|---|
| `input_cost_per_mtok` | number | None | Input cost in USD per million tokens. |
| `output_cost_per_mtok` | number | None | Output cost in USD per million tokens. |
| `cache_input_cost_per_mtok` | number | None | Cached input/read cost in USD per million tokens. |

## `[llm.models.<id>.costs.speed.<speed>]`

Per-speed cost overrides use the same keys as `[llm.models.<id>.costs]`.
Each `<speed>` key must be declared in `[llm.models.<id>.controls].speed`.
The `standard` speed is implicit and always uses the base cost table.

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
