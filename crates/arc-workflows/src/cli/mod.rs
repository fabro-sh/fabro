pub mod backend;
pub mod cli_backend;
pub mod progress;
pub mod run;
pub mod run_config;
pub mod runs;
pub mod validate;

use std::path::Path;

use arc_util::terminal::Styles;
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::HumanBytes;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::event::WorkflowRunEvent;
use crate::outcome::StageUsage;
use crate::validation::{Diagnostic, Severity};
use arc_agent::AgentEvent;

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
}

impl fmt::Display for SandboxProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Docker => write!(f, "docker"),
            Self::Daytona => write!(f, "daytona"),
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
            other => Err(format!("unknown sandbox provider: {other}")),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "arc-workflows",
    version,
    about = "DOT-based workflow runner for AI workflows"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Launch a workflow from a .dot or .toml task file
    Run(RunArgs),
    /// Parse and validate a workflow without executing
    Validate(ValidateArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to a .dot workflow file or .toml task config (not required with --run-branch)
    #[arg(required_unless_present = "run_branch")]
    pub workflow: Option<PathBuf>,

    /// Log/artifact directory
    #[arg(long)]
    pub logs_dir: Option<PathBuf>,

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
}

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the .dot workflow file
    pub workflow: PathBuf,
}

/// Read a .dot file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn read_dot_file(path: &Path) -> anyhow::Result<String> {
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

/// One-line summary of a workflow run event for `-v` output (dimmed).
#[must_use]
pub fn format_event_summary(event: &WorkflowRunEvent, styles: &Styles) -> String {
    let body = match event {
        WorkflowRunEvent::WorkflowRunStarted { name, run_id, .. } => {
            format!("[WORKFLOW_RUN_STARTED] name={name} id={run_id}")
        }
        WorkflowRunEvent::WorkflowRunCompleted {
            duration_ms,
            artifact_count,
            total_cost,
            ..
        } => {
            let mut s = format!(
                "[WORKFLOW_RUN_COMPLETED] duration={duration_ms}ms artifacts={artifact_count}"
            );
            if let Some(cost) = total_cost {
                s.push_str(&format!(" total_cost={}", format_cost(*cost)));
            }
            s
        }
        WorkflowRunEvent::WorkflowRunFailed {
            error, duration_ms, ..
        } => {
            format!("[WORKFLOW_RUN_FAILED] error=\"{error}\" duration={duration_ms}ms")
        }
        WorkflowRunEvent::StageStarted {
            node_id,
            name,
            index,
            handler_type,
            attempt,
            max_attempts,
        } => {
            let mut s = format!("[STAGE_STARTED] node_id={node_id} name={name} index={index}");
            if let Some(ht) = handler_type {
                s.push_str(&format!(" handler_type={ht}"));
            }
            s.push_str(&format!(" attempt={attempt}/{max_attempts}"));
            s
        }
        WorkflowRunEvent::StageCompleted {
            node_id,
            name,
            index,
            duration_ms,
            status,
            preferred_label,
            suggested_next_ids,
            usage,
            failure,
            notes,
            files_touched,
            attempt,
            max_attempts,
        } => {
            let mut s = format!("[STAGE_COMPLETED] node_id={node_id} name={name} index={index} duration={duration_ms}ms status={status}");
            if let Some(label) = preferred_label {
                s.push_str(&format!(" preferred_label=\"{label}\""));
            }
            if !suggested_next_ids.is_empty() {
                s.push_str(&format!(
                    " suggested_next_ids={}",
                    suggested_next_ids.join(",")
                ));
            }
            if let Some(u) = usage {
                let total = u.input_tokens + u.output_tokens;
                let tokens_str = format_tokens_human(total);
                if let Some(cost) = compute_stage_cost(u) {
                    s.push_str(&format!(" tokens={tokens_str} cost={}", format_cost(cost)));
                } else {
                    s.push_str(&format!(" tokens={tokens_str}"));
                }
            }
            if let Some(ref f) = failure {
                s.push_str(&format!(" failure_reason=\"{}\"", f.message));
                s.push_str(&format!(" failure_class={}", f.failure_class));
            }
            if let Some(n) = notes {
                s.push_str(&format!(" notes=\"{n}\""));
            }
            if !files_touched.is_empty() {
                s.push_str(&format!(" files_touched={}", files_touched.len()));
            }
            s.push_str(&format!(" attempt={attempt}/{max_attempts}"));
            s
        }
        WorkflowRunEvent::StageFailed {
            node_id,
            name,
            index,
            failure,
            will_retry,
        } => {
            format!(
                "[STAGE_FAILED] node_id={node_id} name={name} index={index} error=\"{}\" will_retry={will_retry} failure_class={}",
                failure.message, failure.failure_class
            )
        }
        WorkflowRunEvent::StageRetrying {
            node_id,
            name,
            index,
            attempt,
            max_attempts,
            delay_ms,
        } => {
            format!(
                "[STAGE_RETRYING] node_id={node_id} name={name} index={index} attempt={attempt}/{max_attempts} delay={delay_ms}ms"
            )
        }
        WorkflowRunEvent::ParallelStarted {
            branch_count,
            join_policy,
            error_policy,
        } => {
            format!("[PARALLEL_STARTED] branches={branch_count} join_policy={join_policy} error_policy={error_policy}")
        }
        WorkflowRunEvent::ParallelBranchStarted { branch, index } => {
            format!("[PARALLEL_BRANCH_STARTED] branch={branch} index={index}")
        }
        WorkflowRunEvent::ParallelBranchCompleted {
            branch,
            index,
            duration_ms,
            status,
        } => {
            format!("[PARALLEL_BRANCH_COMPLETED] branch={branch} index={index} duration={duration_ms}ms status={status}")
        }
        WorkflowRunEvent::ParallelCompleted {
            duration_ms,
            success_count,
            failure_count,
        } => {
            format!("[PARALLEL_COMPLETED] duration={duration_ms}ms succeeded={success_count} failed={failure_count}")
        }
        WorkflowRunEvent::InterviewStarted {
            question,
            stage,
            question_type,
        } => {
            format!("[INTERVIEW_STARTED] stage={stage} question=\"{question}\" question_type={question_type}")
        }
        WorkflowRunEvent::InterviewCompleted {
            question,
            answer,
            duration_ms,
        } => {
            format!(
                "[INTERVIEW_COMPLETED] question=\"{question}\" answer=\"{answer}\" duration={duration_ms}ms"
            )
        }
        WorkflowRunEvent::InterviewTimeout {
            stage, duration_ms, ..
        } => {
            format!("[INTERVIEW_TIMEOUT] stage={stage} duration={duration_ms}ms")
        }
        WorkflowRunEvent::CheckpointSaved { node_id } => {
            format!("[CHECKPOINT_SAVED] node={node_id}")
        }
        WorkflowRunEvent::GitCheckpoint {
            node_id,
            git_commit_sha,
            status,
            ..
        } => {
            format!("[GIT_CHECKPOINT] node={node_id} sha={git_commit_sha} status={status}")
        }
        WorkflowRunEvent::EdgeSelected {
            from_node,
            to_node,
            label,
            condition,
        } => {
            let mut s = format!("[EDGE_SELECTED] from={from_node} to={to_node}");
            if let Some(l) = label {
                s.push_str(&format!(" label=\"{l}\""));
            }
            if let Some(c) = condition {
                s.push_str(&format!(" condition=\"{c}\""));
            }
            s
        }
        WorkflowRunEvent::LoopRestart { from_node, to_node } => {
            format!("[LOOP_RESTART] from={from_node} to={to_node}")
        }
        WorkflowRunEvent::Prompt { stage, text } => {
            let truncated = if text.len() > 80 { &text[..80] } else { text };
            format!("[PROMPT] stage={stage} text=\"{truncated}\"")
        }
        WorkflowRunEvent::Agent { stage, event } => match event {
            AgentEvent::AssistantMessage {
                model,
                usage,
                tool_call_count,
                ..
            } => {
                let total = usage.input_tokens + usage.output_tokens;
                let tokens_str = format_tokens_human(total);
                let mut s = format!("[ASSISTANT_MESSAGE] stage={stage} model={model} tokens={tokens_str} tool_calls={tool_call_count}");
                if let Some(cache_read) = usage.cache_read_tokens {
                    s.push_str(&format!(" cache_read={}", format_tokens_human(cache_read)));
                }
                if let Some(reasoning) = usage.reasoning_tokens {
                    s.push_str(&format!(" reasoning={}", format_tokens_human(reasoning)));
                }
                s
            }
            AgentEvent::ToolCallStarted { tool_name, .. } => {
                format!("[TOOL_CALL_STARTED] stage={stage} tool={tool_name}")
            }
            AgentEvent::ToolCallCompleted {
                tool_name,
                is_error,
                ..
            } => {
                format!("[TOOL_CALL_COMPLETED] stage={stage} tool={tool_name} is_error={is_error}")
            }
            AgentEvent::Error { error } => {
                format!("[SESSION_ERROR] stage={stage} error=\"{error}\"")
            }
            AgentEvent::ContextWindowWarning { usage_percent, .. } => {
                format!("[CONTEXT_WINDOW_WARNING] stage={stage} usage={usage_percent}%")
            }
            AgentEvent::LoopDetected => format!("[LOOP_DETECTED] stage={stage}"),
            AgentEvent::TurnLimitReached { max_turns } => {
                format!("[TURN_LIMIT_REACHED] stage={stage} max_turns={max_turns}")
            }
            AgentEvent::CompactionStarted {
                estimated_tokens,
                context_window_size,
            } => {
                format!("[COMPACTION_STARTED] stage={stage} estimated_tokens={estimated_tokens} context_window={context_window_size}")
            }
            AgentEvent::CompactionCompleted {
                original_turn_count,
                preserved_turn_count,
                summary_token_estimate,
                tracked_file_count,
            } => {
                format!("[COMPACTION_COMPLETED] stage={stage} original_turns={original_turn_count} preserved_turns={preserved_turn_count} summary_tokens={summary_token_estimate} tracked_files={tracked_file_count}")
            }
            AgentEvent::LlmRetry {
                provider,
                model,
                attempt,
                delay_secs,
                error,
            } => {
                let delay_ms = (*delay_secs * 1000.0) as u64;
                format!("[LLM_RETRY] stage={stage} provider={provider} model={model} attempt={attempt} delay={delay_ms}ms error=\"{error}\"")
            }
            AgentEvent::SubAgentSpawned {
                agent_id,
                depth,
                task,
            } => {
                let short_id = &agent_id[..8.min(agent_id.len())];
                let task_preview = if task.len() > 60 { &task[..60] } else { task };
                format!("[SUBAGENT_SPAWNED] stage={stage} agent_id={short_id} depth={depth} task=\"{task_preview}\"")
            }
            AgentEvent::SubAgentCompleted {
                agent_id,
                depth,
                success,
                turns_used,
            } => {
                let short_id = &agent_id[..8.min(agent_id.len())];
                format!("[SUBAGENT_COMPLETED] stage={stage} agent_id={short_id} depth={depth} success={success} turns={turns_used}")
            }
            AgentEvent::SubAgentFailed {
                agent_id,
                depth,
                error,
            } => {
                let short_id = &agent_id[..8.min(agent_id.len())];
                format!("[SUBAGENT_FAILED] stage={stage} agent_id={short_id} depth={depth} error=\"{error}\"")
            }
            AgentEvent::SubAgentClosed { agent_id, depth } => {
                let short_id = &agent_id[..8.min(agent_id.len())];
                format!("[SUBAGENT_CLOSED] stage={stage} agent_id={short_id} depth={depth}")
            }
            AgentEvent::SubAgentEvent {
                agent_id,
                depth,
                event,
            } => {
                let short_id = &agent_id[..8.min(agent_id.len())];
                format!("[SUBAGENT_EVENT] stage={stage} agent_id={short_id} depth={depth} event={event:?}")
            }
            other => format!("[AGENT] stage={stage} event={other:?}"),
        },
        WorkflowRunEvent::ParallelEarlyTermination {
            reason,
            completed_count,
            pending_count,
        } => {
            format!("[PARALLEL_EARLY_TERMINATION] reason={reason} completed={completed_count} pending={pending_count}")
        }
        WorkflowRunEvent::SubgraphStarted {
            node_id,
            start_node,
        } => {
            format!("[SUBGRAPH_STARTED] node={node_id} start_node={start_node}")
        }
        WorkflowRunEvent::SubgraphCompleted {
            node_id,
            steps_executed,
            status,
            duration_ms,
        } => {
            format!("[SUBGRAPH_COMPLETED] node={node_id} steps={steps_executed} status={status} duration={duration_ms}ms")
        }
        WorkflowRunEvent::Sandbox { event } => {
            use arc_agent::SandboxEvent;
            match event {
                SandboxEvent::Initializing { provider } => format!("[SANDBOX_INITIALIZING] provider={provider}"),
                SandboxEvent::Ready { provider, duration_ms } => format!("[SANDBOX_READY] provider={provider} duration={duration_ms}ms"),
                SandboxEvent::InitializeFailed { provider, error, duration_ms } => format!("[SANDBOX_INIT_FAILED] provider={provider} error=\"{error}\" duration={duration_ms}ms"),
                SandboxEvent::CleanupStarted { provider } => format!("[SANDBOX_CLEANUP_STARTED] provider={provider}"),
                SandboxEvent::CleanupCompleted { provider, duration_ms } => format!("[SANDBOX_CLEANUP_COMPLETED] provider={provider} duration={duration_ms}ms"),
                SandboxEvent::CleanupFailed { provider, error } => format!("[SANDBOX_CLEANUP_FAILED] provider={provider} error=\"{error}\""),
                SandboxEvent::SnapshotPulling { name } => format!("[SANDBOX_SNAPSHOT_PULLING] name={name}"),
                SandboxEvent::SnapshotPulled { name, duration_ms } => format!("[SANDBOX_SNAPSHOT_PULLED] name={name} duration={duration_ms}ms"),
                SandboxEvent::SnapshotEnsuring { name } => format!("[SANDBOX_SNAPSHOT_ENSURING] name={name}"),
                SandboxEvent::SnapshotCreating { name } => format!("[SANDBOX_SNAPSHOT_CREATING] name={name}"),
                SandboxEvent::SnapshotReady { name, duration_ms } => format!("[SANDBOX_SNAPSHOT_READY] name={name} duration={duration_ms}ms"),
                SandboxEvent::SnapshotFailed { name, error } => format!("[SANDBOX_SNAPSHOT_FAILED] name={name} error=\"{error}\""),
                SandboxEvent::GitCloneStarted { url, branch } => {
                    let branch_str = branch.as_deref().unwrap_or("(default)");
                    format!("[SANDBOX_GIT_CLONE_STARTED] url={url} branch={branch_str}")
                }
                SandboxEvent::GitCloneCompleted { url, duration_ms } => format!("[SANDBOX_GIT_CLONE_COMPLETED] url={url} duration={duration_ms}ms"),
                SandboxEvent::GitCloneFailed { url, error } => format!("[SANDBOX_GIT_CLONE_FAILED] url={url} error=\"{error}\""),
            }
        }
        WorkflowRunEvent::SetupStarted { command_count } => {
            format!("[SETUP_STARTED] command_count={command_count}")
        }
        WorkflowRunEvent::SetupCommandStarted { command, index } => {
            format!("[SETUP_COMMAND_STARTED] index={index} command=\"{command}\"")
        }
        WorkflowRunEvent::SetupCommandCompleted {
            command,
            index,
            exit_code,
            duration_ms,
        } => {
            format!("[SETUP_COMMAND_COMPLETED] index={index} command=\"{command}\" exit_code={exit_code} duration={duration_ms}ms")
        }
        WorkflowRunEvent::SetupCompleted { duration_ms } => {
            format!("[SETUP_COMPLETED] duration={duration_ms}ms")
        }
        WorkflowRunEvent::SetupFailed {
            command,
            index,
            exit_code,
            stderr,
        } => {
            let truncated = if stderr.len() > 80 {
                &stderr[..80]
            } else {
                stderr
            };
            format!("[SETUP_FAILED] index={index} command=\"{command}\" exit_code={exit_code} stderr=\"{truncated}\"")
        }
        WorkflowRunEvent::StallWatchdogTimeout { node, idle_seconds } => {
            format!("[STALL_WATCHDOG_TIMEOUT] node={node} idle_seconds={idle_seconds}")
        }
        WorkflowRunEvent::AssetsCaptured {
            node_id,
            files_copied,
            total_bytes,
            files_skipped,
        } => {
            format!("[ASSETS_CAPTURED] node={node_id} files_copied={files_copied} total_bytes={} files_skipped={files_skipped}", HumanBytes(*total_bytes))
        }
    };
    format!("{}", styles.dim.apply_to(body))
}

/// Compute the dollar cost for a stage's token usage, if pricing is available.
#[must_use]
pub fn compute_stage_cost(usage: &StageUsage) -> Option<f64> {
    let info = arc_llm::catalog::get_model_info(&usage.model)?;
    let input_rate = info.input_cost_per_million?;
    let output_rate = info.output_cost_per_million?;
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
        assert!("invalid".parse::<SandboxProvider>().is_err());
    }

    #[test]
    fn sandbox_provider_display() {
        assert_eq!(SandboxProvider::Local.to_string(), "local");
        assert_eq!(SandboxProvider::Docker.to_string(), "docker");
        assert_eq!(SandboxProvider::Daytona.to_string(), "daytona");
    }

    fn test_styles() -> &'static Styles {
        static STYLES: std::sync::LazyLock<Styles> =
            std::sync::LazyLock::new(|| Styles::new(false));
        &STYLES
    }

    #[test]
    fn format_summary_sandbox_initializing() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::Initializing {
                provider: "docker".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_INITIALIZING]"));
        assert!(s.contains("docker"));
    }

    #[test]
    fn format_summary_setup_started() {
        let event = WorkflowRunEvent::SetupStarted { command_count: 3 };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_STARTED]"));
        assert!(s.contains("3"));
    }

    #[test]
    fn format_summary_subagent_spawned() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentSpawned {
                agent_id: "abcdef12-3456-7890-abcd-ef1234567890".into(),
                depth: 1,
                task: "list files".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBAGENT_SPAWNED]"));
        assert!(s.contains("abcdef12"));
        assert!(s.contains("depth=1"));
    }

    #[test]
    fn format_summary_subagent_completed() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentCompleted {
                agent_id: "abcdef12-xxxx".into(),
                depth: 1,
                success: true,
                turns_used: 5,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBAGENT_COMPLETED]"));
        assert!(s.contains("success=true"));
        assert!(s.contains("turns=5"));
    }

    #[test]
    fn format_summary_subagent_event() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentEvent {
                agent_id: "abcdef12-xxxx".into(),
                depth: 1,
                event: Box::new(AgentEvent::SessionStarted),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBAGENT_EVENT]"));
        assert!(s.contains("abcdef12"));
    }

    // ── Helper function tests ──────────────────────────────────────────

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

    // ── compute_stage_cost tests ───────────────────────────────────────

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

    // ── format_event_summary tests ─────────────────────────────────────

    #[test]
    fn format_summary_workflow_run_started() {
        let event = WorkflowRunEvent::WorkflowRunStarted {
            name: "test-wf".into(),
            run_id: "run-123".into(),
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[WORKFLOW_RUN_STARTED]"));
        assert!(s.contains("test-wf"));
        assert!(s.contains("run-123"));
    }

    #[test]
    fn format_summary_workflow_run_completed_no_cost() {
        let event = WorkflowRunEvent::WorkflowRunCompleted {
            duration_ms: 5000,
            artifact_count: 3,
            total_cost: None,
            final_git_commit_sha: None,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[WORKFLOW_RUN_COMPLETED]"));
        assert!(s.contains("5000"));
        assert!(s.contains("artifacts=3"));
    }

    #[test]
    fn format_summary_workflow_run_completed_with_cost() {
        let event = WorkflowRunEvent::WorkflowRunCompleted {
            duration_ms: 5000,
            artifact_count: 3,
            total_cost: Some(1.5),
            final_git_commit_sha: None,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[WORKFLOW_RUN_COMPLETED]"));
        assert!(s.contains("total_cost=$1.50"));
    }

    #[test]
    fn format_summary_workflow_run_failed() {
        let event = WorkflowRunEvent::WorkflowRunFailed {
            error: crate::error::ArcError::Parse("bad input".into()),
            duration_ms: 1000,
            git_commit_sha: None,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[WORKFLOW_RUN_FAILED]"));
        assert!(s.contains("1000"));
    }

    #[test]
    fn format_summary_stage_started_no_handler() {
        let event = WorkflowRunEvent::StageStarted {
            node_id: "n1".into(),
            name: "build".into(),
            index: 0,
            handler_type: None,
            attempt: 1,
            max_attempts: 3,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_STARTED]"));
        assert!(s.contains("node_id=n1"));
        assert!(s.contains("name=build"));
        assert!(s.contains("attempt=1/3"));
    }

    #[test]
    fn format_summary_stage_started_with_handler() {
        let event = WorkflowRunEvent::StageStarted {
            node_id: "n1".into(),
            name: "build".into(),
            index: 0,
            handler_type: Some("agent".into()),
            attempt: 1,
            max_attempts: 3,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_STARTED]"));
        assert!(s.contains("handler_type=agent"));
    }

    #[test]
    fn format_summary_stage_completed_with_usage() {
        let event = WorkflowRunEvent::StageCompleted {
            node_id: "n1".into(),
            name: "code".into(),
            index: 0,
            duration_ms: 2000,
            status: "success".into(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: Some(StageUsage {
                model: "nonexistent-model".into(),
                input_tokens: 500,
                output_tokens: 300,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                cost: None,
            }),
            failure: None,
            notes: None,
            files_touched: vec![],
            attempt: 1,
            max_attempts: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_COMPLETED]"));
        assert!(s.contains("tokens=800"));
    }

    #[test]
    fn format_summary_stage_completed_with_failure() {
        let event = WorkflowRunEvent::StageCompleted {
            node_id: "n1".into(),
            name: "code".into(),
            index: 0,
            duration_ms: 2000,
            status: "failure".into(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: Some(crate::outcome::FailureDetail::new(
                "tests failed",
                crate::error::FailureClass::Deterministic,
            )),
            notes: None,
            files_touched: vec![],
            attempt: 1,
            max_attempts: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_COMPLETED]"));
        assert!(s.contains("failure_reason=\"tests failed\""));
        assert!(s.contains("failure_class=deterministic"));
    }

    #[test]
    fn format_summary_stage_completed_with_notes() {
        let event = WorkflowRunEvent::StageCompleted {
            node_id: "n1".into(),
            name: "code".into(),
            index: 0,
            duration_ms: 2000,
            status: "success".into(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: None,
            notes: Some("all good".into()),
            files_touched: vec![],
            attempt: 1,
            max_attempts: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_COMPLETED]"));
        assert!(s.contains("notes=\"all good\""));
    }

    #[test]
    fn format_summary_stage_completed_with_files() {
        let event = WorkflowRunEvent::StageCompleted {
            node_id: "n1".into(),
            name: "code".into(),
            index: 0,
            duration_ms: 2000,
            status: "success".into(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: None,
            notes: None,
            files_touched: vec!["a.rs".into(), "b.rs".into()],
            attempt: 1,
            max_attempts: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_COMPLETED]"));
        assert!(s.contains("files_touched=2"));
    }

    #[test]
    fn format_summary_stage_failed() {
        let event = WorkflowRunEvent::StageFailed {
            node_id: "n1".into(),
            name: "build".into(),
            index: 0,
            failure: crate::outcome::FailureDetail::new(
                "timeout",
                crate::error::FailureClass::TransientInfra,
            ),
            will_retry: true,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_FAILED]"));
        assert!(s.contains("will_retry=true"));
        assert!(s.contains("transient_infra"));
    }

    #[test]
    fn format_summary_stage_retrying() {
        let event = WorkflowRunEvent::StageRetrying {
            node_id: "n1".into(),
            name: "build".into(),
            index: 0,
            attempt: 2,
            max_attempts: 3,
            delay_ms: 5000,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STAGE_RETRYING]"));
        assert!(s.contains("attempt=2/3"));
        assert!(s.contains("delay=5000ms"));
    }

    #[test]
    fn format_summary_parallel_started() {
        let event = WorkflowRunEvent::ParallelStarted {
            branch_count: 3,
            join_policy: "all".into(),
            error_policy: "fail_fast".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PARALLEL_STARTED]"));
        assert!(s.contains("branches=3"));
        assert!(s.contains("join_policy=all"));
    }

    #[test]
    fn format_summary_parallel_branch_started() {
        let event = WorkflowRunEvent::ParallelBranchStarted {
            branch: "lint".into(),
            index: 0,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PARALLEL_BRANCH_STARTED]"));
        assert!(s.contains("branch=lint"));
    }

    #[test]
    fn format_summary_parallel_branch_completed() {
        let event = WorkflowRunEvent::ParallelBranchCompleted {
            branch: "lint".into(),
            index: 0,
            duration_ms: 3000,
            status: "success".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PARALLEL_BRANCH_COMPLETED]"));
        assert!(s.contains("branch=lint"));
        assert!(s.contains("status=success"));
    }

    #[test]
    fn format_summary_parallel_completed() {
        let event = WorkflowRunEvent::ParallelCompleted {
            duration_ms: 5000,
            success_count: 2,
            failure_count: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PARALLEL_COMPLETED]"));
        assert!(s.contains("succeeded=2"));
        assert!(s.contains("failed=1"));
    }

    #[test]
    fn format_summary_parallel_early_termination() {
        let event = WorkflowRunEvent::ParallelEarlyTermination {
            reason: "fail_fast".into(),
            completed_count: 1,
            pending_count: 2,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PARALLEL_EARLY_TERMINATION]"));
        assert!(s.contains("reason=fail_fast"));
        assert!(s.contains("completed=1"));
        assert!(s.contains("pending=2"));
    }

    #[test]
    fn format_summary_interview_started() {
        let event = WorkflowRunEvent::InterviewStarted {
            question: "What is the goal?".into(),
            stage: "review".into(),
            question_type: "free_text".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[INTERVIEW_STARTED]"));
        assert!(s.contains("stage=review"));
        assert!(s.contains("What is the goal?"));
    }

    #[test]
    fn format_summary_interview_completed() {
        let event = WorkflowRunEvent::InterviewCompleted {
            question: "What?".into(),
            answer: "Everything".into(),
            duration_ms: 1000,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[INTERVIEW_COMPLETED]"));
        assert!(s.contains("question=\"What?\""));
        assert!(s.contains("answer=\"Everything\""));
    }

    #[test]
    fn format_summary_interview_timeout() {
        let event = WorkflowRunEvent::InterviewTimeout {
            question: "q".into(),
            stage: "review".into(),
            duration_ms: 30000,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[INTERVIEW_TIMEOUT]"));
        assert!(s.contains("stage=review"));
        assert!(s.contains("30000"));
    }

    #[test]
    fn format_summary_checkpoint_saved() {
        let event = WorkflowRunEvent::CheckpointSaved {
            node_id: "n1".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[CHECKPOINT_SAVED]"));
        assert!(s.contains("node=n1"));
    }

    #[test]
    fn format_summary_git_checkpoint() {
        let event = WorkflowRunEvent::GitCheckpoint {
            run_id: "run-1".into(),
            node_id: "n1".into(),
            git_commit_sha: "abc123".into(),
            status: "committed".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[GIT_CHECKPOINT]"));
        assert!(s.contains("sha=abc123"));
        assert!(s.contains("status=committed"));
    }

    #[test]
    fn format_summary_edge_selected_no_label() {
        let event = WorkflowRunEvent::EdgeSelected {
            from_node: "a".into(),
            to_node: "b".into(),
            label: None,
            condition: None,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[EDGE_SELECTED]"));
        assert!(s.contains("from=a"));
        assert!(s.contains("to=b"));
    }

    #[test]
    fn format_summary_edge_selected_with_label_and_condition() {
        let event = WorkflowRunEvent::EdgeSelected {
            from_node: "a".into(),
            to_node: "b".into(),
            label: Some("pass".into()),
            condition: Some("tests_pass".into()),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[EDGE_SELECTED]"));
        assert!(s.contains("label=\"pass\""));
        assert!(s.contains("condition=\"tests_pass\""));
    }

    #[test]
    fn format_summary_loop_restart() {
        let event = WorkflowRunEvent::LoopRestart {
            from_node: "check".into(),
            to_node: "build".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[LOOP_RESTART]"));
        assert!(s.contains("from=check"));
        assert!(s.contains("to=build"));
    }

    #[test]
    fn format_summary_prompt_short() {
        let event = WorkflowRunEvent::Prompt {
            stage: "code".into(),
            text: "Fix the bug".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PROMPT]"));
        assert!(s.contains("Fix the bug"));
    }

    #[test]
    fn format_summary_prompt_long_truncates() {
        let long_text = "x".repeat(200);
        let event = WorkflowRunEvent::Prompt {
            stage: "code".into(),
            text: long_text,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[PROMPT]"));
        assert!(!s.contains(&"x".repeat(200)));
        assert!(s.contains(&"x".repeat(80)));
    }

    #[test]
    fn format_summary_agent_assistant_message() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::AssistantMessage {
                text: "hello".into(),
                model: "claude-sonnet-4-5".into(),
                usage: arc_llm::types::Usage {
                    input_tokens: 1000,
                    output_tokens: 500,
                    total_tokens: 1500,
                    ..Default::default()
                },
                tool_call_count: 2,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[ASSISTANT_MESSAGE]"));
        assert!(s.contains("model=claude-sonnet-4-5"));
        assert!(s.contains("tool_calls=2"));
    }

    #[test]
    fn format_summary_agent_assistant_message_with_cache_and_reasoning() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::AssistantMessage {
                text: "hello".into(),
                model: "claude-sonnet-4-5".into(),
                usage: arc_llm::types::Usage {
                    input_tokens: 1000,
                    output_tokens: 500,
                    total_tokens: 1500,
                    cache_read_tokens: Some(800),
                    reasoning_tokens: Some(200),
                    ..Default::default()
                },
                tool_call_count: 0,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[ASSISTANT_MESSAGE]"));
        assert!(s.contains("cache_read=800"));
        assert!(s.contains("reasoning=200"));
    }

    #[test]
    fn format_summary_agent_tool_call_started() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".into(),
                tool_call_id: "tc-1".into(),
                arguments: serde_json::json!({}),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[TOOL_CALL_STARTED]"));
        assert!(s.contains("tool=read_file"));
    }

    #[test]
    fn format_summary_agent_tool_call_completed() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::ToolCallCompleted {
                tool_name: "read_file".into(),
                tool_call_id: "tc-1".into(),
                output: serde_json::json!("content"),
                is_error: false,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[TOOL_CALL_COMPLETED]"));
        assert!(s.contains("tool=read_file"));
        assert!(s.contains("is_error=false"));
    }

    #[test]
    fn format_summary_agent_error() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::Error {
                error: arc_agent::error::AgentError::InvalidState("bad state".into()),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SESSION_ERROR]"));
        assert!(s.contains("stage=code"));
    }

    #[test]
    fn format_summary_agent_context_window_warning() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::ContextWindowWarning {
                estimated_tokens: 90000,
                context_window_size: 100000,
                usage_percent: 90,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[CONTEXT_WINDOW_WARNING]"));
        assert!(s.contains("usage=90%"));
    }

    #[test]
    fn format_summary_agent_loop_detected() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::LoopDetected,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[LOOP_DETECTED]"));
        assert!(s.contains("stage=code"));
    }

    #[test]
    fn format_summary_agent_turn_limit_reached() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::TurnLimitReached { max_turns: 25 },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[TURN_LIMIT_REACHED]"));
        assert!(s.contains("max_turns=25"));
    }

    #[test]
    fn format_summary_agent_compaction_started() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::CompactionStarted {
                estimated_tokens: 80000,
                context_window_size: 100000,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[COMPACTION_STARTED]"));
        assert!(s.contains("estimated_tokens=80000"));
        assert!(s.contains("context_window=100000"));
    }

    #[test]
    fn format_summary_agent_compaction_completed() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::CompactionCompleted {
                original_turn_count: 50,
                preserved_turn_count: 10,
                summary_token_estimate: 2000,
                tracked_file_count: 5,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[COMPACTION_COMPLETED]"));
        assert!(s.contains("original_turns=50"));
        assert!(s.contains("preserved_turns=10"));
        assert!(s.contains("tracked_files=5"));
    }

    #[test]
    fn format_summary_agent_llm_retry() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::LlmRetry {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                attempt: 2,
                delay_secs: 1.5,
                error: arc_llm::error::SdkError::RequestTimeout {
                    message: "timed out".into(),
                },
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[LLM_RETRY]"));
        assert!(s.contains("provider=anthropic"));
        assert!(s.contains("attempt=2"));
        assert!(s.contains("delay=1500ms"));
    }

    #[test]
    fn format_summary_agent_subagent_failed() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentFailed {
                agent_id: "abcdef12-3456".into(),
                depth: 1,
                error: arc_agent::error::AgentError::ToolExecution("failed".into()),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBAGENT_FAILED]"));
        assert!(s.contains("abcdef12"));
        assert!(s.contains("depth=1"));
    }

    #[test]
    fn format_summary_agent_subagent_closed() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentClosed {
                agent_id: "abcdef12-3456".into(),
                depth: 2,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBAGENT_CLOSED]"));
        assert!(s.contains("abcdef12"));
        assert!(s.contains("depth=2"));
    }

    #[test]
    fn format_summary_subgraph_started() {
        let event = WorkflowRunEvent::SubgraphStarted {
            node_id: "sg1".into(),
            start_node: "inner_start".into(),
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBGRAPH_STARTED]"));
        assert!(s.contains("node=sg1"));
        assert!(s.contains("start_node=inner_start"));
    }

    #[test]
    fn format_summary_subgraph_completed() {
        let event = WorkflowRunEvent::SubgraphCompleted {
            node_id: "sg1".into(),
            steps_executed: 5,
            status: "success".into(),
            duration_ms: 3000,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SUBGRAPH_COMPLETED]"));
        assert!(s.contains("node=sg1"));
        assert!(s.contains("steps=5"));
        assert!(s.contains("status=success"));
    }

    #[test]
    fn format_summary_sandbox_ready() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::Ready {
                provider: "docker".into(),
                duration_ms: 1500,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_READY]"));
        assert!(s.contains("docker"));
        assert!(s.contains("1500"));
    }

    #[test]
    fn format_summary_sandbox_init_failed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::InitializeFailed {
                provider: "docker".into(),
                error: "daemon not running".into(),
                duration_ms: 500,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_INIT_FAILED]"));
        assert!(s.contains("daemon not running"));
    }

    #[test]
    fn format_summary_sandbox_cleanup_started() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::CleanupStarted {
                provider: "docker".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_CLEANUP_STARTED]"));
    }

    #[test]
    fn format_summary_sandbox_cleanup_completed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::CleanupCompleted {
                provider: "docker".into(),
                duration_ms: 200,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_CLEANUP_COMPLETED]"));
        assert!(s.contains("200"));
    }

    #[test]
    fn format_summary_sandbox_cleanup_failed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::CleanupFailed {
                provider: "docker".into(),
                error: "busy".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_CLEANUP_FAILED]"));
        assert!(s.contains("busy"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_pulling() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotPulling {
                name: "base-img".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_PULLING]"));
        assert!(s.contains("base-img"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_pulled() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotPulled {
                name: "base-img".into(),
                duration_ms: 3000,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_PULLED]"));
        assert!(s.contains("base-img"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_ensuring() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotEnsuring {
                name: "snap1".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_ENSURING]"));
        assert!(s.contains("snap1"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_creating() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotCreating {
                name: "snap1".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_CREATING]"));
        assert!(s.contains("snap1"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_ready() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotReady {
                name: "snap1".into(),
                duration_ms: 2000,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_READY]"));
        assert!(s.contains("snap1"));
    }

    #[test]
    fn format_summary_sandbox_snapshot_failed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::SnapshotFailed {
                name: "snap1".into(),
                error: "disk full".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_SNAPSHOT_FAILED]"));
        assert!(s.contains("disk full"));
    }

    #[test]
    fn format_summary_sandbox_git_clone_started_with_branch() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::GitCloneStarted {
                url: "https://github.com/repo".into(),
                branch: Some("main".into()),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_GIT_CLONE_STARTED]"));
        assert!(s.contains("branch=main"));
    }

    #[test]
    fn format_summary_sandbox_git_clone_started_no_branch() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::GitCloneStarted {
                url: "https://github.com/repo".into(),
                branch: None,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_GIT_CLONE_STARTED]"));
        assert!(s.contains("branch=(default)"));
    }

    #[test]
    fn format_summary_sandbox_git_clone_completed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::GitCloneCompleted {
                url: "https://github.com/repo".into(),
                duration_ms: 5000,
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_GIT_CLONE_COMPLETED]"));
        assert!(s.contains("5000"));
    }

    #[test]
    fn format_summary_sandbox_git_clone_failed() {
        let event = WorkflowRunEvent::Sandbox {
            event: arc_agent::SandboxEvent::GitCloneFailed {
                url: "https://github.com/repo".into(),
                error: "auth failed".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SANDBOX_GIT_CLONE_FAILED]"));
        assert!(s.contains("auth failed"));
    }

    #[test]
    fn format_summary_setup_command_started() {
        let event = WorkflowRunEvent::SetupCommandStarted {
            command: "npm install".into(),
            index: 0,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_COMMAND_STARTED]"));
        assert!(s.contains("npm install"));
        assert!(s.contains("index=0"));
    }

    #[test]
    fn format_summary_setup_command_completed() {
        let event = WorkflowRunEvent::SetupCommandCompleted {
            command: "npm install".into(),
            index: 0,
            exit_code: 0,
            duration_ms: 3000,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_COMMAND_COMPLETED]"));
        assert!(s.contains("exit_code=0"));
        assert!(s.contains("3000"));
    }

    #[test]
    fn format_summary_setup_completed() {
        let event = WorkflowRunEvent::SetupCompleted { duration_ms: 10000 };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_COMPLETED]"));
        assert!(s.contains("10000"));
    }

    #[test]
    fn format_summary_setup_failed_truncates_long_stderr() {
        let long_stderr = "e".repeat(200);
        let event = WorkflowRunEvent::SetupFailed {
            command: "make".into(),
            index: 0,
            exit_code: 1,
            stderr: long_stderr,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_FAILED]"));
        assert!(s.contains("exit_code=1"));
        assert!(!s.contains(&"e".repeat(200)));
        assert!(s.contains(&"e".repeat(80)));
    }

    #[test]
    fn format_summary_stall_watchdog_timeout() {
        let event = WorkflowRunEvent::StallWatchdogTimeout {
            node: "build".into(),
            idle_seconds: 300,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[STALL_WATCHDOG_TIMEOUT]"));
        assert!(s.contains("node=build"));
        assert!(s.contains("idle_seconds=300"));
    }

    #[test]
    fn format_summary_assets_captured() {
        let event = WorkflowRunEvent::AssetsCaptured {
            node_id: "n1".into(),
            files_copied: 5,
            total_bytes: 1024,
            files_skipped: 1,
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[ASSETS_CAPTURED]"));
        assert!(s.contains("node=n1"));
        assert!(s.contains("files_copied=5"));
        assert!(s.contains("files_skipped=1"));
    }
}
