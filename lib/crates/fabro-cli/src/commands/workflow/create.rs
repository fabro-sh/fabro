use std::path::Path;

use anyhow::{bail, Context, Result};

use fabro_config::project::{discover_project_config, resolve_fabro_root};

use crate::args::WorkflowCreateArgs;
use crate::shared::relative_path;

pub fn create_command(args: &WorkflowCreateArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;

    let (config_path, config) = match discover_project_config(&cwd)? {
        Some(found) => found,
        None => bail!(
            "No fabro.toml found in {cwd} or any parent directory",
            cwd = cwd.display()
        ),
    };

    let fabro_root = resolve_fabro_root(&config_path, &config);
    write_workflow_scaffold(args, &fabro_root)?;

    let workflows_dir = fabro_root.join("workflows").join(&args.name);
    let green = console::Style::new().green();
    let bold = console::Style::new().bold();
    let cyan_bold = console::Style::new().cyan().bold();
    let dim = console::Style::new().dim();

    let rel_dir = relative_path(&workflows_dir);
    eprintln!(
        "  {} {}",
        green.apply_to("✔"),
        dim.apply_to(format!("{rel_dir}/workflow.fabro"))
    );
    eprintln!(
        "  {} {}",
        green.apply_to("✔"),
        dim.apply_to(format!("{rel_dir}/workflow.toml"))
    );

    eprintln!("\n{} Next steps:\n", bold.apply_to("Workflow created!"));
    eprintln!(
        "  1. Edit the graph:  {}",
        cyan_bold.apply_to(format!("{rel_dir}/workflow.fabro"))
    );
    eprintln!(
        "  2. Validate:        {}",
        cyan_bold.apply_to(format!("fabro validate {}", args.name))
    );
    eprintln!(
        "  3. Run:             {}",
        cyan_bold.apply_to(format!("fabro run {}", args.name))
    );

    Ok(())
}

fn write_workflow_scaffold(args: &WorkflowCreateArgs, fabro_root: &Path) -> Result<()> {
    let workflows_dir = fabro_root.join("workflows").join(&args.name);

    if workflows_dir.exists() {
        bail!(
            "Workflow '{}' already exists at {}",
            args.name,
            workflows_dir.display()
        );
    }

    std::fs::create_dir_all(&workflows_dir)
        .with_context(|| format!("failed to create {}", workflows_dir.display()))?;

    let goal = args.goal.as_deref().unwrap_or("TODO: describe the goal");
    let digraph_name = to_pascal_case(&args.name);

    let fabro_content = format!(
        r#"digraph {digraph_name} {{
    graph [goal="{goal}"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    main [label="Main", prompt="TODO: describe what this agent should do"]

    start -> main -> exit
}}
"#
    );

    let dot_path = workflows_dir.join("workflow.fabro");
    std::fs::write(&dot_path, &fabro_content)
        .with_context(|| format!("failed to write {}", dot_path.display()))?;

    let toml_path = workflows_dir.join("workflow.toml");
    std::fs::write(&toml_path, "version = 1\n")
        .with_context(|| format!("failed to write {}", toml_path.display()))?;

    Ok(())
}

fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{upper}{rest}", rest = chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect()
}
