use std::io::Write;

use fabro_config::project::resolve_workflow;
use fabro_graphviz::parser::parse_ast;

use crate::args::ParseArgs;
use crate::shared::read_workflow_file;

pub fn run(args: &ParseArgs) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    run_to(args, stdout.lock())
}

fn run_to(args: &ParseArgs, mut out: impl Write) -> anyhow::Result<()> {
    let (dot_path, _cfg) = resolve_workflow(&args.workflow)?;
    let source = read_workflow_file(&dot_path)?;
    let ast = parse_ast(&source)?;
    serde_json::to_writer_pretty(&mut out, &ast)?;
    writeln!(out)?;
    Ok(())
}
