use anyhow::{bail, Context, Result};

pub fn run_deinit() -> Result<()> {
    let repo_root = super::init::git_repo_root()?;

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
    eprintln!(
        "  {} {}",
        green.apply_to("✔"),
        dim.apply_to("removed fabro.toml")
    );

    let fabro_dir = repo_root.join("fabro");
    if fabro_dir.exists() {
        std::fs::remove_dir_all(&fabro_dir)
            .with_context(|| format!("failed to remove {}", fabro_dir.display()))?;
        eprintln!(
            "  {} {}",
            green.apply_to("✔"),
            dim.apply_to("removed fabro/")
        );
    }

    eprintln!(
        "\n{}",
        console::Style::new()
            .bold()
            .apply_to("Project deinitialized.")
    );

    Ok(())
}
