use anyhow::{Result, bail};
use fabro_sandbox::daytona::detect_repo_info;

pub(crate) fn ensure_matching_repo_origin(
    expected_origin_url: Option<&str>,
    action: &str,
) -> Result<()> {
    let Some(expected_origin_url) = expected_origin_url else {
        return Ok(());
    };

    let cwd = std::env::current_dir()?;
    let (origin_url, _) = detect_repo_info(&cwd).map_err(|_| {
        anyhow::anyhow!(
            "Current directory is not a git repository with an origin remote; refusing to {action} run from repository '{expected_origin_url}'"
        )
    })?;
    let current_origin_url = fabro_github::normalize_repo_origin_url(&origin_url);

    if current_origin_url != expected_origin_url {
        bail!(
            "Current repository origin '{current_origin_url}' does not match run repository '{expected_origin_url}'; refusing to {action} this run from the wrong checkout"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_matching_repo_origin;

    #[test]
    fn missing_expected_origin_skips_guard() {
        ensure_matching_repo_origin(None, "fork").unwrap();
    }
}
