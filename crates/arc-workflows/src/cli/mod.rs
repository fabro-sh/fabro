pub mod backend;
pub mod cli_backend;
pub mod run;
pub mod runs;
pub mod task_config;
pub mod validate;

use std::path::Path;

use arc_util::terminal::Styles;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::event::PipelineEvent;
use crate::outcome::StageUsage;
use crate::validation::{Diagnostic, Severity};
use arc_agent::AgentEvent;

/// Execution environment for agent tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ExecutionEnvKind {
    /// Run tools on the local host (default)
    #[default]
    Local,
    /// Run tools inside a Docker container
    Docker,
    /// Run tools inside a Daytona cloud sandbox
    Daytona,
}

impl fmt::Display for ExecutionEnvKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Docker => write!(f, "docker"),
            Self::Daytona => write!(f, "daytona"),
        }
    }
}

impl FromStr for ExecutionEnvKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "docker" => Ok(Self::Docker),
            "daytona" => Ok(Self::Daytona),
            other => Err(format!("unknown execution environment: {other}")),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "arc-workflows",
    version,
    about = "DOT-based pipeline runner for AI workflows"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Launch a pipeline from a .dot or .toml task file
    Run(RunArgs),
    /// Parse and validate a pipeline without executing
    Validate(ValidateArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to a .dot pipeline file or .toml task config (not required with --run-branch)
    #[arg(required_unless_present = "run_branch")]
    pub pipeline: Option<PathBuf>,

    /// Log/artifact directory
    #[arg(long)]
    pub logs_dir: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

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

    /// Verbosity level (-v summary, -vv full details)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Execution environment for agent tools
    #[arg(long, value_enum)]
    pub execution_env: Option<ExecutionEnvKind>,

    /// Attach a label to this run (repeatable, format: KEY=VALUE)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,
}

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the .dot pipeline file
    pub pipeline: PathBuf,
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
                "{red}error{reset}{location}: {} ({dim}{}{reset})",
                d.message,
                d.rule,
                red = styles.red,
                dim = styles.dim,
                reset = styles.reset,
            ),
            Severity::Warning => eprintln!(
                "{yellow}warning{reset}{location}: {} ({dim}{}{reset})",
                d.message,
                d.rule,
                yellow = styles.yellow,
                dim = styles.dim,
                reset = styles.reset,
            ),
            Severity::Info => eprintln!(
                "{dim}info{location}: {} ({}){reset}",
                d.message,
                d.rule,
                dim = styles.dim,
                reset = styles.reset,
            ),
        }
    }
}

/// Format milliseconds into a human-readable duration string.
///
/// - < 1000ms: `123ms`
/// - < 60s: `12.3s`
/// - >= 60s: `1m 23s`
#[must_use]
pub fn format_duration_human(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        let secs = ms as f64 / 1000.0;
        format!("{secs:.1}s")
    } else {
        let total_secs = ms / 1000;
        let minutes = total_secs / 60;
        let secs = total_secs % 60;
        format!("{minutes}m {secs}s")
    }
}

/// One-line summary of a pipeline event for `-v` output (dimmed).
#[must_use]
pub fn format_event_summary(event: &PipelineEvent, styles: &Styles) -> String {
    let body = match event {
        PipelineEvent::PipelineStarted { name, run_id, .. } => {
            format!("[PIPELINE_STARTED] name={name} id={run_id}")
        }
        PipelineEvent::PipelineCompleted {
            duration_ms,
            artifact_count,
            total_cost,
            ..
        } => {
            let mut s =
                format!("[PIPELINE_COMPLETED] duration={duration_ms}ms artifacts={artifact_count}");
            if let Some(cost) = total_cost {
                s.push_str(&format!(" total_cost={}", format_cost(*cost)));
            }
            s
        }
        PipelineEvent::PipelineFailed {
            error, duration_ms, ..
        } => {
            format!("[PIPELINE_FAILED] error=\"{error}\" duration={duration_ms}ms")
        }
        PipelineEvent::StageStarted {
            name,
            index,
            handler_type,
            attempt,
            max_attempts,
        } => {
            let mut s = format!("[STAGE_STARTED] name={name} index={index}");
            if let Some(ht) = handler_type {
                s.push_str(&format!(" handler_type={ht}"));
            }
            s.push_str(&format!(" attempt={attempt}/{max_attempts}"));
            s
        }
        PipelineEvent::StageCompleted {
            name,
            index,
            duration_ms,
            status,
            preferred_label,
            suggested_next_ids,
            usage,
            failure_reason,
            notes,
            files_touched,
            attempt,
            max_attempts,
            failure_class,
        } => {
            let mut s = format!("[STAGE_COMPLETED] name={name} index={index} duration={duration_ms}ms status={status}");
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
            if let Some(reason) = failure_reason {
                s.push_str(&format!(" failure_reason=\"{reason}\""));
            }
            if let Some(n) = notes {
                s.push_str(&format!(" notes=\"{n}\""));
            }
            if !files_touched.is_empty() {
                s.push_str(&format!(" files_touched={}", files_touched.len()));
            }
            s.push_str(&format!(" attempt={attempt}/{max_attempts}"));
            if let Some(fc) = failure_class {
                s.push_str(&format!(" failure_class={fc}"));
            }
            s
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
            failure_reason,
            failure_class,
        } => {
            let mut s = format!(
                "[STAGE_FAILED] name={name} index={index} error=\"{error}\" will_retry={will_retry}"
            );
            if let Some(reason) = failure_reason {
                s.push_str(&format!(" failure_reason=\"{reason}\""));
            }
            if let Some(fc) = failure_class {
                s.push_str(&format!(" failure_class={fc}"));
            }
            s
        }
        PipelineEvent::StageRetrying {
            name,
            index,
            attempt,
            max_attempts,
            delay_ms,
        } => {
            format!(
                "[STAGE_RETRYING] name={name} index={index} attempt={attempt}/{max_attempts} delay={delay_ms}ms"
            )
        }
        PipelineEvent::ParallelStarted {
            branch_count,
            join_policy,
            error_policy,
        } => {
            format!("[PARALLEL_STARTED] branches={branch_count} join_policy={join_policy} error_policy={error_policy}")
        }
        PipelineEvent::ParallelBranchStarted { branch, index } => {
            format!("[PARALLEL_BRANCH_STARTED] branch={branch} index={index}")
        }
        PipelineEvent::ParallelBranchCompleted {
            branch,
            index,
            duration_ms,
            status,
        } => {
            format!("[PARALLEL_BRANCH_COMPLETED] branch={branch} index={index} duration={duration_ms}ms status={status}")
        }
        PipelineEvent::ParallelCompleted {
            duration_ms,
            success_count,
            failure_count,
        } => {
            format!("[PARALLEL_COMPLETED] duration={duration_ms}ms succeeded={success_count} failed={failure_count}")
        }
        PipelineEvent::InterviewStarted {
            question,
            stage,
            question_type,
        } => {
            format!("[INTERVIEW_STARTED] stage={stage} question=\"{question}\" question_type={question_type}")
        }
        PipelineEvent::InterviewCompleted {
            question,
            answer,
            duration_ms,
        } => {
            format!(
                "[INTERVIEW_COMPLETED] question=\"{question}\" answer=\"{answer}\" duration={duration_ms}ms"
            )
        }
        PipelineEvent::InterviewTimeout {
            stage, duration_ms, ..
        } => {
            format!("[INTERVIEW_TIMEOUT] stage={stage} duration={duration_ms}ms")
        }
        PipelineEvent::CheckpointSaved { node_id } => {
            format!("[CHECKPOINT_SAVED] node={node_id}")
        }
        PipelineEvent::GitCheckpoint {
            node_id,
            git_commit_sha,
            status,
            ..
        } => {
            format!("[GIT_CHECKPOINT] node={node_id} sha={git_commit_sha} status={status}")
        }
        PipelineEvent::EdgeSelected {
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
        PipelineEvent::LoopRestart { from_node, to_node } => {
            format!("[LOOP_RESTART] from={from_node} to={to_node}")
        }
        PipelineEvent::Prompt { stage, text } => {
            let truncated = if text.len() > 80 { &text[..80] } else { text };
            format!("[PROMPT] stage={stage} text=\"{truncated}\"")
        }
        PipelineEvent::Agent { stage, event } => match event {
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
        PipelineEvent::ParallelEarlyTermination {
            reason,
            completed_count,
            pending_count,
        } => {
            format!("[PARALLEL_EARLY_TERMINATION] reason={reason} completed={completed_count} pending={pending_count}")
        }
        PipelineEvent::SubgraphStarted {
            node_id,
            start_node,
        } => {
            format!("[SUBGRAPH_STARTED] node={node_id} start_node={start_node}")
        }
        PipelineEvent::SubgraphCompleted {
            node_id,
            steps_executed,
            status,
            duration_ms,
        } => {
            format!("[SUBGRAPH_COMPLETED] node={node_id} steps={steps_executed} status={status} duration={duration_ms}ms")
        }
        PipelineEvent::ExecutionEnv { event } => {
            use arc_agent::ExecutionEnvEvent;
            match event {
                ExecutionEnvEvent::Initializing { env_type } => format!("[EXEC_ENV_INITIALIZING] env_type={env_type}"),
                ExecutionEnvEvent::Ready { env_type, duration_ms } => format!("[EXEC_ENV_READY] env_type={env_type} duration={duration_ms}ms"),
                ExecutionEnvEvent::InitializeFailed { env_type, error, duration_ms } => format!("[EXEC_ENV_INIT_FAILED] env_type={env_type} error=\"{error}\" duration={duration_ms}ms"),
                ExecutionEnvEvent::CleanupStarted { env_type } => format!("[EXEC_ENV_CLEANUP_STARTED] env_type={env_type}"),
                ExecutionEnvEvent::CleanupCompleted { env_type, duration_ms } => format!("[EXEC_ENV_CLEANUP_COMPLETED] env_type={env_type} duration={duration_ms}ms"),
                ExecutionEnvEvent::CleanupFailed { env_type, error } => format!("[EXEC_ENV_CLEANUP_FAILED] env_type={env_type} error=\"{error}\""),
                ExecutionEnvEvent::ImagePulling { image } => format!("[EXEC_ENV_IMAGE_PULLING] image={image}"),
                ExecutionEnvEvent::ImagePulled { image, duration_ms } => format!("[EXEC_ENV_IMAGE_PULLED] image={image} duration={duration_ms}ms"),
                ExecutionEnvEvent::SnapshotEnsuring { name } => format!("[EXEC_ENV_SNAPSHOT_ENSURING] name={name}"),
                ExecutionEnvEvent::SnapshotCreating { name } => format!("[EXEC_ENV_SNAPSHOT_CREATING] name={name}"),
                ExecutionEnvEvent::SnapshotReady { name, duration_ms } => format!("[EXEC_ENV_SNAPSHOT_READY] name={name} duration={duration_ms}ms"),
                ExecutionEnvEvent::SnapshotFailed { name, error } => format!("[EXEC_ENV_SNAPSHOT_FAILED] name={name} error=\"{error}\""),
                ExecutionEnvEvent::GitCloneStarted { url, branch } => {
                    let branch_str = branch.as_deref().unwrap_or("(default)");
                    format!("[EXEC_ENV_GIT_CLONE_STARTED] url={url} branch={branch_str}")
                }
                ExecutionEnvEvent::GitCloneCompleted { url, duration_ms } => format!("[EXEC_ENV_GIT_CLONE_COMPLETED] url={url} duration={duration_ms}ms"),
                ExecutionEnvEvent::GitCloneFailed { url, error } => format!("[EXEC_ENV_GIT_CLONE_FAILED] url={url} error=\"{error}\""),
            }
        }
        PipelineEvent::SetupStarted { command_count } => {
            format!("[SETUP_STARTED] command_count={command_count}")
        }
        PipelineEvent::SetupCommandStarted { command, index } => {
            format!("[SETUP_COMMAND_STARTED] index={index} command=\"{command}\"")
        }
        PipelineEvent::SetupCommandCompleted {
            command,
            index,
            exit_code,
            duration_ms,
        } => {
            format!("[SETUP_COMMAND_COMPLETED] index={index} command=\"{command}\" exit_code={exit_code} duration={duration_ms}ms")
        }
        PipelineEvent::SetupCompleted { duration_ms } => {
            format!("[SETUP_COMPLETED] duration={duration_ms}ms")
        }
        PipelineEvent::SetupFailed {
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
        PipelineEvent::StallWatchdogTimeout { node, idle_seconds } => {
            format!("[STALL_WATCHDOG_TIMEOUT] node={node} idle_seconds={idle_seconds}")
        }
    };
    format!("{dim}{body}{reset}", dim = styles.dim, reset = styles.reset)
}

/// Multi-line detail view of a pipeline event for `-vv` output.
/// Box-drawing is dimmed; values are normal.
#[must_use]
pub fn format_event_detail(event: &PipelineEvent, styles: &Styles) -> String {
    let d = styles.dim;
    let r = styles.reset;

    match event {
        PipelineEvent::PipelineStarted { name, run_id, .. } => {
            format!(
                "{d}── PIPELINE_STARTED ─────────────────────────{r}\n  {d}name:{r} {name}\n  {d}id:{r}   {run_id}\n"
            )
        }
        PipelineEvent::PipelineCompleted {
            duration_ms,
            artifact_count,
            total_cost,
            ..
        } => {
            let mut s = format!("{d}── PIPELINE_COMPLETED ───────────────────────{r}\n  {d}duration_ms:{r}    {duration_ms}\n  {d}artifact_count:{r} {artifact_count}\n");
            if let Some(cost) = total_cost {
                s.push_str(&format!("  {d}total_cost:{r}     {}\n", format_cost(*cost)));
            }
            s
        }
        PipelineEvent::PipelineFailed { error, duration_ms, .. } => {
            format!("{d}── PIPELINE_FAILED ──────────────────────────{r}\n  {d}error:{r}       {error}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::StageStarted { name, index, handler_type, attempt, max_attempts } => {
            let mut s = format!(
                "{d}── STAGE_STARTED ────────────────────────────{r}\n  {d}name:{r}  {name}\n  {d}index:{r} {index}\n"
            );
            if let Some(ht) = handler_type {
                s.push_str(&format!("  {d}handler_type:{r} {ht}\n"));
            }
            s.push_str(&format!("  {d}attempt:{r}      {attempt}/{max_attempts}\n"));
            s
        }
        PipelineEvent::StageCompleted {
            name,
            index,
            duration_ms,
            status,
            preferred_label,
            suggested_next_ids,
            usage,
            failure_reason,
            notes,
            files_touched,
            attempt,
            max_attempts,
            failure_class,
        } => {
            let mut s = format!("{d}── STAGE_COMPLETED ──────────────────────────{r}\n  {d}name:{r}        {name}\n  {d}index:{r}       {index}\n  {d}duration_ms:{r} {duration_ms}\n  {d}status:{r}      {status}\n");
            if let Some(label) = preferred_label {
                s.push_str(&format!("  {d}preferred_label:{r} {label}\n"));
            }
            if !suggested_next_ids.is_empty() {
                s.push_str(&format!("  {d}suggested_next_ids:{r} {}\n", suggested_next_ids.join(", ")));
            }
            if let Some(u) = usage {
                let total = u.input_tokens + u.output_tokens;
                s.push_str(&format!("  {d}model:{r}       {}\n", u.model));
                s.push_str(&format!("  {d}tokens:{r}      {} ({} in / {} out)\n",
                    format_tokens_human(total),
                    format_tokens_human(u.input_tokens),
                    format_tokens_human(u.output_tokens),
                ));
                if let Some(cache_read) = u.cache_read_tokens {
                    s.push_str(&format!("  {d}cache_read:{r}  {}\n", format_tokens_human(cache_read)));
                }
                if let Some(cache_write) = u.cache_write_tokens {
                    s.push_str(&format!("  {d}cache_write:{r} {}\n", format_tokens_human(cache_write)));
                }
                if let Some(reasoning) = u.reasoning_tokens {
                    s.push_str(&format!("  {d}reasoning:{r}   {}\n", format_tokens_human(reasoning)));
                }
                if let Some(cost) = compute_stage_cost(u) {
                    s.push_str(&format!("  {d}cost:{r}        {}\n", format_cost(cost)));
                }
            }
            if !files_touched.is_empty() {
                s.push_str(&format!("  {d}files_touched:{r} {} files\n", files_touched.len()));
            }
            if let Some(reason) = failure_reason {
                s.push_str(&format!("  {d}failure_reason:{r} {reason}\n"));
            }
            if let Some(n) = notes {
                s.push_str(&format!("  {d}notes:{r}       {n}\n"));
            }
            s.push_str(&format!("  {d}attempt:{r}     {attempt}/{max_attempts}\n"));
            if let Some(fc) = failure_class {
                s.push_str(&format!("  {d}failure_class:{r} {fc}\n"));
            }
            s
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
            failure_reason,
            failure_class,
        } => {
            let mut s = format!("{d}── STAGE_FAILED ─────────────────────────────{r}\n  {d}name:{r}       {name}\n  {d}index:{r}      {index}\n  {d}error:{r}      {error}\n  {d}will_retry:{r} {will_retry}\n");
            if let Some(reason) = failure_reason {
                s.push_str(&format!("  {d}failure_reason:{r} {reason}\n"));
            }
            if let Some(fc) = failure_class {
                s.push_str(&format!("  {d}failure_class:{r}  {fc}\n"));
            }
            s
        }
        PipelineEvent::StageRetrying {
            name,
            index,
            attempt,
            max_attempts,
            delay_ms,
        } => {
            format!("{d}── STAGE_RETRYING ───────────────────────────{r}\n  {d}name:{r}     {name}\n  {d}index:{r}    {index}\n  {d}attempt:{r}  {attempt}/{max_attempts}\n  {d}delay_ms:{r} {delay_ms}\n")
        }
        PipelineEvent::ParallelStarted { branch_count, join_policy, error_policy } => {
            format!("{d}── PARALLEL_STARTED ─────────────────────────{r}\n  {d}branch_count:{r} {branch_count}\n  {d}join_policy:{r}  {join_policy}\n  {d}error_policy:{r} {error_policy}\n")
        }
        PipelineEvent::ParallelBranchStarted { branch, index } => {
            format!("{d}── PARALLEL_BRANCH_STARTED ──────────────────{r}\n  {d}branch:{r} {branch}\n  {d}index:{r}  {index}\n")
        }
        PipelineEvent::ParallelBranchCompleted {
            branch,
            index,
            duration_ms,
            status,
        } => {
            format!("{d}── PARALLEL_BRANCH_COMPLETED ────────────────{r}\n  {d}branch:{r}      {branch}\n  {d}index:{r}       {index}\n  {d}duration_ms:{r} {duration_ms}\n  {d}status:{r}      {status}\n")
        }
        PipelineEvent::ParallelCompleted {
            duration_ms,
            success_count,
            failure_count,
        } => {
            format!("{d}── PARALLEL_COMPLETED ───────────────────────{r}\n  {d}duration_ms:{r}   {duration_ms}\n  {d}success_count:{r} {success_count}\n  {d}failure_count:{r} {failure_count}\n")
        }
        PipelineEvent::InterviewStarted { question, stage, question_type } => {
            format!("{d}── INTERVIEW_STARTED ────────────────────────{r}\n  {d}stage:{r}         {stage}\n  {d}question:{r}      {question}\n  {d}question_type:{r} {question_type}\n")
        }
        PipelineEvent::InterviewCompleted {
            question,
            answer,
            duration_ms,
        } => {
            format!("{d}── INTERVIEW_COMPLETED ──────────────────────{r}\n  {d}question:{r}    {question}\n  {d}answer:{r}      {answer}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::InterviewTimeout {
            question,
            stage,
            duration_ms,
        } => {
            format!("{d}── INTERVIEW_TIMEOUT ────────────────────────{r}\n  {d}question:{r}    {question}\n  {d}stage:{r}       {stage}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::CheckpointSaved { node_id } => {
            format!(
                "{d}── CHECKPOINT_SAVED ─────────────────────────{r}\n  {d}node_id:{r} {node_id}\n"
            )
        }
        PipelineEvent::GitCheckpoint { run_id, node_id, status, git_commit_sha } => {
            format!(
                "{d}── GIT_CHECKPOINT ───────────────────────────{r}\n  {d}run_id:{r}  {run_id}\n  {d}node_id:{r} {node_id}\n  {d}status:{r}  {status}\n  {d}sha:{r}     {git_commit_sha}\n"
            )
        }
        PipelineEvent::EdgeSelected { from_node, to_node, label, condition } => {
            let mut s = format!("{d}── EDGE_SELECTED ────────────────────────────{r}\n  {d}from:{r} {from_node}\n  {d}to:{r}   {to_node}\n");
            if let Some(l) = label {
                s.push_str(&format!("  {d}label:{r}     {l}\n"));
            }
            if let Some(c) = condition {
                s.push_str(&format!("  {d}condition:{r} {c}\n"));
            }
            s
        }
        PipelineEvent::LoopRestart { from_node, to_node } => {
            format!("{d}── LOOP_RESTART ─────────────────────────────{r}\n  {d}from:{r} {from_node}\n  {d}to:{r}   {to_node}\n")
        }
        PipelineEvent::Prompt { stage, text } => {
            format!("{d}── PROMPT ───────────────────────────────────{r}\n  {d}stage:{r} {stage}\n  {d}text:{r}\n{text}\n")
        }
        PipelineEvent::Agent { stage, event } => match event {
            AgentEvent::AssistantMessage { text, model, usage, tool_call_count } => {
                let total = usage.input_tokens + usage.output_tokens;
                let truncated = if text.len() > 200 { &text[..200] } else { text.as_str() };
                let mut s = format!("{d}── ASSISTANT_MESSAGE ────────────────────────{r}\n  {d}stage:{r}       {stage}\n  {d}model:{r}       {model}\n  {d}tokens:{r}      {} ({} in / {} out)\n  {d}tool_calls:{r}  {tool_call_count}\n",
                    format_tokens_human(total),
                    format_tokens_human(usage.input_tokens),
                    format_tokens_human(usage.output_tokens),
                );
                if let Some(cache_read) = usage.cache_read_tokens {
                    s.push_str(&format!("  {d}cache_read:{r}  {}\n", format_tokens_human(cache_read)));
                }
                if let Some(cache_write) = usage.cache_write_tokens {
                    s.push_str(&format!("  {d}cache_write:{r} {}\n", format_tokens_human(cache_write)));
                }
                if let Some(reasoning) = usage.reasoning_tokens {
                    s.push_str(&format!("  {d}reasoning:{r}   {}\n", format_tokens_human(reasoning)));
                }
                s.push_str(&format!("  {d}text:{r}        {truncated}\n"));
                s
            }
            AgentEvent::ToolCallStarted { tool_name, tool_call_id, arguments } => {
                let args_str = serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
                let truncated = if args_str.len() > 200 { &args_str[..200] } else { &args_str };
                format!("{d}── TOOL_CALL_STARTED ────────────────────────{r}\n  {d}stage:{r}        {stage}\n  {d}tool_name:{r}    {tool_name}\n  {d}tool_call_id:{r} {tool_call_id}\n  {d}arguments:{r}    {truncated}\n")
            }
            AgentEvent::ToolCallCompleted { tool_name, tool_call_id, output, is_error } => {
                let output_str = serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
                let truncated = if output_str.len() > 200 { &output_str[..200] } else { &output_str };
                format!("{d}── TOOL_CALL_COMPLETED ──────────────────────{r}\n  {d}stage:{r}        {stage}\n  {d}tool_name:{r}    {tool_name}\n  {d}tool_call_id:{r} {tool_call_id}\n  {d}is_error:{r}     {is_error}\n  {d}output:{r}       {truncated}\n")
            }
            AgentEvent::Error { error } => {
                format!("{d}── SESSION_ERROR ────────────────────────────{r}\n  {d}stage:{r} {stage}\n  {d}error:{r} {error}\n")
            }
            AgentEvent::ContextWindowWarning { estimated_tokens, context_window_size, usage_percent } => {
                format!("{d}── CONTEXT_WINDOW_WARNING ───────────────────{r}\n  {d}stage:{r}               {stage}\n  {d}estimated_tokens:{r}    {estimated_tokens}\n  {d}context_window_size:{r} {context_window_size}\n  {d}usage_percent:{r}       {usage_percent}%\n")
            }
            AgentEvent::LoopDetected => {
                format!("{d}── LOOP_DETECTED ────────────────────────────{r}\n  {d}stage:{r} {stage}\n")
            }
            AgentEvent::TurnLimitReached { max_turns } => {
                format!("{d}── TURN_LIMIT_REACHED ───────────────────────{r}\n  {d}stage:{r}     {stage}\n  {d}max_turns:{r} {max_turns}\n")
            }
            AgentEvent::CompactionStarted { estimated_tokens, context_window_size } => {
                format!("{d}── COMPACTION_STARTED ───────────────────────{r}\n  {d}stage:{r}               {stage}\n  {d}estimated_tokens:{r}    {estimated_tokens}\n  {d}context_window_size:{r} {context_window_size}\n")
            }
            AgentEvent::CompactionCompleted { original_turn_count, preserved_turn_count, summary_token_estimate, tracked_file_count } => {
                format!("{d}── COMPACTION_COMPLETED ─────────────────────{r}\n  {d}stage:{r}                  {stage}\n  {d}original_turn_count:{r}    {original_turn_count}\n  {d}preserved_turn_count:{r}   {preserved_turn_count}\n  {d}summary_token_estimate:{r} {summary_token_estimate}\n  {d}tracked_file_count:{r}     {tracked_file_count}\n")
            }
            AgentEvent::LlmRetry { provider, model, attempt, delay_secs, error } => {
                let delay_ms = (*delay_secs * 1000.0) as u64;
                format!("{d}── LLM_RETRY ────────────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}provider:{r} {provider}\n  {d}model:{r}    {model}\n  {d}attempt:{r}  {attempt}\n  {d}delay_ms:{r} {delay_ms}\n  {d}error:{r}    {error}\n")
            }
            AgentEvent::SubAgentSpawned { agent_id, depth, task } => {
                let task_preview = if task.len() > 200 { &task[..200] } else { task.as_str() };
                format!("{d}── SUBAGENT_SPAWNED ─────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}agent_id:{r} {agent_id}\n  {d}depth:{r}    {depth}\n  {d}task:{r}     {task_preview}\n")
            }
            AgentEvent::SubAgentCompleted { agent_id, depth, success, turns_used } => {
                format!("{d}── SUBAGENT_COMPLETED ───────────────────────{r}\n  {d}stage:{r}      {stage}\n  {d}agent_id:{r}   {agent_id}\n  {d}depth:{r}      {depth}\n  {d}success:{r}    {success}\n  {d}turns_used:{r} {turns_used}\n")
            }
            AgentEvent::SubAgentFailed { agent_id, depth, error } => {
                format!("{d}── SUBAGENT_FAILED ──────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}agent_id:{r} {agent_id}\n  {d}depth:{r}    {depth}\n  {d}error:{r}    {error}\n")
            }
            AgentEvent::SubAgentClosed { agent_id, depth } => {
                format!("{d}── SUBAGENT_CLOSED ──────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}agent_id:{r} {agent_id}\n  {d}depth:{r}    {depth}\n")
            }
            AgentEvent::SubAgentEvent { agent_id, depth, event } => {
                format!("{d}── SUBAGENT_EVENT ───────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}agent_id:{r} {agent_id}\n  {d}depth:{r}    {depth}\n  {d}event:{r}    {event:?}\n")
            }
            other => format!("{d}── AGENT ────────────────────────────────────{r}\n  {d}stage:{r} {stage}\n  {d}event:{r} {other:?}\n"),
        }
        PipelineEvent::ParallelEarlyTermination {
            reason,
            completed_count,
            pending_count,
        } => {
            format!("{d}── PARALLEL_EARLY_TERMINATION ───────────────{r}\n  {d}reason:{r}          {reason}\n  {d}completed_count:{r} {completed_count}\n  {d}pending_count:{r}   {pending_count}\n")
        }
        PipelineEvent::SubgraphStarted { node_id, start_node } => {
            format!("{d}── SUBGRAPH_STARTED ─────────────────────────{r}\n  {d}node_id:{r}    {node_id}\n  {d}start_node:{r} {start_node}\n")
        }
        PipelineEvent::SubgraphCompleted {
            node_id,
            steps_executed,
            status,
            duration_ms,
        } => {
            format!("{d}── SUBGRAPH_COMPLETED ───────────────────────{r}\n  {d}node_id:{r}        {node_id}\n  {d}steps_executed:{r} {steps_executed}\n  {d}status:{r}         {status}\n  {d}duration_ms:{r}    {duration_ms}\n")
        }
        PipelineEvent::ExecutionEnv { event } => {
            use arc_agent::ExecutionEnvEvent;
            match event {
                ExecutionEnvEvent::Initializing { env_type } => {
                    format!("{d}── EXEC_ENV_INITIALIZING ────────────────────{r}\n  {d}env_type:{r} {env_type}\n")
                }
                ExecutionEnvEvent::Ready { env_type, duration_ms } => {
                    format!("{d}── EXEC_ENV_READY ───────────────────────────{r}\n  {d}env_type:{r}    {env_type}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::InitializeFailed { env_type, error, duration_ms } => {
                    format!("{d}── EXEC_ENV_INIT_FAILED ─────────────────────{r}\n  {d}env_type:{r}    {env_type}\n  {d}error:{r}       {error}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::CleanupStarted { env_type } => {
                    format!("{d}── EXEC_ENV_CLEANUP_STARTED ─────────────────{r}\n  {d}env_type:{r} {env_type}\n")
                }
                ExecutionEnvEvent::CleanupCompleted { env_type, duration_ms } => {
                    format!("{d}── EXEC_ENV_CLEANUP_COMPLETED ───────────────{r}\n  {d}env_type:{r}    {env_type}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::CleanupFailed { env_type, error } => {
                    format!("{d}── EXEC_ENV_CLEANUP_FAILED ──────────────────{r}\n  {d}env_type:{r} {env_type}\n  {d}error:{r}    {error}\n")
                }
                ExecutionEnvEvent::ImagePulling { image } => {
                    format!("{d}── EXEC_ENV_IMAGE_PULLING ───────────────────{r}\n  {d}image:{r} {image}\n")
                }
                ExecutionEnvEvent::ImagePulled { image, duration_ms } => {
                    format!("{d}── EXEC_ENV_IMAGE_PULLED ────────────────────{r}\n  {d}image:{r}       {image}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::SnapshotEnsuring { name } => {
                    format!("{d}── EXEC_ENV_SNAPSHOT_ENSURING ───────────────{r}\n  {d}name:{r} {name}\n")
                }
                ExecutionEnvEvent::SnapshotCreating { name } => {
                    format!("{d}── EXEC_ENV_SNAPSHOT_CREATING ───────────────{r}\n  {d}name:{r} {name}\n")
                }
                ExecutionEnvEvent::SnapshotReady { name, duration_ms } => {
                    format!("{d}── EXEC_ENV_SNAPSHOT_READY ──────────────────{r}\n  {d}name:{r}        {name}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::SnapshotFailed { name, error } => {
                    format!("{d}── EXEC_ENV_SNAPSHOT_FAILED ─────────────────{r}\n  {d}name:{r}  {name}\n  {d}error:{r} {error}\n")
                }
                ExecutionEnvEvent::GitCloneStarted { url, branch } => {
                    let branch_str = branch.as_deref().unwrap_or("(default)");
                    format!("{d}── EXEC_ENV_GIT_CLONE_STARTED ──────────────{r}\n  {d}url:{r}    {url}\n  {d}branch:{r} {branch_str}\n")
                }
                ExecutionEnvEvent::GitCloneCompleted { url, duration_ms } => {
                    format!("{d}── EXEC_ENV_GIT_CLONE_COMPLETED ────────────{r}\n  {d}url:{r}         {url}\n  {d}duration_ms:{r} {duration_ms}\n")
                }
                ExecutionEnvEvent::GitCloneFailed { url, error } => {
                    format!("{d}── EXEC_ENV_GIT_CLONE_FAILED ───────────────{r}\n  {d}url:{r}   {url}\n  {d}error:{r} {error}\n")
                }
            }
        }
        PipelineEvent::SetupStarted { command_count } => {
            format!("{d}── SETUP_STARTED ────────────────────────────{r}\n  {d}command_count:{r} {command_count}\n")
        }
        PipelineEvent::SetupCommandStarted { command, index } => {
            format!("{d}── SETUP_COMMAND_STARTED ────────────────────{r}\n  {d}index:{r}   {index}\n  {d}command:{r} {command}\n")
        }
        PipelineEvent::SetupCommandCompleted { command, index, exit_code, duration_ms } => {
            format!("{d}── SETUP_COMMAND_COMPLETED ──────────────────{r}\n  {d}index:{r}       {index}\n  {d}command:{r}     {command}\n  {d}exit_code:{r}   {exit_code}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::SetupCompleted { duration_ms } => {
            format!("{d}── SETUP_COMPLETED ──────────────────────────{r}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::SetupFailed { command, index, exit_code, stderr } => {
            format!("{d}── SETUP_FAILED ─────────────────────────────{r}\n  {d}index:{r}     {index}\n  {d}command:{r}   {command}\n  {d}exit_code:{r} {exit_code}\n  {d}stderr:{r}    {stderr}\n")
        }
        PipelineEvent::StallWatchdogTimeout { node, idle_seconds } => {
            format!("{d}── STALL_WATCHDOG_TIMEOUT ────────────────────{r}\n  {d}node:{r}         {node}\n  {d}idle_seconds:{r} {idle_seconds}\n")
        }
    }
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

/// Format a token count for human display (e.g. `"15.2k"` or `"850"`).
#[must_use]
pub fn format_tokens_human(tokens: i64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_env_kind_default_is_local() {
        assert_eq!(ExecutionEnvKind::default(), ExecutionEnvKind::Local);
    }

    #[test]
    fn execution_env_kind_from_str() {
        assert_eq!(
            "local".parse::<ExecutionEnvKind>().unwrap(),
            ExecutionEnvKind::Local
        );
        assert_eq!(
            "docker".parse::<ExecutionEnvKind>().unwrap(),
            ExecutionEnvKind::Docker
        );
        assert_eq!(
            "daytona".parse::<ExecutionEnvKind>().unwrap(),
            ExecutionEnvKind::Daytona
        );
        assert_eq!(
            "LOCAL".parse::<ExecutionEnvKind>().unwrap(),
            ExecutionEnvKind::Local
        );
        assert!("invalid".parse::<ExecutionEnvKind>().is_err());
    }

    #[test]
    fn execution_env_kind_display() {
        assert_eq!(ExecutionEnvKind::Local.to_string(), "local");
        assert_eq!(ExecutionEnvKind::Docker.to_string(), "docker");
        assert_eq!(ExecutionEnvKind::Daytona.to_string(), "daytona");
    }

    fn test_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
    }

    #[test]
    fn format_summary_execution_env_initializing() {
        let event = PipelineEvent::ExecutionEnv {
            event: arc_agent::ExecutionEnvEvent::Initializing {
                env_type: "docker".into(),
            },
        };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[EXEC_ENV_INITIALIZING]"));
        assert!(s.contains("docker"));
    }

    #[test]
    fn format_summary_setup_started() {
        let event = PipelineEvent::SetupStarted { command_count: 3 };
        let s = format_event_summary(&event, test_styles());
        assert!(s.contains("[SETUP_STARTED]"));
        assert!(s.contains("3"));
    }

    #[test]
    fn format_detail_execution_env_ready() {
        let event = PipelineEvent::ExecutionEnv {
            event: arc_agent::ExecutionEnvEvent::Ready {
                env_type: "local".into(),
                duration_ms: 42,
            },
        };
        let s = format_event_detail(&event, test_styles());
        assert!(s.contains("EXEC_ENV_READY"));
        assert!(s.contains("local"));
        assert!(s.contains("42"));
    }

    #[test]
    fn format_summary_subagent_spawned() {
        let event = PipelineEvent::Agent {
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
        let event = PipelineEvent::Agent {
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
    fn format_detail_subagent_failed() {
        let event = PipelineEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentFailed {
                agent_id: "abcdef12-xxxx".into(),
                depth: 2,
                error: "timeout".into(),
            },
        };
        let s = format_event_detail(&event, test_styles());
        assert!(s.contains("SUBAGENT_FAILED"));
        assert!(s.contains("timeout"));
        assert!(s.contains("depth"));
    }

    #[test]
    fn format_detail_subagent_closed() {
        let event = PipelineEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::SubAgentClosed {
                agent_id: "abcdef12-xxxx".into(),
                depth: 1,
            },
        };
        let s = format_event_detail(&event, test_styles());
        assert!(s.contains("SUBAGENT_CLOSED"));
        assert!(s.contains("abcdef12-xxxx"));
    }

    #[test]
    fn format_summary_subagent_event() {
        let event = PipelineEvent::Agent {
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

    #[test]
    fn format_detail_setup_command_completed() {
        let event = PipelineEvent::SetupCommandCompleted {
            command: "npm install".into(),
            index: 0,
            exit_code: 0,
            duration_ms: 5000,
        };
        let s = format_event_detail(&event, test_styles());
        assert!(s.contains("SETUP_COMMAND_COMPLETED"));
        assert!(s.contains("npm install"));
        assert!(s.contains("5000"));
    }
}
