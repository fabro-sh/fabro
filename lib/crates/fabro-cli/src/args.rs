use std::fmt;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use fabro_agent::cli::AgentArgs;
use fabro_graphviz::render::GraphFormat;
use fabro_llm::cli::ModelsCommand;

pub(crate) const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("FABRO_GIT_SHA"),
    " ",
    env!("FABRO_BUILD_DATE"),
    ")"
);

#[derive(Args)]
pub(crate) struct GlobalArgs {
    /// Output as JSON
    #[arg(long, global = true, env = "FABRO_JSON", value_parser = clap::builder::BoolishValueParser::new())]
    pub json: bool,

    /// Enable DEBUG-level logging (default is INFO)
    #[arg(long, global = true, env = "FABRO_DEBUG", value_parser = clap::builder::BoolishValueParser::new())]
    pub debug: bool,

    /// Disable automatic upgrade check
    #[arg(long, global = true, env = "FABRO_NO_UPGRADE_CHECK", value_parser = clap::builder::BoolishValueParser::new())]
    pub no_upgrade_check: bool,

    /// Suppress non-essential output
    #[arg(long, global = true, env = "FABRO_QUIET", value_parser = clap::builder::BoolishValueParser::new(), conflicts_with = "verbose")]
    pub quiet: bool,

    /// Enable verbose output
    #[arg(long, global = true, env = "FABRO_VERBOSE", value_parser = clap::builder::BoolishValueParser::new(), conflicts_with = "quiet")]
    pub verbose: bool,

    /// Storage directory (default: ~/.fabro)
    #[arg(long, global = true, env = "FABRO_STORAGE_DIR")]
    pub storage_dir: Option<PathBuf>,

    /// Server URL (overrides server.base_url from user.toml)
    #[arg(
        long,
        global = true,
        env = "FABRO_SERVER_URL",
        conflicts_with = "storage_dir"
    )]
    pub server_url: Option<String>,
}

impl GlobalArgs {
    pub(crate) fn require_no_json(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.json, "--json is not supported for this command");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliSandboxProvider {
    Local,
    Docker,
    Daytona,
}

impl From<CliSandboxProvider> for fabro_sandbox::SandboxProvider {
    fn from(value: CliSandboxProvider) -> Self {
        match value {
            CliSandboxProvider::Local => Self::Local,
            CliSandboxProvider::Docker => Self::Docker,
            CliSandboxProvider::Daytona => Self::Daytona,
        }
    }
}

impl From<fabro_sandbox::SandboxProvider> for CliSandboxProvider {
    fn from(value: fabro_sandbox::SandboxProvider) -> Self {
        match value {
            fabro_sandbox::SandboxProvider::Local => Self::Local,
            fabro_sandbox::SandboxProvider::Docker => Self::Docker,
            fabro_sandbox::SandboxProvider::Daytona => Self::Daytona,
        }
    }
}

#[derive(Args)]
pub(crate) struct RunArgs {
    /// Path to a .fabro workflow file or .toml task config
    #[arg(required = true)]
    pub(crate) workflow: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Auto-approve all human gates
    #[arg(long)]
    pub(crate) auto_approve: bool,

    /// Override the workflow goal (exposed as $goal in prompts)
    #[arg(long)]
    pub(crate) goal: Option<String>,

    /// Read the workflow goal from a file
    #[arg(long, conflicts_with = "goal")]
    pub(crate) goal_file: Option<PathBuf>,

    /// Override default LLM model
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub(crate) provider: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    pub(crate) verbose: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub(crate) sandbox: Option<CliSandboxProvider>,

    /// Attach a label to this run (repeatable, format: KEY=VALUE)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub(crate) label: Vec<String>,

    /// Skip retro generation after the run
    #[arg(long)]
    pub(crate) no_retro: bool,

    /// Keep the sandbox alive after the run finishes (for debugging)
    #[arg(long)]
    pub(crate) preserve_sandbox: bool,

    /// Run the workflow in the background and print the run ID
    #[arg(short = 'd', long)]
    pub(crate) detach: bool,

    /// Pre-generated run ID (used internally by --detach)
    #[arg(long, hide = true)]
    pub(crate) run_id: Option<String>,
}

#[derive(Args)]
pub(crate) struct PreflightArgs {
    /// Path to a .fabro workflow file or .toml task config
    pub(crate) workflow: PathBuf,

    /// Override the workflow goal (exposed as $goal in prompts)
    #[arg(long)]
    pub(crate) goal: Option<String>,

    /// Read the workflow goal from a file
    #[arg(long, conflicts_with = "goal")]
    pub(crate) goal_file: Option<PathBuf>,

    /// Override default LLM model
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub(crate) provider: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    pub(crate) verbose: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub(crate) sandbox: Option<CliSandboxProvider>,
}

#[derive(Args)]
pub(crate) struct RunFilterArgs {
    /// Only include runs started before this date (YYYY-MM-DD prefix match)
    #[arg(long)]
    pub(crate) before: Option<String>,

    /// Filter by workflow name (substring match)
    #[arg(long)]
    pub(crate) workflow: Option<String>,

    /// Filter by label (KEY=VALUE, repeatable, AND semantics)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub(crate) label: Vec<String>,

    /// Include orphan directories (no run.json)
    #[arg(long)]
    pub(crate) orphans: bool,
}

#[derive(Args)]
pub(crate) struct RunsListArgs {
    #[command(flatten)]
    pub(crate) filter: RunFilterArgs,

    /// Show all runs, not just running (like docker ps -a)
    #[arg(short = 'a', long)]
    pub(crate) all: bool,

    /// Only display run IDs
    #[arg(short = 'q', long)]
    pub(crate) quiet: bool,
}

#[derive(Args)]
pub(crate) struct RunsRemoveArgs {
    /// Run IDs or workflow names to remove
    #[arg(required = true)]
    pub(crate) runs: Vec<String>,

    /// Force removal of active runs
    #[arg(short, long)]
    pub(crate) force: bool,
}

#[derive(Args)]
pub(crate) struct LogsArgs {
    /// Run ID prefix or workflow name (most recent run)
    pub(crate) run: String,
    /// Follow log output
    #[arg(short, long)]
    pub(crate) follow: bool,
    /// Logs since timestamp or relative (e.g. "42m", "2h", "2026-01-02T13:00:00Z")
    #[arg(long)]
    pub(crate) since: Option<String>,
    /// Lines from end (default: all)
    #[arg(short = 'n', long)]
    pub(crate) tail: Option<usize>,
    /// Formatted colored output with rendered assistant text
    #[arg(short = 'p', long)]
    pub(crate) pretty: bool,
}

#[derive(Args)]
pub(crate) struct ValidateArgs {
    /// Path to the .fabro workflow file
    pub(crate) workflow: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum GraphDirection {
    /// Left to right
    Lr,
    /// Top to bottom
    Tb,
}

impl fmt::Display for GraphDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lr => write!(f, "LR"),
            Self::Tb => write!(f, "TB"),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum GraphOutputFormat {
    Svg,
    Png,
}

impl From<GraphOutputFormat> for GraphFormat {
    fn from(value: GraphOutputFormat) -> Self {
        match value {
            GraphOutputFormat::Svg => Self::Svg,
            GraphOutputFormat::Png => Self::Png,
        }
    }
}

impl fmt::Display for GraphOutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Svg => write!(f, "svg"),
            Self::Png => write!(f, "png"),
        }
    }
}

#[derive(Args)]
pub(crate) struct GraphArgs {
    /// Path to the .fabro workflow file, .toml task config, or project workflow name
    pub(crate) workflow: PathBuf,

    /// Output format
    #[arg(long, value_enum, default_value_t = GraphOutputFormat::Svg)]
    pub(crate) format: GraphOutputFormat,

    /// Output file path (defaults to stdout)
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,

    /// Graph layout direction (overrides the DOT file's rankdir)
    #[arg(short = 'd', long)]
    pub(crate) direction: Option<GraphDirection>,
}

#[derive(Args)]
pub(crate) struct ParseArgs {
    /// Path to the .fabro workflow file
    pub(crate) workflow: PathBuf,
}

#[derive(Args)]
pub(crate) struct ArtifactListArgs {
    /// Run ID (or prefix)
    pub(crate) run_id: String,

    /// Filter to artifacts from a specific node
    #[arg(long)]
    pub(crate) node: Option<String>,

    /// Filter to artifacts from a specific retry attempt
    #[arg(long)]
    pub(crate) retry: Option<u32>,
}

#[derive(Args)]
pub(crate) struct ArtifactCpArgs {
    /// Source: RUN_ID (all artifacts) or RUN_ID:path (specific artifact)
    pub(crate) source: String,

    /// Destination directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub(crate) dest: PathBuf,

    /// Filter to artifacts from a specific node
    #[arg(long)]
    pub(crate) node: Option<String>,

    /// Filter to artifacts from a specific retry attempt
    #[arg(long)]
    pub(crate) retry: Option<u32>,

    /// Preserve {node_slug}/retry_{N}/ directory structure
    #[arg(long)]
    pub(crate) tree: bool,
}

#[derive(Args)]
pub(crate) struct CpArgs {
    /// Source: <run-id>:<path> or local path
    pub(crate) src: String,
    /// Destination: <run-id>:<path> or local path
    pub(crate) dst: String,
    /// Recurse into directories
    #[arg(short, long)]
    pub(crate) recursive: bool,
}

#[derive(Args)]
pub(crate) struct PreviewArgs {
    /// Run ID or prefix
    pub(crate) run: String,
    /// Port number
    pub(crate) port: u16,
    /// Generate a signed URL (embeds auth token, no headers needed)
    #[arg(long)]
    pub(crate) signed: bool,
    /// Signed URL expiry in seconds (default 3600, requires --signed)
    #[arg(long, default_value = "3600", requires = "signed")]
    pub(crate) ttl: i32,
    /// Open URL in browser (implies --signed)
    #[arg(long)]
    pub(crate) open: bool,
}

#[derive(Args)]
pub(crate) struct SshArgs {
    /// Run ID or prefix
    pub(crate) run: String,
    /// SSH access expiry in minutes (default 60)
    #[arg(long, default_value = "60")]
    pub(crate) ttl: f64,
    /// Print the SSH command instead of connecting
    #[arg(long)]
    pub(crate) print: bool,
}

#[derive(Args)]
pub(crate) struct DiffArgs {
    /// Run ID or prefix
    pub(crate) run: String,
    /// Show diff for a specific node
    #[arg(long)]
    pub(crate) node: Option<String>,
    /// Show diffstat instead of full patch (live diffs only)
    #[arg(long)]
    pub(crate) stat: bool,
    /// Show only files-changed/insertions/deletions summary (live diffs only)
    #[arg(long)]
    pub(crate) shortstat: bool,
}

#[derive(Args)]
pub(crate) struct InspectArgs {
    /// Run ID prefix or workflow name (most recent run)
    pub(crate) run: String,
}

#[derive(Args)]
pub(crate) struct StoreDumpArgs {
    /// Run ID prefix or workflow name
    pub(crate) run: String,

    /// Output directory (must not exist or be empty)
    #[arg(long, short)]
    pub(crate) output: PathBuf,
}

#[derive(Args)]
pub(crate) struct SecretGetArgs {
    /// Name of the secret
    pub(crate) key: String,
}

#[derive(Args)]
pub(crate) struct SecretListArgs {
    /// Show values alongside keys
    #[arg(long)]
    pub(crate) show_values: bool,
}

#[derive(Args)]
pub(crate) struct SecretRmArgs {
    /// Name of the secret to remove
    pub(crate) key: String,
}

#[derive(Args)]
pub(crate) struct SecretSetArgs {
    /// Name of the secret
    pub(crate) key: String,
    /// Value to store
    pub(crate) value: String,
}

#[derive(Debug, Args)]
pub(crate) struct ResumeArgs {
    /// Run ID or unambiguous prefix
    pub(crate) run: String,

    /// Run in the background and print the run ID
    #[arg(short = 'd', long)]
    pub(crate) detach: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RewindArgs {
    /// Run ID (or unambiguous prefix)
    pub(crate) run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit with --list)
    pub(crate) target: Option<String>,

    /// Show the checkpoint timeline instead of rewinding
    #[arg(long)]
    pub(crate) list: bool,

    /// Skip force-pushing rewound refs to the remote
    #[arg(long)]
    pub(crate) no_push: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ForkArgs {
    /// Run ID (or unambiguous prefix)
    pub(crate) run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit to fork from latest)
    pub(crate) target: Option<String>,

    /// Show the checkpoint timeline instead of forking
    #[arg(long)]
    pub(crate) list: bool,

    /// Skip pushing new branches to the remote
    #[arg(long)]
    pub(crate) no_push: bool,
}

#[derive(Args)]
pub(crate) struct WaitArgs {
    /// Run ID prefix or workflow name (most recent run)
    pub(crate) run: String,

    /// Maximum time to wait in seconds
    #[arg(long, value_name = "SECONDS")]
    pub(crate) timeout: Option<u64>,

    /// Poll interval in milliseconds
    #[arg(long, value_name = "MS", default_value = "1000")]
    pub(crate) interval: u64,
}

#[derive(Args)]
pub(crate) struct WorkflowListArgs;

#[derive(Args)]
pub(crate) struct WorkflowCreateArgs {
    /// Name of the workflow
    pub(crate) name: String,

    /// Goal description for the workflow
    #[arg(short, long)]
    pub(crate) goal: Option<String>,
}

#[derive(Args)]
pub(crate) struct ProviderLoginArgs {
    /// LLM provider to authenticate with
    #[arg(long)]
    pub(crate) provider: fabro_model::Provider,
}

#[derive(Args)]
pub(crate) struct RunsPruneArgs {
    #[command(flatten)]
    pub(crate) filter: RunFilterArgs,

    /// Only prune runs older than this duration (e.g. 24h, 7d). Default: 24h when no explicit filters are set.
    #[arg(
        long,
        value_name = "DURATION",
        value_parser = crate::commands::system::parse_duration
    )]
    pub(crate) older_than: Option<chrono::Duration>,

    /// Actually delete (default is dry-run)
    #[arg(long)]
    pub(crate) yes: bool,
}

#[derive(Args)]
pub(crate) struct DfArgs {
    /// Show per-run breakdown
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

#[derive(Args)]
pub(crate) struct SettingsArgs {
    /// Optional workflow name, .fabro path, or .toml run config to overlay
    pub(crate) workflow: Option<PathBuf>,
}

#[derive(Clone, ValueEnum)]
pub(crate) enum SkillDir {
    Claude,
    Agents,
}

#[derive(Clone, ValueEnum)]
pub(crate) enum SkillScope {
    User,
    Project,
}

#[derive(Args)]
pub(crate) struct SkillInstallArgs {
    /// Where to install: user-level or project-level
    #[arg(long = "for", default_value = "user")]
    pub(crate) scope: SkillScope,

    /// Target directory convention
    #[arg(long)]
    pub(crate) dir: SkillDir,

    /// Overwrite existing skill without prompting
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Args)]
pub(crate) struct PrCreateArgs {
    /// Run ID or prefix
    pub(crate) run_id: String,
    /// LLM model for generating PR description
    #[arg(long)]
    pub(crate) model: Option<String>,
}

#[derive(Args)]
pub(crate) struct PrListArgs {
    /// Show all PRs (including closed/merged), not just open
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Args)]
pub(crate) struct PrViewArgs {
    /// Run ID or prefix
    pub(crate) run_id: String,
}

#[derive(Args)]
pub(crate) struct PrMergeArgs {
    /// Run ID or prefix
    pub(crate) run_id: String,
    /// Merge method: merge, squash, or rebase
    #[arg(long, default_value = "squash")]
    pub(crate) method: String,
}

#[derive(Args)]
pub(crate) struct PrCloseArgs {
    /// Run ID or prefix
    pub(crate) run_id: String,
}

#[derive(Args)]
pub(crate) struct UpgradeArgs {
    /// Target version (e.g. "0.5.0" or "v0.5.0")
    #[arg(long)]
    pub(crate) version: Option<String>,

    /// Upgrade even if already on the target version
    #[arg(long)]
    pub(crate) force: bool,

    /// Preview what would happen without making changes
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Subcommand)]
pub(crate) enum RunCommands {
    /// Launch a workflow run
    Run(RunArgs),
    /// Create a workflow run (allocate run dir, persist spec)
    Create(RunArgs),
    /// Start a created workflow run on the server
    Start {
        /// Run ID prefix or workflow name
        run: String,
    },
    /// Attach to a running or finished workflow run
    Attach {
        /// Run ID prefix or workflow name
        run: String,
    },
    /// Internal: queue or resume a workflow run via the server
    #[command(name = "__runner", hide = true)]
    Runner {
        /// Run ID
        #[arg(long)]
        run_id: fabro_types::RunId,
        /// Resume from checkpoint instead of fresh start
        #[arg(long)]
        resume: bool,
    },
    /// Show the diff of changes from a workflow run
    #[command(hide = true)]
    Diff(DiffArgs),
    /// View the event log of a workflow run
    Logs(LogsArgs),
    /// Resume an interrupted workflow run
    Resume(ResumeArgs),
    /// Rewind a workflow run to an earlier checkpoint
    Rewind(RewindArgs),
    /// Fork a workflow run from an earlier checkpoint into a new run
    Fork(ForkArgs),
    /// Block until a workflow run completes
    Wait(WaitArgs),
}

impl RunCommands {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
            Self::Create(_) => "create",
            Self::Start { .. } => "start",
            Self::Attach { .. } => "attach",
            Self::Runner { .. } => "__runner",
            Self::Diff(_) => "diff",
            Self::Logs(_) => "logs",
            Self::Resume(_) => "resume",
            Self::Rewind(_) => "rewind",
            Self::Fork(_) => "fork",
            Self::Wait(_) => "wait",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum SandboxCommand {
    /// Copy files to/from a run's sandbox
    Cp(CpArgs),
    /// Get a preview URL for a port on a run's sandbox
    Preview(PreviewArgs),
    /// SSH into a run's sandbox
    Ssh(SshArgs),
}

impl SandboxCommand {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Cp(_) => "sandbox cp",
            Self::Preview(_) => "sandbox preview",
            Self::Ssh(_) => "sandbox ssh",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RunsCommands {
    /// List workflow runs
    #[command(hide = true)]
    Ps(RunsListArgs),
    /// Remove one or more workflow runs
    Rm(RunsRemoveArgs),
    /// Show detailed information about a workflow run
    Inspect(InspectArgs),
}

impl RunsCommands {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Ps(_) => "ps",
            Self::Rm(_) => "rm",
            Self::Inspect(_) => "inspect",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Run an agentic coding session
    #[command(hide = true)]
    Exec(AgentArgs),
    #[command(flatten)]
    RunCmd(RunCommands),
    /// Validate run configuration without executing
    Preflight(PreflightArgs),
    /// Validate a workflow
    Validate(ValidateArgs),
    /// Render a workflow graph as SVG or PNG
    Graph(GraphArgs),
    /// Parse a DOT file and print its AST
    #[command(hide = true)]
    Parse(ParseArgs),
    /// Inspect and copy run artifacts (screenshots, reports, traces)
    Artifact(ArtifactNamespace),
    /// Export store-backed run state for debugging
    Store(StoreNamespace),
    #[command(flatten)]
    RunsCmd(RunsCommands),
    /// List and test LLM models
    Model {
        #[command(subcommand)]
        command: Option<ModelsCommand>,
    },
    /// Server operations
    Server(ServerNamespace),
    /// Check environment and integration health
    Doctor {
        /// Show detailed information for each check
        #[arg(short, long)]
        verbose: bool,

        /// Skip live service probes (LLM, sandbox, API, web, Brave Search)
        #[arg(long)]
        dry_run: bool,
    },
    /// Set up the Fabro environment (LLMs, certs, GitHub)
    Install {
        /// Base URL for the web UI (used for OAuth callback URLs)
        #[arg(long, default_value = "http://localhost:3000")]
        web_url: String,
    },
    /// Pull request operations
    Pr(PrNamespace),
    /// Skill management
    #[command(hide = true)]
    Skill(SkillNamespace),
    /// Manage secrets in ~/.fabro/.env
    Secret(SecretNamespace),
    /// Inspect merged configuration
    Settings(SettingsArgs),
    /// Workflow operations
    Workflow(WorkflowNamespace),
    /// Open the Discord community in the browser
    Discord,
    /// Open the docs website in the browser
    Docs,
    /// Upgrade fabro to the latest version
    Upgrade(UpgradeArgs),
    /// Repository commands
    Repo(RepoNamespace),
    /// Provider operations
    Provider(ProviderNamespace),
    /// Sandbox operations (cp, ssh, preview)
    Sandbox {
        #[command(subcommand)]
        command: SandboxCommand,
    },
    /// Generate shell completions
    Completion(CompletionArgs),
    /// System maintenance commands
    System(SystemNamespace),
    /// Send a queued analytics event (internal)
    #[command(name = "__send_analytics", hide = true)]
    SendAnalytics {
        /// Path to the JSON event file
        path: PathBuf,
    },
    /// Send a queued panic event to Sentry (internal)
    #[command(name = "__send_panic", hide = true)]
    SendPanic {
        /// Path to the JSON event file
        path: PathBuf,
    },
    /// Build a panic event and write JSON to stdout (internal testing)
    #[cfg(debug_assertions)]
    #[command(name = "__test_panic", hide = true)]
    TestPanic {
        /// Panic message
        message: String,
    },
}

impl Commands {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Artifact(ns) => match &ns.command {
                ArtifactCommand::List(_) => "artifact list",
                ArtifactCommand::Cp(_) => "artifact cp",
            },
            Self::Store(ns) => match &ns.command {
                StoreCommand::Dump(_) => "store dump",
            },
            Self::Exec(_) => "exec",
            Self::RunCmd(cmd) => cmd.name(),
            Self::Preflight(_) => "preflight",
            Self::Validate(_) => "validate",
            Self::Graph(_) => "graph",
            Self::Parse(_) => "parse",
            Self::RunsCmd(cmd) => cmd.name(),
            Self::Model { command } => match command {
                Some(ModelsCommand::List { .. }) => "model list",
                Some(ModelsCommand::Test { .. }) => "model test",
                None => "model",
            },
            Self::Server(ns) => match &ns.command {
                ServerCommand::Start { .. } => "server start",
                ServerCommand::Stop { .. } => "server stop",
                ServerCommand::Status { .. } => "server status",
                ServerCommand::Serve { .. } => "server __serve",
            },
            Self::Doctor { .. } => "doctor",
            Self::Repo(ns) => match &ns.command {
                RepoCommand::Init { .. } => "repo init",
                RepoCommand::Deinit => "repo deinit",
            },
            Self::Install { .. } => "install",
            Self::Pr(ns) => match &ns.command {
                PrCommand::Create(_) => "pr create",
                PrCommand::List(_) => "pr list",
                PrCommand::View(_) => "pr view",
                PrCommand::Merge(_) => "pr merge",
                PrCommand::Close(_) => "pr close",
            },
            Self::Secret(ns) => match &ns.command {
                SecretCommand::Get(_) => "secret get",
                SecretCommand::List(_) => "secret list",
                SecretCommand::Rm(_) => "secret rm",
                SecretCommand::Set(_) => "secret set",
            },
            Self::Settings(_) => "settings",
            Self::Workflow(ns) => match &ns.command {
                WorkflowCommand::List(_) => "workflow list",
                WorkflowCommand::Create(_) => "workflow create",
            },
            Self::Skill(ns) => match &ns.command {
                SkillCommand::Install(_) => "skill install",
            },
            Self::Discord => "discord",
            Self::Docs => "docs",
            Self::Upgrade(_) => "upgrade",
            Self::Provider(ns) => match &ns.command {
                ProviderCommand::Login(_) => "provider login",
            },
            Self::Sandbox { command } => command.name(),
            Self::Completion(_) => "completion",
            Self::System(ns) => match &ns.command {
                SystemCommand::Prune(_) => "system prune",
                SystemCommand::Df(_) => "system df",
            },
            Self::SendAnalytics { .. } => "__send_analytics",
            Self::SendPanic { .. } => "__send_panic",
            #[cfg(debug_assertions)]
            Self::TestPanic { .. } => "__test_panic",
        }
    }
}

#[derive(Args)]
pub(crate) struct PrNamespace {
    #[command(subcommand)]
    pub(crate) command: PrCommand,
}

#[derive(Subcommand)]
pub(crate) enum PrCommand {
    /// Create a pull request from a completed run
    Create(PrCreateArgs),
    /// List pull requests from workflow runs
    List(PrListArgs),
    /// View pull request details
    View(PrViewArgs),
    /// Merge a pull request
    Merge(PrMergeArgs),
    /// Close a pull request
    Close(PrCloseArgs),
}

#[derive(Args)]
pub(crate) struct ArtifactNamespace {
    #[command(subcommand)]
    pub(crate) command: ArtifactCommand,
}

#[derive(Subcommand)]
pub(crate) enum ArtifactCommand {
    /// List artifacts for a workflow run
    List(ArtifactListArgs),
    /// Copy artifacts from a workflow run
    Cp(ArtifactCpArgs),
}

#[derive(Args)]
pub(crate) struct StoreNamespace {
    #[command(subcommand)]
    pub(crate) command: StoreCommand,
}

#[derive(Subcommand)]
pub(crate) enum StoreCommand {
    /// Export a run's durable state to a directory
    Dump(StoreDumpArgs),
}

#[derive(Args)]
pub(crate) struct SecretNamespace {
    #[command(subcommand)]
    pub(crate) command: SecretCommand,
}

#[derive(Subcommand)]
pub(crate) enum SecretCommand {
    /// Get a secret value
    Get(SecretGetArgs),
    /// List secret names
    #[command(alias = "ls")]
    List(SecretListArgs),
    /// Remove a secret
    Rm(SecretRmArgs),
    /// Set a secret value
    Set(SecretSetArgs),
}

#[derive(Args)]
pub(crate) struct ServerNamespace {
    #[command(subcommand)]
    pub(crate) command: ServerCommand,
}

use fabro_server::serve::ServeArgs;

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// Start the HTTP API server
    Start {
        /// Run in the foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,

        #[command(flatten)]
        serve_args: ServeArgs,
    },
    /// Stop the HTTP API server
    Stop {
        /// Seconds to wait for graceful shutdown before SIGKILL
        #[arg(long, default_value = "10")]
        timeout: u64,
    },
    /// Show server status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Internal: run the server process (spawned by `start`)
    #[command(name = "__serve", hide = true)]
    Serve {
        /// Path to the server record file
        #[arg(long)]
        record_path: PathBuf,

        #[command(flatten)]
        serve_args: ServeArgs,
    },
}

#[derive(Args)]
pub(crate) struct SystemNamespace {
    #[command(subcommand)]
    pub(crate) command: SystemCommand,
}

#[derive(Subcommand)]
pub(crate) enum SystemCommand {
    /// Delete old workflow runs
    Prune(RunsPruneArgs),
    /// Show disk usage
    Df(DfArgs),
}

#[derive(Args)]
pub(crate) struct WorkflowNamespace {
    #[command(subcommand)]
    pub(crate) command: WorkflowCommand,
}

#[derive(Subcommand)]
pub(crate) enum WorkflowCommand {
    /// List available workflows
    List(WorkflowListArgs),
    /// Create a new workflow
    Create(WorkflowCreateArgs),
}

#[derive(Args)]
pub(crate) struct RepoNamespace {
    #[command(subcommand)]
    pub(crate) command: RepoCommand,
}

#[derive(Subcommand)]
pub(crate) enum RepoCommand {
    /// Initialize a new project
    Init {
        /// Also install the fabro-create-workflow skill
        #[arg(long, hide = true)]
        skill: bool,
    },
    /// Remove fabro.toml and fabro/ directory
    Deinit,
}

#[derive(Args)]
pub(crate) struct ProviderNamespace {
    #[command(subcommand)]
    pub(crate) command: ProviderCommand,
}

#[derive(Subcommand)]
pub(crate) enum ProviderCommand {
    /// Log in to an LLM provider
    Login(ProviderLoginArgs),
}

#[derive(Args)]
pub(crate) struct CompletionArgs {
    /// Shell to generate completions for
    pub shell: clap_complete::Shell,
}

#[derive(Args)]
pub(crate) struct SkillNamespace {
    #[command(subcommand)]
    pub(crate) command: SkillCommand,
}

#[derive(Subcommand)]
pub(crate) enum SkillCommand {
    /// Install a built-in skill
    Install(SkillInstallArgs),
}
