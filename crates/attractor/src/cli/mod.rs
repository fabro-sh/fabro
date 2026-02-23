pub mod backend;
pub mod run;
pub mod validate;

use std::path::Path;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use terminal::Styles;

use crate::event::PipelineEvent;
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
        PipelineEvent::StageStarted { name, index } => {
            format!("[STAGE_STARTED] name={name} index={index}")
        }
        PipelineEvent::StageCompleted {
            name,
            index,
            duration_ms,
        } => {
            format!("[STAGE_COMPLETED] name={name} index={index} duration={duration_ms}ms")
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
        } => {
            format!(
                "[STAGE_FAILED] name={name} index={index} error=\"{error}\" will_retry={will_retry}"
            )
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
        PipelineEvent::ParallelStarted { branch_count } => {
            format!("[PARALLEL_STARTED] branches={branch_count}")
        }
        PipelineEvent::ParallelBranchStarted { branch, index } => {
            format!("[PARALLEL_BRANCH_STARTED] branch={branch} index={index}")
        }
        PipelineEvent::ParallelBranchCompleted {
            branch,
            index,
            duration_ms,
            success,
        } => {
            format!("[PARALLEL_BRANCH_COMPLETED] branch={branch} index={index} duration={duration_ms}ms success={success}")
        }
        PipelineEvent::ParallelCompleted {
            duration_ms,
            success_count,
            failure_count,
        } => {
            format!("[PARALLEL_COMPLETED] duration={duration_ms}ms succeeded={success_count} failed={failure_count}")
        }
        PipelineEvent::InterviewStarted { question, stage } => {
            format!("[INTERVIEW_STARTED] stage={stage} question=\"{question}\"")
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
        PipelineEvent::StageStarted { name, index } => {
            format!(
                "{d}── STAGE_STARTED ────────────────────────────{r}\n  {d}name:{r}  {name}\n  {d}index:{r} {index}\n"
            )
        }
        PipelineEvent::StageCompleted {
            name,
            index,
            duration_ms,
        } => {
            format!("{d}── STAGE_COMPLETED ──────────────────────────{r}\n  {d}name:{r}        {name}\n  {d}index:{r}       {index}\n  {d}duration_ms:{r} {duration_ms}\n")
        }
        PipelineEvent::StageFailed {
            name,
            index,
            error,
            will_retry,
        } => {
            format!("{d}── STAGE_FAILED ─────────────────────────────{r}\n  {d}name:{r}       {name}\n  {d}index:{r}      {index}\n  {d}error:{r}      {error}\n  {d}will_retry:{r} {will_retry}\n")
        }
        PipelineEvent::StageRetrying {
            name,
            index,
            attempt,
            delay_ms,
        } => {
            format!("{d}── STAGE_RETRYING ───────────────────────────{r}\n  {d}name:{r}     {name}\n  {d}index:{r}    {index}\n  {d}attempt:{r}  {attempt}\n  {d}delay_ms:{r} {delay_ms}\n")
        }
        PipelineEvent::ParallelStarted { branch_count } => {
            format!("{d}── PARALLEL_STARTED ─────────────────────────{r}\n  {d}branch_count:{r} {branch_count}\n")
        }
        PipelineEvent::ParallelBranchStarted { branch, index } => {
            format!("{d}── PARALLEL_BRANCH_STARTED ──────────────────{r}\n  {d}branch:{r} {branch}\n  {d}index:{r}  {index}\n")
        }
        PipelineEvent::ParallelBranchCompleted {
            branch,
            index,
            duration_ms,
            success,
        } => {
            format!("{d}── PARALLEL_BRANCH_COMPLETED ────────────────{r}\n  {d}branch:{r}      {branch}\n  {d}index:{r}       {index}\n  {d}duration_ms:{r} {duration_ms}\n  {d}success:{r}     {success}\n")
        }
        PipelineEvent::ParallelCompleted {
            duration_ms,
            success_count,
            failure_count,
        } => {
            format!("{d}── PARALLEL_COMPLETED ───────────────────────{r}\n  {d}duration_ms:{r}   {duration_ms}\n  {d}success_count:{r} {success_count}\n  {d}failure_count:{r} {failure_count}\n")
        }
        PipelineEvent::InterviewStarted { question, stage } => {
            format!("{d}── INTERVIEW_STARTED ────────────────────────{r}\n  {d}stage:{r}    {stage}\n  {d}question:{r} {question}\n")
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
    }
}
