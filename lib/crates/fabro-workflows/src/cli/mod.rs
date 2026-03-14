pub mod backend;
pub mod cli_backend;
pub mod cp;
pub mod diff;
pub mod graph;
pub mod logs;
pub mod parse;
pub mod pr;
pub mod preview;
pub mod progress;
pub mod project_config;
pub mod rewind;
pub mod run;
pub mod run_config;
pub mod runs;
pub mod ssh;
pub mod validate;
pub mod workflow;

use std::path::Path;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fabro_util::terminal::Styles;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::outcome::StageUsage;
use crate::validation::{Diagnostic, Severity};

/// Sandbox provider for agent tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SandboxProvider {
    /// Run tools on the local host (default)
    #[default]
    Local,
    /// Run tools inside a Docker container
    Docker,
    /// Run tools inside a Daytona cloud sandbox
    Daytona,
    /// Run tools inside an exe.dev VM
    #[cfg(feature = "exedev")]
    Exe,
    /// Run tools on a user-provided SSH host
    Ssh,
}

impl SandboxProvider {
    pub fn is_remote(&self) -> bool {
        match self {
            Self::Daytona => true,
            #[cfg(feature = "exedev")]
            Self::Exe => true,
            Self::Ssh => true,
            _ => false,
        }
    }
}

impl fmt::Display for SandboxProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Docker => write!(f, "docker"),
            Self::Daytona => write!(f, "daytona"),
            #[cfg(feature = "exedev")]
            Self::Exe => write!(f, "exe"),
            Self::Ssh => write!(f, "ssh"),
        }
    }
}

impl FromStr for SandboxProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "docker" => Ok(Self::Docker),
            "daytona" => Ok(Self::Daytona),
            #[cfg(feature = "exedev")]
            "exe" => Ok(Self::Exe),
            "ssh" => Ok(Self::Ssh),
            other => Err(format!("unknown sandbox provider: {other}")),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "fabro-workflows",
    version,
    about = "Workflow runner for AI workflows"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Launch a workflow from a .fabro or .toml task file
    Run(RunArgs),
    /// Parse and validate a workflow without executing
    Validate(ValidateArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to a .fabro workflow file or .toml task config (not required with --run-branch)
    #[arg(required_unless_present = "run_branch")]
    pub workflow: Option<PathBuf>,

    /// Run output directory
    #[arg(long)]
    pub run_dir: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

    /// Validate run configuration without executing
    #[arg(long, conflicts_with_all = ["resume", "run_branch", "dry_run"])]
    pub preflight: bool,

    /// Auto-approve all human gates
    #[arg(long)]
    pub auto_approve: bool,

    /// Resume from a checkpoint file
    #[arg(long)]
    pub resume: Option<PathBuf>,

    /// Resume from a git run branch (reads checkpoint and graph from metadata branch)
    #[arg(long, conflicts_with = "resume")]
    pub run_branch: Option<String>,

    /// Override the workflow goal (exposed as $goal in prompts)
    #[arg(long)]
    pub goal: Option<String>,

    /// Read the workflow goal from a file
    #[arg(long, conflicts_with = "goal")]
    pub goal_file: Option<PathBuf>,

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub sandbox: Option<SandboxProvider>,

    /// Attach a label to this run (repeatable, format: KEY=VALUE)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,

    /// Skip retro generation after the run
    #[arg(long)]
    pub no_retro: bool,

    /// Create SSH access to the Daytona sandbox and print the connection command
    #[arg(long)]
    pub ssh: bool,

    /// Keep the sandbox alive after the run finishes (for debugging)
    #[arg(long)]
    pub preserve_sandbox: bool,

    /// Run the workflow in the background and print the run ID
    #[arg(short = 'd', long, conflicts_with_all = ["resume", "run_branch", "preflight"])]
    pub detach: bool,

    /// Pre-generated run ID (used internally by --detach)
    #[arg(long, hide = true)]
    pub run_id: Option<String>,
}

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the .fabro workflow file
    pub workflow: PathBuf,
}

#[derive(Args)]
pub struct ParseArgs {
    /// Path to the .fabro workflow file
    pub workflow: PathBuf,
}

/// Read a workflow file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn read_workflow_file(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", path.display()))
}

/// Print diagnostics to stderr, colored by severity.
pub fn print_diagnostics(diagnostics: &[Diagnostic], styles: &Styles) {
    for d in diagnostics {
        let location = match (&d.node_id, &d.edge) {
            (Some(node), _) => format!(" [node: {node}]"),
            (_, Some((from, to))) => format!(" [edge: {from} -> {to}]"),
            _ => String::new(),
        };
        match d.severity {
            Severity::Error => eprintln!(
                "{}{location}: {} ({})",
                styles.red.apply_to("error"),
                d.message,
                styles.dim.apply_to(&d.rule),
            ),
            Severity::Warning => eprintln!(
                "{}{location}: {} ({})",
                styles.yellow.apply_to("warning"),
                d.message,
                styles.dim.apply_to(&d.rule),
            ),
            Severity::Info => eprintln!(
                "{}",
                styles
                    .dim
                    .apply_to(format!("info{location}: {} ({})", d.message, d.rule)),
            ),
        }
    }
}

/// Compute the dollar cost for a stage's token usage, if pricing is available.
#[must_use]
pub fn compute_stage_cost(usage: &StageUsage) -> Option<f64> {
    let info = fabro_llm::catalog::get_model_info(&usage.model)?;
    let input_rate = info.costs.input_cost_per_mtok?;
    let output_rate = info.costs.output_cost_per_mtok?;
    Some(
        usage.input_tokens as f64 * input_rate / 1_000_000.0
            + usage.output_tokens as f64 * output_rate / 1_000_000.0,
    )
}

/// Format a dollar cost for display (e.g. `"$1.23"`).
#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

/// Format a token count for human display (e.g. `"850"`, `"15.2k"`, `"3.4m"`).
#[must_use]
pub fn format_tokens_human(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

/// Produce a relative path from cwd; falls back to `tilde_path` if not under cwd.
pub fn relative_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    tilde_path(path)
}

/// Shorten an absolute path by replacing the home directory prefix with `~`.
pub fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(suffix) = path.strip_prefix(&home) {
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_provider_default_is_local() {
        assert_eq!(SandboxProvider::default(), SandboxProvider::Local);
    }

    #[test]
    fn sandbox_provider_from_str() {
        assert_eq!(
            "local".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Local
        );
        assert_eq!(
            "docker".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Docker
        );
        assert_eq!(
            "daytona".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Daytona
        );
        assert_eq!(
            "LOCAL".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Local
        );
        #[cfg(feature = "exedev")]
        {
            assert_eq!(
                "exe".parse::<SandboxProvider>().unwrap(),
                SandboxProvider::Exe
            );
            assert_eq!(
                "EXE".parse::<SandboxProvider>().unwrap(),
                SandboxProvider::Exe
            );
        }
        assert_eq!(
            "ssh".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Ssh
        );
        assert_eq!(
            "SSH".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Ssh
        );
        assert!("invalid".parse::<SandboxProvider>().is_err());
    }

    #[test]
    fn sandbox_provider_display() {
        assert_eq!(SandboxProvider::Local.to_string(), "local");
        assert_eq!(SandboxProvider::Docker.to_string(), "docker");
        assert_eq!(SandboxProvider::Daytona.to_string(), "daytona");
        #[cfg(feature = "exedev")]
        assert_eq!(SandboxProvider::Exe.to_string(), "exe");
        assert_eq!(SandboxProvider::Ssh.to_string(), "ssh");
    }

    #[test]
    fn format_cost_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
    }

    #[test]
    fn format_cost_normal() {
        assert_eq!(format_cost(1.5), "$1.50");
    }

    #[test]
    fn format_cost_rounds() {
        assert_eq!(format_cost(123.456), "$123.46");
    }

    #[test]
    fn format_tokens_human_zero() {
        assert_eq!(format_tokens_human(0), "0");
    }

    #[test]
    fn format_tokens_human_small() {
        assert_eq!(format_tokens_human(999), "999");
    }

    #[test]
    fn format_tokens_human_thousands() {
        assert_eq!(format_tokens_human(1000), "1.0k");
    }

    #[test]
    fn format_tokens_human_mid_thousands() {
        assert_eq!(format_tokens_human(15234), "15.2k");
    }

    #[test]
    fn format_tokens_human_millions() {
        assert_eq!(format_tokens_human(1_000_000), "1.0m");
    }

    #[test]
    fn format_tokens_human_mid_millions() {
        assert_eq!(format_tokens_human(3_456_789), "3.5m");
    }

    #[test]
    fn compute_stage_cost_known_model() {
        let usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        };
        let cost = compute_stage_cost(&usage);
        assert!(cost.is_some());
        assert!(cost.unwrap() > 0.0);
    }

    #[test]
    fn compute_stage_cost_unknown_model() {
        let usage = StageUsage {
            model: "nonexistent-model-xyz".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        };
        assert_eq!(compute_stage_cost(&usage), None);
    }
}
