use anyhow::{Result, bail};
use fabro_config::project::{
    WorkflowInfo, WorkflowSource, discover_project_config, list_workflows_detailed,
    resolve_fabro_root,
};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, WorkflowListArgs};
use crate::shared::{print_json_pretty, relative_path};

const GOAL_MAX_LEN: usize = 60;

pub(super) fn list_command(
    _args: &WorkflowListArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let styles = Styles::detect_stderr();
    let cwd = std::env::current_dir()?;

    let Some((config_path, config)) = discover_project_config(&cwd)? else {
        bail!(
            "No .fabro/project.toml found in {cwd} or any parent directory",
            cwd = cwd.display()
        );
    };

    let fabro_root = resolve_fabro_root(&config_path, &config);
    let project_wf_dir = fabro_root.join("workflows");
    let user_wf_dir = Some(fabro_util::Home::from_env().workflows_dir());

    let workflows = list_workflows_detailed(Some(&project_wf_dir), user_wf_dir.as_deref());

    if globals.json {
        print_json_pretty(&workflows)?;
        return Ok(());
    }

    let project: Vec<_> = workflows
        .iter()
        .filter(|w| w.source == WorkflowSource::Project)
        .collect();
    let user: Vec<_> = workflows
        .iter()
        .filter(|w| w.source == WorkflowSource::User)
        .collect();

    let name_width = workflows.iter().map(|w| w.name.len()).max().unwrap_or(0);

    fabro_util::printerr!(
        printer,
        "{} workflow(s) found\n",
        styles.bold.apply_to(workflows.len())
    );

    let user_path = user_wf_dir
        .as_deref()
        .map_or_else(|| "~/.fabro/workflows".to_string(), relative_path);
    print_section(
        "User Workflows",
        &user_path,
        &user,
        name_width,
        &styles,
        printer,
    );

    fabro_util::printerr!(printer, "");

    print_section(
        "Project Workflows",
        &relative_path(&project_wf_dir),
        &project,
        name_width,
        &styles,
        printer,
    );

    Ok(())
}

fn print_section(
    title: &str,
    path: &str,
    workflows: &[&WorkflowInfo],
    name_width: usize,
    styles: &Styles,
    printer: Printer,
) {
    fabro_util::printerr!(
        printer,
        "{} {}",
        styles.bold.apply_to(title),
        styles.dim.apply_to(format!("({path})")),
    );
    if workflows.is_empty() {
        fabro_util::printerr!(printer, "  {}", styles.dim.apply_to("(none)"));
        return;
    }
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "  {:<name_width$}  {}",
        styles.bold_dim.apply_to("NAME"),
        styles.bold_dim.apply_to("DESCRIPTION"),
    );
    for w in workflows {
        let goal_str = w
            .goal
            .as_deref()
            .map(|g| truncate_str(g, GOAL_MAX_LEN))
            .unwrap_or_default();
        fabro_util::printerr!(
            printer,
            "  {:<name_width$}  {}",
            styles.cyan.apply_to(&w.name),
            styles.dim.apply_to(goal_str),
        );
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..max - 3])
    }
}
