use fabro_static::EnvVars;

/// When this environment variable is set to any value, [`try_open`] returns
/// `Ok(())` without launching a browser. Test harnesses set it so spawned
/// `fabro` subprocesses do not pop real browser windows during CI or local
/// runs.
#[expect(
    clippy::disallowed_methods,
    reason = "Browser launching checks the documented process-env escape hatch."
)]
pub fn try_open(url: &str) -> std::io::Result<()> {
    if std::env::var_os(EnvVars::FABRO_SUPPRESS_OPEN_BROWSER).is_some() {
        return Ok(());
    }
    open::that(url)
}
