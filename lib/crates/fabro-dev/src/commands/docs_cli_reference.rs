use std::path::Path;

use anyhow::{Context, Result, bail};

use super::{PlannedCommand, capture_command, replace_generated_region, workspace_root};

const CLI_REFERENCE_PATH: &str = "docs/public/reference/cli.mdx";
const FENCE_START: &str = "{/* generated:cli */}";
const FENCE_END: &str = "{/* /generated:cli */}";

#[expect(
    clippy::print_stdout,
    clippy::disallowed_methods,
    reason = "dev generator reports the generated docs path directly and intentionally uses sync filesystem I/O"
)]
pub(crate) fn docs_cli_reference_root(root: &Path, check: bool) -> Result<()> {
    let path = root.join(CLI_REFERENCE_PATH);
    let current =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let generated = render_cli_reference()?;
    let updated = replace_generated_region(
        &current,
        &generated,
        CLI_REFERENCE_PATH,
        FENCE_START,
        FENCE_END,
    )?;

    if check {
        if current != updated {
            bail!("{CLI_REFERENCE_PATH} is stale; run `cargo dev docs refresh`");
        }
        println!("{CLI_REFERENCE_PATH} is up to date.");
        return Ok(());
    }

    if current != updated {
        std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    }
    println!("Generated {CLI_REFERENCE_PATH}.");
    Ok(())
}

fn render_cli_reference() -> Result<String> {
    let command = PlannedCommand::new("cargo")
        .arg("run")
        .arg("--locked")
        .arg("-p")
        .arg("fabro-cli")
        .arg("--")
        .arg("__cli-reference");
    let output = capture_command(&workspace_root(), &command)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`fabro __cli-reference` failed with {}:\n{}",
            output.status,
            stderr.trim()
        );
    }

    String::from_utf8(output.stdout)
        .context("fabro __cli-reference emitted invalid UTF-8")
        .map(|output| output.trim_end().to_string())
}
