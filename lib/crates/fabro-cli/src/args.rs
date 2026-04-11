use std::fmt;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use fabro_agent::cli::AgentArgs;
use fabro_graphviz::render::GraphFormat;

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
}

impl GlobalArgs {
    pub(crate) fn require_no_json(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.json, "--json is not supported for this command");
        Ok(())
    }
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct StorageDirArgs {
    /// Local storage directory (default: ~/.fabro/storage)
    #[arg(long, env = "FABRO_STORAGE_DIR")]
    pub(crate) storage_dir: Option<PathBuf>,
}

impl StorageDirArgs {
    pub(crate) fn as_deref(&self) -> Option<&Path> {
        self.storage_dir.as_deref()
    }

    pub(crate) fn clone_path(&self) -> Option<PathBuf> {
        self.storage_dir.clone()
    }
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ServerTargetArgs {
    /// Fabro server target: http(s) URL or absolute Unix socket path
    #[arg(long = "server", env = "FABRO_SERVER")]
    pub(crate) server: Option<String>,
}

impl ServerTargetArgs {
    pub(crate) fn as_deref(&self) -> Option<&str> {
        self.server.as_deref()
    }
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ServerConnectionArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,
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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Path to a .fabro workflow file or .toml task config
    #[arg(required = true)]
    pub(crate) workflow: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Auto-approve all human gates
    #[arg(long)]
    pub(crate) auto_approve: bool,

    /// Override the workflow goal (available as {{ goal }} in prompts)
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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Path to a .fabro workflow file or .toml task config
    pub(crate) workflow: PathBuf,

    /// Override the workflow goal (available as {{ goal }} in prompts)
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

    /// Include orphan directories (no matching durable run)
    #[arg(long)]
    pub(crate) orphans: bool,
}

#[derive(Args)]
pub(crate) struct RunsListArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run IDs or workflow names to remove
    #[arg(required = true)]
    pub(crate) runs: Vec<String>,

    /// Force removal of active runs
    #[arg(short, long)]
    pub(crate) force: bool,
}

#[derive(Args)]
pub(crate) struct LogsArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID prefix or workflow name (most recent run)
    pub(crate) run: String,
    /// Follow log output
    #[arg(short, long)]
    pub(crate) follow: bool,
    /// Logs since timestamp or relative (e.g. "42m", "2h",
    /// "2026-01-02T13:00:00Z")
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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Path to the .fabro workflow file, .toml task config, or project workflow
    /// name
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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or prefix
    pub(crate) run: String,
    /// Show diff for a specific node
    #[arg(long)]
    pub(crate) node: Option<String>,
}

#[derive(Args)]
pub(crate) struct InspectArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID prefix or workflow name (most recent run)
    pub(crate) run: String,
}

#[derive(Args)]
pub(crate) struct StoreDumpArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Run ID prefix or workflow name
    pub(crate) run: String,

    /// Output directory (must not exist or be empty)
    #[arg(long, short)]
    pub(crate) output: PathBuf,
}

#[derive(Args)]
pub(crate) struct SecretListArgs;

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or unambiguous prefix
    pub(crate) run: String,

    /// Run in the background and print the run ID
    #[arg(short = 'd', long)]
    pub(crate) detach: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RewindArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID (or unambiguous prefix)
    pub(crate) run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit to fork from
    /// latest)
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
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// LLM provider to authenticate with
    #[arg(long)]
    pub(crate) provider: fabro_model::Provider,
}

#[derive(Args)]
pub(crate) struct SystemInfoArgs {
    #[command(flatten)]
    pub(crate) connection: ServerConnectionArgs,
}

#[derive(Args)]
pub(crate) struct RunsPruneArgs {
    #[command(flatten)]
    pub(crate) connection: ServerConnectionArgs,

    #[command(flatten)]
    pub(crate) filter: RunFilterArgs,

    /// Only prune runs older than this duration (e.g. 24h, 7d). Default: 24h
    /// when no explicit filters are set.
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
    #[command(flatten)]
    pub(crate) connection: ServerConnectionArgs,

    /// Show per-run breakdown
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

#[derive(Args)]
pub(crate) struct SystemEventsArgs {
    #[command(flatten)]
    pub(crate) connection: ServerConnectionArgs,

    /// Filter by run ID (repeatable)
    #[arg(long = "run-id")]
    pub(crate) run_ids: Vec<String>,
}

#[derive(Args)]
pub(crate) struct SettingsArgs {
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Show only locally resolved settings and skip the server call
    #[arg(long, conflicts_with = "server")]
    pub(crate) local: bool,

    /// Optional workflow name, .fabro path, or .toml run config to overlay
    pub(crate) workflow: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct PrCreateArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or prefix
    pub(crate) run_id: String,
    /// LLM model for generating PR description
    #[arg(long)]
    pub(crate) model: Option<String>,
}

#[derive(Args)]
pub(crate) struct PrListArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Show all PRs (including closed/merged), not just open
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Args)]
pub(crate) struct PrViewArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or prefix
    pub(crate) run_id: String,
}

#[derive(Args)]
pub(crate) struct PrMergeArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or prefix
    pub(crate) run_id: String,
    /// Merge method: merge, squash, or rebase
    #[arg(long, default_value = "squash")]
    pub(crate) method: String,
}

#[derive(Args)]
pub(crate) struct PrCloseArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID or prefix
    pub(crate) run_id: String,
}

#[derive(Args)]
pub(crate) struct StartArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID prefix or workflow name
    pub(crate) run: String,
}

#[derive(Args)]
pub(crate) struct AttachArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    /// Run ID prefix or workflow name
    pub(crate) run: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum RunWorkerMode {
    Start,
    Resume,
}

#[derive(Args)]
pub(crate) struct RunWorkerArgs {
    /// Fabro server target: http(s) URL or absolute Unix socket path
    #[arg(long)]
    pub(crate) server: String,

    /// Short-lived bearer token for artifact uploads
    #[arg(long, hide = true)]
    pub(crate) artifact_upload_token: Option<String>,

    /// Run scratch directory
    #[arg(long)]
    pub(crate) run_dir: PathBuf,

    /// Run ID
    #[arg(long)]
    pub(crate) run_id: fabro_types::RunId,

    /// Worker mode
    #[arg(long, value_enum)]
    pub(crate) mode: RunWorkerMode,
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ModelListArgs {
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Filter by provider
    #[arg(short, long)]
    pub(crate) provider: Option<String>,

    /// Search for models matching this string
    #[arg(short, long)]
    pub(crate) query: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ModelTestArgs {
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Filter by provider
    #[arg(short, long)]
    pub(crate) provider: Option<String>,

    /// Test a specific model
    #[arg(short, long)]
    pub(crate) model: Option<String>,

    /// Run a multi-turn tool-use test (catches reasoning round-trip bugs)
    #[arg(long)]
    pub(crate) deep: bool,
}

#[derive(Args)]
pub(crate) struct ExecArgs {
    #[command(flatten)]
    pub(crate) server: ServerTargetArgs,

    #[command(flatten)]
    pub(crate) agent: AgentArgs,
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
    Start(StartArgs),
    /// Attach to a running or finished workflow run
    Attach(AttachArgs),
    /// Internal: execute a single workflow run locally
    #[command(name = "__run-worker", hide = true)]
    RunWorker(RunWorkerArgs),
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
            Self::Start(_) => "start",
            Self::Attach(_) => "attach",
            Self::RunWorker(_) => "__run-worker",
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
pub(crate) enum ModelsCommand {
    /// List available models
    List(ModelListArgs),

    /// Test model availability by sending a simple prompt
    Test(ModelTestArgs),
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Run an agentic coding session
    #[command(hide = true)]
    Exec(ExecArgs),
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
    Doctor(DoctorArgs),
    /// Set up the Fabro environment (LLMs, certs, GitHub)
    Install(InstallArgs),
    /// Uninstall Fabro from this machine
    Uninstall(UninstallArgs),
    /// Pull request operations
    Pr(PrNamespace),
    /// Manage server-owned secrets
    Secret(SecretNamespace),
    /// Inspect effective settings
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
                Some(ModelsCommand::List(_)) => "model list",
                Some(ModelsCommand::Test(_)) => "model test",
                None => "model",
            },
            Self::Server(ns) => match &ns.command {
                ServerCommand::Start(_) => "server start",
                ServerCommand::Stop(_) => "server stop",
                ServerCommand::Status(_) => "server status",
                ServerCommand::Serve(_) => "server __serve",
            },
            Self::Doctor(_) => "doctor",
            Self::Repo(ns) => match &ns.command {
                RepoCommand::Init(_) => "repo init",
                RepoCommand::Deinit => "repo deinit",
            },
            Self::Install(_) => "install",
            Self::Uninstall(_) => "uninstall",
            Self::Pr(ns) => match &ns.command {
                PrCommand::Create(_) => "pr create",
                PrCommand::List(_) => "pr list",
                PrCommand::View(_) => "pr view",
                PrCommand::Merge(_) => "pr merge",
                PrCommand::Close(_) => "pr close",
            },
            Self::Secret(ns) => match &ns.command {
                SecretCommand::List(_) => "secret list",
                SecretCommand::Rm(_) => "secret rm",
                SecretCommand::Set(_) => "secret set",
            },
            Self::Settings(_) => "settings",
            Self::Workflow(ns) => match &ns.command {
                WorkflowCommand::List(_) => "workflow list",
                WorkflowCommand::Create(_) => "workflow create",
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
                SystemCommand::Info(_) => "system info",
                SystemCommand::Prune(_) => "system prune",
                SystemCommand::Df(_) => "system df",
                SystemCommand::Events(_) => "system events",
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
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    #[command(subcommand)]
    pub(crate) command: SecretCommand,
}

#[derive(Subcommand)]
pub(crate) enum SecretCommand {
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

#[derive(Args)]
pub(crate) struct ServerStartArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Run in the foreground instead of daemonizing
    #[arg(long)]
    pub(crate) foreground: bool,

    #[command(flatten)]
    pub(crate) serve_args: ServeArgs,
}

#[derive(Args)]
pub(crate) struct ServerStopArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Seconds to wait for graceful shutdown before SIGKILL
    #[arg(long, default_value = "10")]
    pub(crate) timeout: u64,
}

#[derive(Args)]
pub(crate) struct ServerStatusArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Output as JSON
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Args)]
pub(crate) struct ServerServeArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Path to the server record file
    #[arg(long)]
    pub(crate) record_path: PathBuf,

    #[command(flatten)]
    pub(crate) serve_args: ServeArgs,
}

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// Start the HTTP API server
    Start(ServerStartArgs),
    /// Stop the HTTP API server
    Stop(ServerStopArgs),
    /// Show server status
    Status(ServerStatusArgs),
    /// Internal: run the server process (spawned by `start`)
    #[command(name = "__serve", hide = true)]
    Serve(ServerServeArgs),
}

#[derive(Args)]
pub(crate) struct SystemNamespace {
    #[command(subcommand)]
    pub(crate) command: SystemCommand,
}

#[derive(Subcommand)]
pub(crate) enum SystemCommand {
    /// Show server runtime information
    Info(SystemInfoArgs),
    /// Delete old workflow runs
    Prune(RunsPruneArgs),
    /// Show disk usage
    Df(DfArgs),
    /// Stream run events from the server
    Events(SystemEventsArgs),
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
    Init(RepoInitArgs),
    /// Remove .fabro/ project directory
    Deinit,
}

#[derive(Args)]
pub(crate) struct RepoInitArgs {
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,
}

#[derive(Args)]
pub(crate) struct DoctorArgs {
    #[command(flatten)]
    pub(crate) target: ServerTargetArgs,

    /// Show detailed information for each check
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

#[derive(Args)]
pub(crate) struct InstallArgs {
    #[command(flatten)]
    pub(crate) storage_dir: StorageDirArgs,

    /// Base URL for the web UI (used for OAuth callback URLs)
    #[arg(long, default_value = "http://localhost:3000")]
    pub(crate) web_url: String,
}

#[derive(Args)]
pub(crate) struct UninstallArgs {
    /// Skip confirmation prompt
    #[arg(long)]
    pub(crate) yes: bool,
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
