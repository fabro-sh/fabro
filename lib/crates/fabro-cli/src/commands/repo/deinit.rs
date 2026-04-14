use anyhow::{Context, Result, bail};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;

pub(crate) fn run_deinit(cli: &CliSettings, printer: Printer) -> Result<Vec<String>> {
    let repo_root = super::init::git_repo_root()?;
    let mut removed = Vec::new();

    let fabro_dir = repo_root.join(".fabro");
    let project_toml = fabro_dir.join("project.toml");

    let green = console::Style::new().green();
    let dim = console::Style::new().dim();

    if !project_toml.exists() {
        bail!("not initialized — .fabro/project.toml not found");
    }

    std::fs::remove_dir_all(&fabro_dir)
        .with_context(|| format!("failed to remove {}", fabro_dir.display()))?;
    removed.push(".fabro/".to_string());
    if cli.output.format != OutputFormat::Json {
        fabro_util::printerr!(
            printer,
            "  {} {}",
            green.apply_to("✔"),
            dim.apply_to("removed .fabro/")
        );
    }

    if cli.output.format != OutputFormat::Json {
        fabro_util::printerr!(
            printer,
            "\n{}",
            console::Style::new()
                .bold()
                .apply_to("Project deinitialized.")
        );
    }

    Ok(removed)
}
