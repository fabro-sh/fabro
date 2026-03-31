use anyhow::{Context, Result, bail};

use crate::args::GlobalArgs;

pub(crate) fn run_deinit(globals: &GlobalArgs) -> Result<Vec<String>> {
    let repo_root = super::init::git_repo_root()?;
    let mut removed = Vec::new();

    let fabro_toml = repo_root.join("fabro.toml");

    let green = console::Style::new().green();
    let dim = console::Style::new().dim();

    match std::fs::remove_file(&fabro_toml) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("not initialized — fabro.toml not found");
        }
        Err(e) => bail!("failed to remove {}: {e}", fabro_toml.display()),
    }
    removed.push("fabro.toml".to_string());
    if !globals.json {
        eprintln!(
            "  {} {}",
            green.apply_to("✔"),
            dim.apply_to("removed fabro.toml")
        );
    }

    let fabro_dir = repo_root.join("fabro");
    if fabro_dir.exists() {
        std::fs::remove_dir_all(&fabro_dir)
            .with_context(|| format!("failed to remove {}", fabro_dir.display()))?;
        removed.push("fabro/".to_string());
        if !globals.json {
            eprintln!(
                "  {} {}",
                green.apply_to("✔"),
                dim.apply_to("removed fabro/")
            );
        }
    }

    if !globals.json {
        eprintln!(
            "\n{}",
            console::Style::new()
                .bold()
                .apply_to("Project deinitialized.")
        );
    }

    Ok(removed)
}
