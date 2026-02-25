pub mod backend;
pub mod run;
#[cfg(feature = "server")]
pub mod serve;
pub mod validate;

use std::path::Path;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use terminal::Styles;

use crate::event::PipelineEvent;
use crate::outcome::StageUsage;
use crate::validation::{Diagnostic, Severity};

#[derive(Parser)]
#[command(name = "attractor", version, about = "DOT-based pipeline runner for AI workflows")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Launch a pipeline from a .dot file
    Run(RunArgs),
    /// Parse and validate a pipeline without executing
    Validate(ValidateArgs),
    /// Start the HTTP API server
    #[cfg(feature = "server")]
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to the .dot pipeline file
    pub pipeline: PathBuf,

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

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Verbosity level (-v summary, -vv full details)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Run agent tools inside a Docker container
    #[arg(long)]
    pub docker: bool,
}

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the .dot pipeline file
    pub pipeline: PathBuf,
}

#[cfg(feature = "server")]
#[derive(Args)]
pub struct ServeArgs {
    /// Port to listen on
    #[arg(long, default_value = "3000")]
    pub port: u16,

    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

    /// Run agent tools inside a Docker container
    #[arg(long)]
    pub docker: bool,
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
                d.message, d.rule,
                red = styles.red, dim = styles.dim, reset = styles.reset,
            ),
            Severity::Warning => eprintln!(
                "{yellow}warning{reset}{location}: {} ({dim}{}{reset})",
                d.message, d.rule,
                yellow = styles.yellow, dim = styles.dim, reset = styles.reset,
            ),
            Severity::Info => eprintln!(
                "{dim}info{location}: {} ({}){reset}",
                d.message, d.rule,
                dim = styles.dim, reset = styles.reset,
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
        PipelineEvent::PipelineStarted { name, id } => {
            format!("[PIPELINE_STARTED] name={name} id={id}")
        }
        PipelineEvent::PipelineCompleted {
            duration_ms,
            artifact_count,
        } => {
            format!("[PIPELINE_COMPLETED] duration={duration_ms}ms artifacts={artifact_count}")
        }
        PipelineEvent::PipelineFailed { error, duration_ms } => {
            format!("[PIPELINE_FAILED] error=\"{error}\" duration={duration_ms}ms")
        }
        PipelineEvent::StageStarted { name, index, handler_type } => {
            let mut s = format!("[STAGE_STARTED] name={name} index={index}");
            if let Some(ht) = handler_type {
                s.push_str(&format!(" handler_type={ht}"));
            }
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
        } => {
            let mut s = format!("[STAGE_COMPLETED] name={name} index={index} duration={duration_ms}ms status={status}");
            if let Some(label) = preferred_label {
                s.push_str(&format!(" preferred_label=\"{label}\""));
            }
            if !suggested_next_ids.is_empty() {
                s.push_str(&format!(" suggested_next_ids={}", suggested_next_ids.join(",")));
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
            s
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
            failure_reason,
        } => {
            let mut s = format!(
                "[STAGE_FAILED] name={name} index={index} error=\"{error}\" will_retry={will_retry}"
            );
            if let Some(reason) = failure_reason {
                s.push_str(&format!(" failure_reason=\"{reason}\""));
            }
            s
        }
        PipelineEvent::StageRetrying {
            name,
            index,
            attempt,
            delay_ms,
        } => {
            format!(
                "[STAGE_RETRYING] name={name} index={index} attempt={attempt} delay={delay_ms}ms"
            )
        }
        PipelineEvent::ParallelStarted { branch_count, join_policy, error_policy } => {
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
        PipelineEvent::InterviewStarted { question, stage, question_type } => {
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
        PipelineEvent::Prompt { stage, text } => {
            let truncated = if text.len() > 80 { &text[..80] } else { text };
            format!("[PROMPT] stage={stage} text=\"{truncated}\"")
        }
        PipelineEvent::AssistantMessage {
            stage,
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
        PipelineEvent::ToolCallStarted {
            stage,
            tool_name,
            ..
        } => {
            format!("[TOOL_CALL_STARTED] stage={stage} tool={tool_name}")
        }
        PipelineEvent::ToolCallCompleted {
            stage,
            tool_name,
            is_error,
            ..
        } => {
            format!("[TOOL_CALL_COMPLETED] stage={stage} tool={tool_name} is_error={is_error}")
        }
        PipelineEvent::SessionError { stage, error } => {
            format!("[SESSION_ERROR] stage={stage} error=\"{error}\"")
        }
        PipelineEvent::ContextWindowWarning {
            stage,
            usage_percent,
            ..
        } => {
            format!("[CONTEXT_WINDOW_WARNING] stage={stage} usage={usage_percent}%")
        }
        PipelineEvent::LoopDetected { stage } => {
            format!("[LOOP_DETECTED] stage={stage}")
        }
        PipelineEvent::TurnLimitReached { stage } => {
            format!("[TURN_LIMIT_REACHED] stage={stage}")
        }
        PipelineEvent::CompactionStarted {
            stage,
            estimated_tokens,
            context_window_size,
        } => {
            format!("[COMPACTION_STARTED] stage={stage} estimated_tokens={estimated_tokens} context_window={context_window_size}")
        }
        PipelineEvent::CompactionCompleted {
            stage,
            original_turn_count,
            preserved_turn_count,
            summary_token_estimate,
        } => {
            format!("[COMPACTION_COMPLETED] stage={stage} original_turns={original_turn_count} preserved_turns={preserved_turn_count} summary_tokens={summary_token_estimate}")
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
        PipelineEvent::PipelineStarted { name, id } => {
            format!(
                "{d}── PIPELINE_STARTED ─────────────────────────{r}\n  {d}name:{r} {name}\n  {d}id:{r}   {id}\n"
            )
        }
        PipelineEvent::PipelineCompleted {
            duration_ms,
            artifact_count,
        } => {
            format!("{d}── PIPELINE_COMPLETED ───────────────────────{r}\n  {d}duration_ms:{r}    {duration_ms}\n  {d}artifact_count:{r} {artifact_count}\n")
        }
        PipelineEvent::PipelineFailed { error, duration_ms } => {
            format!("{d}── PIPELINE_FAILED ──────────────────────────{r}\n  {d}error:{r}       {error}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::StageStarted { name, index, handler_type } => {
            let mut s = format!(
                "{d}── STAGE_STARTED ────────────────────────────{r}\n  {d}name:{r}  {name}\n  {d}index:{r} {index}\n"
            );
            if let Some(ht) = handler_type {
                s.push_str(&format!("  {d}handler_type:{r} {ht}\n"));
            }
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
            s
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
            failure_reason,
        } => {
            let mut s = format!("{d}── STAGE_FAILED ─────────────────────────────{r}\n  {d}name:{r}       {name}\n  {d}index:{r}      {index}\n  {d}error:{r}      {error}\n  {d}will_retry:{r} {will_retry}\n");
            if let Some(reason) = failure_reason {
                s.push_str(&format!("  {d}failure_reason:{r} {reason}\n"));
            }
            s
        }
        PipelineEvent::StageRetrying {
            name,
            index,
            attempt,
            delay_ms,
        } => {
            format!("{d}── STAGE_RETRYING ───────────────────────────{r}\n  {d}name:{r}     {name}\n  {d}index:{r}    {index}\n  {d}attempt:{r}  {attempt}\n  {d}delay_ms:{r} {delay_ms}\n")
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
        PipelineEvent::Prompt { stage, text } => {
            format!("{d}── PROMPT ───────────────────────────────────{r}\n  {d}stage:{r} {stage}\n  {d}text:{r}\n{text}\n")
        }
        PipelineEvent::AssistantMessage {
            stage,
            text,
            model,
            usage,
            tool_call_count,
        } => {
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
        PipelineEvent::ToolCallStarted {
            stage,
            tool_name,
            tool_call_id,
            arguments,
        } => {
            let args_str = serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
            let truncated = if args_str.len() > 200 { &args_str[..200] } else { &args_str };
            format!("{d}── TOOL_CALL_STARTED ────────────────────────{r}\n  {d}stage:{r}        {stage}\n  {d}tool_name:{r}    {tool_name}\n  {d}tool_call_id:{r} {tool_call_id}\n  {d}arguments:{r}    {truncated}\n")
        }
        PipelineEvent::ToolCallCompleted {
            stage,
            tool_name,
            tool_call_id,
            output,
            is_error,
        } => {
            let output_str = serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
            let truncated = if output_str.len() > 200 { &output_str[..200] } else { &output_str };
            format!("{d}── TOOL_CALL_COMPLETED ──────────────────────{r}\n  {d}stage:{r}        {stage}\n  {d}tool_name:{r}    {tool_name}\n  {d}tool_call_id:{r} {tool_call_id}\n  {d}is_error:{r}     {is_error}\n  {d}output:{r}       {truncated}\n")
        }
        PipelineEvent::SessionError { stage, error } => {
            format!("{d}── SESSION_ERROR ────────────────────────────{r}\n  {d}stage:{r} {stage}\n  {d}error:{r} {error}\n")
        }
        PipelineEvent::ContextWindowWarning {
            stage,
            estimated_tokens,
            context_window_size,
            usage_percent,
        } => {
            format!("{d}── CONTEXT_WINDOW_WARNING ───────────────────{r}\n  {d}stage:{r}               {stage}\n  {d}estimated_tokens:{r}    {estimated_tokens}\n  {d}context_window_size:{r} {context_window_size}\n  {d}usage_percent:{r}       {usage_percent}%\n")
        }
        PipelineEvent::LoopDetected { stage } => {
            format!("{d}── LOOP_DETECTED ────────────────────────────{r}\n  {d}stage:{r} {stage}\n")
        }
        PipelineEvent::TurnLimitReached { stage } => {
            format!("{d}── TURN_LIMIT_REACHED ───────────────────────{r}\n  {d}stage:{r} {stage}\n")
        }
        PipelineEvent::CompactionStarted {
            stage,
            estimated_tokens,
            context_window_size,
        } => {
            format!("{d}── COMPACTION_STARTED ───────────────────────{r}\n  {d}stage:{r}               {stage}\n  {d}estimated_tokens:{r}    {estimated_tokens}\n  {d}context_window_size:{r} {context_window_size}\n")
        }
        PipelineEvent::CompactionCompleted {
            stage,
            original_turn_count,
            preserved_turn_count,
            summary_token_estimate,
        } => {
            format!("{d}── COMPACTION_COMPLETED ─────────────────────{r}\n  {d}stage:{r}                  {stage}\n  {d}original_turn_count:{r}    {original_turn_count}\n  {d}preserved_turn_count:{r}   {preserved_turn_count}\n  {d}summary_token_estimate:{r} {summary_token_estimate}\n")
        }
    }
}

/// Compute the dollar cost for a stage's token usage, if pricing is available.
#[must_use]
pub fn compute_stage_cost(usage: &StageUsage) -> Option<f64> {
    let info = llm::catalog::get_model_info(&usage.model)?;
    let input_rate = info.input_cost_per_million?;
    let output_rate = info.output_cost_per_million?;
    Some(usage.input_tokens as f64 * input_rate / 1_000_000.0
        + usage.output_tokens as f64 * output_rate / 1_000_000.0)
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
