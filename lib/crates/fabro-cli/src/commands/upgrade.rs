#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `upgrade` command: blocking std::io::Write and std::io::BufReader for \
              release download + prompt output"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `upgrade` command: interactive confirm prompt reads from std::io::stdin"
)]

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_types::settings::CliNamespace;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::process::Command as TokioCommand;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::args::UpgradeArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

// ── Download backend abstraction ───────────────────────────────────────────

const GITHUB_REPO: &str = "fabro-sh/fabro";

enum Backend {
    Gh,
    Http(fabro_http::HttpClient),
}

fn http_client() -> Result<fabro_http::HttpClient> {
    fabro_http::HttpClientBuilder::new()
        .user_agent("fabro-cli")
        .build()
        .context("failed to build HTTP client")
}

// ── Install source detection ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallSource {
    Tarball,
    Brew { channel: BrewChannel },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrewChannel {
    Stable,
    Nightly,
}

impl InstallSource {
    fn cache_tag(self) -> &'static str {
        match self {
            Self::Tarball => "tarball",
            Self::Brew {
                channel: BrewChannel::Stable,
            } => "brew-stable",
            Self::Brew {
                channel: BrewChannel::Nightly,
            } => "brew-nightly",
        }
    }
}

fn upgrade_command_for(source: InstallSource) -> &'static str {
    match source {
        InstallSource::Tarball => "fabro upgrade",
        InstallSource::Brew {
            channel: BrewChannel::Stable,
        } => "brew upgrade fabro",
        InstallSource::Brew {
            channel: BrewChannel::Nightly,
        } => "brew upgrade fabro-nightly",
    }
}

fn detect_install_source(current_exe: &Path) -> InstallSource {
    let s = current_exe.to_string_lossy();
    if s.contains("/Cellar/fabro-nightly/") {
        InstallSource::Brew {
            channel: BrewChannel::Nightly,
        }
    } else if s.contains("/Cellar/fabro/") {
        InstallSource::Brew {
            channel: BrewChannel::Stable,
        }
    } else {
        InstallSource::Tarball
    }
}

// ── Homebrew tap manifest ──────────────────────────────────────────────────

const TAP_VERSIONS_URL: &str =
    "https://raw.githubusercontent.com/fabro-sh/homebrew-tap/main/versions.json";

fn parse_tap_versions(body: &str, channel: BrewChannel) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body).context("parsing tap versions.json")?;
    let key = match channel {
        BrewChannel::Stable => "stable",
        BrewChannel::Nightly => "nightly",
    };
    v.get(key)
        .and_then(|c| c.get("version"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .with_context(|| format!("missing {key}.version in tap versions.json"))
}

async fn fetch_tap_version(
    client: &fabro_http::HttpClient,
    channel: BrewChannel,
) -> Result<String> {
    let resp = client
        .get(TAP_VERSIONS_URL)
        .send()
        .await
        .context("failed to fetch tap versions.json")?;
    if !resp.status().is_success() {
        bail!("tap versions.json returned HTTP {}", resp.status());
    }
    let body = resp.text().await?;
    parse_tap_versions(&body, channel)
}

impl Backend {
    async fn fetch_latest_release_tag(&self) -> Result<String> {
        match self {
            Self::Gh => {
                let output = TokioCommand::new("gh")
                    .args([
                        "api",
                        &format!("repos/{GITHUB_REPO}/releases/latest"),
                        "--jq",
                        ".tag_name",
                    ])
                    .output()
                    .await
                    .context("failed to run `gh api repos/.../releases/latest`")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("gh api repos/.../releases/latest failed: {stderr}");
                }
                Ok(String::from_utf8(output.stdout)?.trim().to_string())
            }
            Self::Http(client) => {
                let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .context("failed to fetch latest release from GitHub API")?;
                if !resp.status().is_success() {
                    bail!(
                        "GitHub API returned status {} when fetching latest release",
                        resp.status()
                    );
                }
                let json: serde_json::Value = resp.json().await?;
                let tag = json["tag_name"]
                    .as_str()
                    .context("missing tag_name in GitHub API response")?;
                Ok(tag.to_string())
            }
        }
    }

    async fn fetch_releases(&self) -> Result<Vec<ReleaseSummary>> {
        match self {
            Self::Gh => {
                let output = TokioCommand::new("gh")
                    .args(["api", &format!("repos/{GITHUB_REPO}/releases")])
                    .output()
                    .await
                    .context("failed to run `gh api repos/.../releases`")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("gh api repos/.../releases failed: {stderr}");
                }
                let releases: Vec<ReleaseSummary> = serde_json::from_slice(&output.stdout)
                    .context("failed to parse gh releases JSON")?;
                Ok(releases)
            }
            Self::Http(client) => {
                let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases");
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .context("failed to fetch releases from GitHub API")?;
                if !resp.status().is_success() {
                    bail!(
                        "GitHub API returned status {} when fetching releases",
                        resp.status()
                    );
                }
                let releases: Vec<ReleaseSummary> = resp.json().await?;
                Ok(releases)
            }
        }
    }

    async fn download_release(&self, tag: &str, asset: &str, dest_dir: &Path) -> Result<PathBuf> {
        let dest = dest_dir.join(asset);
        match self {
            Self::Gh => {
                let status = TokioCommand::new("gh")
                    .args([
                        "release",
                        "download",
                        tag,
                        "--repo",
                        GITHUB_REPO,
                        "--pattern",
                        asset,
                        "--dir",
                        &dest_dir.to_string_lossy(),
                        "--clobber",
                    ])
                    .status()
                    .await
                    .context("failed to run `gh release download`")?;
                if !status.success() {
                    bail!("gh release download failed with exit code {status}");
                }
            }
            Self::Http(client) => {
                let url =
                    format!("https://github.com/{GITHUB_REPO}/releases/download/{tag}/{asset}");
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .with_context(|| format!("failed to download {url}"))?;
                if !resp.status().is_success() {
                    bail!("download failed: HTTP {}", resp.status());
                }
                let bytes = resp.bytes().await?;
                let mut file = fs::File::create(&dest)
                    .with_context(|| format!("creating download file {}", dest.display()))?;
                file.write_all(&bytes)
                    .with_context(|| format!("writing download to {}", dest.display()))?;
            }
        }
        Ok(dest)
    }
}

async fn select_backend() -> Backend {
    select_backend_for_gh_command("gh").await
}

async fn select_backend_for_gh_command(gh_command: &str) -> Backend {
    // Check if gh is available
    let gh_version = TokioCommand::new(gh_command)
        .arg("--version")
        .output()
        .await;
    let Ok(output) = gh_version else {
        debug!("gh CLI not found, using HTTP backend");
        return Backend::Http(http_client().expect("failed to build HTTP client"));
    };
    if !output.status.success() {
        debug!("gh --version failed, using HTTP backend");
        return Backend::Http(http_client().expect("failed to build HTTP client"));
    }

    // Check if gh is authenticated
    let auth_status = TokioCommand::new(gh_command)
        .args(["auth", "status"])
        .output()
        .await;
    match auth_status {
        Ok(o) if o.status.success() => {
            debug!("gh CLI available and authenticated, using Gh backend");
            Backend::Gh
        }
        _ => {
            debug!("gh not authenticated, using HTTP backend");
            Backend::Http(http_client().expect("failed to build HTTP client"))
        }
    }
}

// ── Platform detection ─────────────────────────────────────────────────────

fn detect_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok(match detect_linux_libc() {
            Libc::Musl => "x86_64-unknown-linux-musl",
            Libc::Gnu => "x86_64-unknown-linux-gnu",
        }),
        ("linux", "aarch64") => Ok(match detect_linux_libc() {
            Libc::Musl => "aarch64-unknown-linux-musl",
            Libc::Gnu => "aarch64-unknown-linux-gnu",
        }),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Libc {
    Gnu,
    Musl,
}

/// Parse `ldd --version` output to determine the host libc flavor.
///
/// glibc's ldd writes to stdout ("ldd (Ubuntu GLIBC 2.35...)");
/// musl's ldd writes to stderr ("musl libc (x86_64)\nVersion 1.2.4").
/// Callers concatenate both streams before passing in.
fn parse_ldd_libc(output: &str) -> Libc {
    if output.to_ascii_lowercase().contains("musl") {
        Libc::Musl
    } else {
        Libc::Gnu
    }
}

fn detect_linux_libc() -> Libc {
    #[expect(
        clippy::disallowed_methods,
        reason = "one-shot libc detection at upgrade startup; async overhead is unwarranted"
    )]
    let result = std::process::Command::new("ldd").arg("--version").output();
    let Ok(output) = result else {
        return Libc::Gnu;
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    parse_ldd_libc(&combined)
}

// ── Version helpers ────────────────────────────────────────────────────────

fn parse_version_from_tag(tag: &str) -> Result<Version> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(stripped).with_context(|| format!("invalid version: {tag}"))
}

// ── Release listing (for --prerelease) ─────────────────────────────────────

#[derive(serde::Deserialize)]
struct ReleaseSummary {
    tag_name: String,
    #[serde(default)]
    draft:    bool,
}

/// Pick the `tag_name` with the highest semver from `releases`, skipping
/// drafts and entries whose `tag_name` does not parse as semver. Returns
/// `None` if no candidate remains (caller may fall back to stable-latest).
fn pick_latest_tag(releases: &[ReleaseSummary]) -> Option<String> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            parse_version_from_tag(&r.tag_name)
                .ok()
                .map(|v| (v, &r.tag_name))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, tag)| tag.clone())
}

// ── SHA256 verification ────────────────────────────────────────────────────

fn verify_checksum(path: &Path, expected_hex: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut file = std::io::BufReader::new(
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
    );
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("reading {} for checksum", path.display()))?;
    let computed = format!("{:x}", hasher.finalize());
    // The .sha256 file may contain "hash  filename" or just "hash"
    let expected = expected_hex
        .split_whitespace()
        .next()
        .unwrap_or(expected_hex)
        .to_lowercase();
    if computed != expected {
        bail!("SHA256 mismatch: expected {expected}, got {computed}");
    }
    Ok(())
}

// ── Upgrade check state ────────────────────────────────────────────────────

const CHECK_INTERVAL_SECS: u64 = 86400; // 24 hours
const LAST_CHECK_FILE: &str = "last_upgrade_check.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct UpgradeCheckState {
    checked_at:     u64,
    latest_version: String,
    #[serde(default)]
    install_source: Option<String>,
}

impl UpgradeCheckState {
    fn is_stale(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.checked_at) >= CHECK_INTERVAL_SECS
    }

    fn is_usable_for(&self, source: InstallSource) -> bool {
        !self.is_stale() && self.install_source.as_deref() == Some(source.cache_tag())
    }

    fn load(path: &Path) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
        let json = serde_json::to_string(self)?;
        fs::write(path, json)
            .with_context(|| format!("writing upgrade check state {}", path.display()))?;
        Ok(())
    }
}

// ── Main upgrade command ───────────────────────────────────────────────────

pub(crate) async fn run_upgrade(args: UpgradeArgs, ctx: &CommandContext) -> Result<()> {
    let cli = &ctx.user_settings().cli;
    let printer = ctx.printer();
    let current_exe = std::env::current_exe()
        .context("resolving current fabro executable path")?
        .canonicalize()
        .context("canonicalizing current fabro executable path")?;

    if let InstallSource::Brew { channel } = detect_install_source(&current_exe) {
        return run_upgrade_brew(&args, cli, printer, channel);
    }

    let backend = select_backend().await;

    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).context("failed to parse current version")?;

    // Determine target version
    let (target, tag) = if let Some(ref v) = args.version {
        let version = parse_version_from_tag(v)?;
        let tag = format!("v{version}");
        (version, tag)
    } else {
        let tag = if args.prerelease {
            match pick_latest_tag(&backend.fetch_releases().await?) {
                Some(t) => t,
                None => backend.fetch_latest_release_tag().await?,
            }
        } else {
            backend.fetch_latest_release_tag().await?
        };
        let version = parse_version_from_tag(&tag)?;
        (version, tag)
    };

    // Downgrade protection
    match target.cmp(&current) {
        std::cmp::Ordering::Less => {
            if args.version.is_none() {
                bail!(
                    "latest release ({target}) is older than installed version ({current}), skipping"
                );
            }
            // Explicit --version: warn + prompt
            fabro_util::printerr!(printer, "Warning: downgrading from {current} to {target}");
            if std::io::stdin().is_terminal() {
                let confirm = dialoguer::Confirm::new()
                    .with_prompt("Continue with downgrade?")
                    .default(false)
                    .interact()?;
                if !confirm {
                    bail!("downgrade cancelled");
                }
            } else {
                bail!("downgrade requires interactive confirmation (stdin is not a tty)");
            }
        }
        std::cmp::Ordering::Equal if !args.force => {
            if cli.output.format == OutputFormat::Json {
                print_json_pretty(&serde_json::json!({
                    "previous_version": current.to_string(),
                    "installed_version": current.to_string(),
                }))?;
            } else {
                fabro_util::printerr!(printer, "Already on version {current}");
            }
            return Ok(());
        }
        _ => {}
    }

    if args.dry_run {
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&serde_json::json!({
                "previous_version": current.to_string(),
                "installed_version": target.to_string(),
                "dry_run": true,
            }))?;
        } else {
            fabro_util::printerr!(printer, "Would upgrade fabro from {current} to {target}");
            fabro_util::printerr!(printer, "  tag: {tag}");
            fabro_util::printerr!(printer, "  target: {}", detect_target()?);
        }
        return Ok(());
    }

    let triple = detect_target()?;
    let tarball_name = format!("fabro-{triple}.tar.gz");
    let checksum_name = format!("{tarball_name}.sha256");

    let exe_dir = current_exe
        .parent()
        .context("could not determine executable directory")?;

    let tmp_dir = tempfile::tempdir_in(exe_dir)
        .or_else(|_| tempfile::tempdir())
        .context("failed to create temp directory")?;

    // Download tarball and checksum in parallel
    fabro_util::printerr!(printer, "Downloading fabro {target}...");
    let (tarball_path, checksum_path) = tokio::try_join!(
        backend.download_release(&tag, &tarball_name, tmp_dir.path()),
        backend.download_release(&tag, &checksum_name, tmp_dir.path()),
    )?;

    // Verify SHA256 using streaming hash
    let checksum_content = fs::read_to_string(&checksum_path)
        .with_context(|| format!("reading checksum file {}", checksum_path.display()))?;
    verify_checksum(&tarball_path, &checksum_content)?;
    debug!("SHA256 checksum verified");

    // Extract tarball
    let status = TokioCommand::new("tar")
        .args([
            "xzf",
            &tarball_path.to_string_lossy(),
            "-C",
            &tmp_dir.path().to_string_lossy(),
        ])
        .status()
        .await
        .context("failed to run tar")?;
    if !status.success() {
        bail!("tar extraction failed");
    }

    // Atomic binary replacement
    let extracted_binary = tmp_dir.path().join(format!("fabro-{triple}")).join("fabro");
    let backup = exe_dir.join(".fabro-upgrade-backup");
    fs::rename(&current_exe, &backup).context("failed to move current binary to backup")?;
    if let Err(e) = fs::rename(&extracted_binary, &current_exe) {
        // Restore from backup
        if let Err(restore_err) = fs::rename(&backup, &current_exe) {
            bail!(
                "failed to install new binary ({e}) and failed to restore backup ({restore_err})"
            );
        }
        bail!("failed to install new binary: {e}");
    }
    let _ = fs::remove_file(&backup);

    // Set permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755));
    }

    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({
            "previous_version": current.to_string(),
            "installed_version": target.to_string(),
        }))?;
    } else {
        fabro_util::printerr!(printer, "Upgraded fabro to {target}");
    }
    Ok(())
}

// ── Homebrew-managed upgrade path ──────────────────────────────────────────

fn run_upgrade_brew(
    args: &UpgradeArgs,
    cli: &CliNamespace,
    printer: Printer,
    channel: BrewChannel,
) -> Result<()> {
    let formula = match channel {
        BrewChannel::Stable => "fabro",
        BrewChannel::Nightly => "fabro-nightly",
    };
    let cmd = format!("brew upgrade {formula}");

    if args.version.is_some() || args.prerelease || args.force {
        bail!(
            "fabro is managed by Homebrew (formula `{formula}`); Homebrew selects the version \
             and channel. Use `brew upgrade {formula}` (or reinstall with a different formula) \
             instead of `fabro upgrade --version`/`--prerelease`/`--force`."
        );
    }

    if args.dry_run {
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&serde_json::json!({
                "install_source": "homebrew",
                "formula": formula,
                "brew_command": cmd,
                "dry_run": true,
                "note": "fabro is Homebrew-managed; no in-place upgrade attempted",
            }))?;
        } else {
            fabro_util::printerr!(printer, "fabro was installed via Homebrew.");
            fabro_util::printerr!(printer, "Run `{cmd}` to update.");
        }
        return Ok(());
    }

    fabro_util::printerr!(printer, "fabro was installed via Homebrew.");
    fabro_util::printerr!(printer, "Run `{cmd}` to update.");
    bail!("refusing to overwrite a Homebrew-managed binary");
}

// ── Auto version check ────────────────────────────────────────────────────

/// Spawn a background task that checks for a newer version and prints a notice
/// to stderr after the main command completes. Returns a handle that should be
/// awaited at the end of `main_inner`.
pub(crate) fn spawn_upgrade_check(check: bool, printer: Printer) -> Option<JoinHandle<()>> {
    if !check {
        return None;
    }
    Some(tokio::spawn(async move {
        if let Err(e) = check_and_print_notice(printer).await {
            debug!(%e, "Upgrade check failed (silently swallowed)");
        }
    }))
}

async fn check_and_print_notice(printer: Printer) -> Result<()> {
    let state_path = fabro_util::Home::from_env().root().join(LAST_CHECK_FILE);

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;

    let current_exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            debug!(%e, "skipping upgrade notice: cannot resolve current executable");
            return Ok(());
        }
    };
    let source = detect_install_source(&current_exe);

    if let Some(state) = UpgradeCheckState::load(&state_path) {
        if state.is_usable_for(source) {
            if let Ok(latest) = Version::parse(&state.latest_version) {
                if latest > current {
                    print_notice(&current, &latest, source, printer);
                }
            }
            return Ok(());
        }
    }

    let latest = match source {
        InstallSource::Brew { channel } => {
            let client = match http_client() {
                Ok(c) => c,
                Err(e) => {
                    debug!(%e, "skipping upgrade notice: failed to build HTTP client");
                    return Ok(());
                }
            };
            match fetch_tap_version(&client, channel).await {
                Ok(v) => parse_version_from_tag(&v)?,
                Err(e) => {
                    debug!(%e, "skipping upgrade notice: tap manifest fetch failed");
                    return Ok(());
                }
            }
        }
        InstallSource::Tarball => {
            let backend = select_backend().await;
            let tag = backend.fetch_latest_release_tag().await?;
            parse_version_from_tag(&tag)?
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let state = UpgradeCheckState {
        checked_at:     now,
        latest_version: latest.to_string(),
        install_source: Some(source.cache_tag().to_string()),
    };
    let _ = state.save(&state_path);

    if latest > current {
        print_notice(&current, &latest, source, printer);
    }

    Ok(())
}

fn print_notice(current: &Version, latest: &Version, source: InstallSource, printer: Printer) {
    fabro_util::printerr!(
        printer,
        "A new version of fabro is available: {latest} (current: {current})"
    );
    fabro_util::printerr!(printer, "Run `{}` to update.", upgrade_command_for(source));
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Platform detection --

    #[test]
    fn detect_target_returns_known_triple() {
        let result = detect_target();
        // We can only assert it succeeds on known CI platforms
        if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            let got = result.unwrap();
            assert!(
                got == "x86_64-unknown-linux-gnu" || got == "x86_64-unknown-linux-musl",
                "got {got}"
            );
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            assert_eq!(result.unwrap(), "aarch64-apple-darwin");
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            let got = result.unwrap();
            assert!(
                got == "aarch64-unknown-linux-gnu" || got == "aarch64-unknown-linux-musl",
                "got {got}"
            );
        }
        // On other platforms it would return an error, which is fine
    }

    // -- Libc parsing --

    #[test]
    fn parse_ldd_libc_detects_glibc() {
        assert_eq!(
            parse_ldd_libc(
                "ldd (Ubuntu GLIBC 2.35-0ubuntu3.4) 2.35\nCopyright (C) 2022 Free Software Foundation, Inc."
            ),
            Libc::Gnu,
        );
    }

    #[test]
    fn parse_ldd_libc_detects_musl() {
        assert_eq!(
            parse_ldd_libc("musl libc (x86_64)\nVersion 1.2.4\nDynamic Program Loader"),
            Libc::Musl,
        );
    }

    #[test]
    fn parse_ldd_libc_defaults_to_gnu_on_unknown_output() {
        assert_eq!(parse_ldd_libc(""), Libc::Gnu);
        assert_eq!(parse_ldd_libc("some unrelated output"), Libc::Gnu);
    }

    // -- Version parsing --

    #[test]
    fn parse_version_from_tag_with_v_prefix() {
        let v = parse_version_from_tag("v0.5.0").unwrap();
        assert_eq!(v, Version::new(0, 5, 0));
    }

    #[test]
    fn parse_version_from_tag_without_prefix() {
        let v = parse_version_from_tag("0.5.0").unwrap();
        assert_eq!(v, Version::new(0, 5, 0));
    }

    #[test]
    fn parse_version_from_tag_invalid() {
        assert!(parse_version_from_tag("not-a-version").is_err());
    }

    // -- SHA256 verification --

    #[test]
    fn verify_checksum_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        fs::write(&path, b"hello world").unwrap();
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_checksum(&path, expected).is_ok());
    }

    #[test]
    fn verify_checksum_with_filename_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        fs::write(&path, b"hello world").unwrap();
        let expected =
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  fabro.tar.gz";
        assert!(verify_checksum(&path, expected).is_ok());
    }

    #[test]
    fn verify_checksum_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        fs::write(&path, b"hello world").unwrap();
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_checksum(&path, wrong).is_err());
    }

    // -- Upgrade check state --

    #[test]
    fn upgrade_check_state_roundtrip() {
        let state = UpgradeCheckState {
            checked_at:     1_710_000_000,
            latest_version: "0.5.0".to_string(),
            install_source: Some("tarball".to_string()),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: UpgradeCheckState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.checked_at, 1_710_000_000);
        assert_eq!(parsed.latest_version, "0.5.0");
        assert_eq!(parsed.install_source.as_deref(), Some("tarball"));
    }

    #[test]
    fn upgrade_check_state_stale() {
        let old = UpgradeCheckState {
            checked_at:     0, // epoch — definitely stale
            latest_version: "0.1.0".to_string(),
            install_source: Some("tarball".to_string()),
        };
        assert!(old.is_stale());
    }

    #[test]
    fn upgrade_check_state_fresh() {
        let fresh = UpgradeCheckState {
            checked_at:     now_secs(),
            latest_version: "0.5.0".to_string(),
            install_source: Some("tarball".to_string()),
        };
        assert!(!fresh.is_stale());
    }

    #[test]
    fn upgrade_check_state_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = UpgradeCheckState {
            checked_at:     1_710_000_000,
            latest_version: "0.5.0".to_string(),
            install_source: Some("brew-stable".to_string()),
        };
        state.save(&path).unwrap();
        let loaded = UpgradeCheckState::load(&path).unwrap();
        assert_eq!(loaded.checked_at, 1_710_000_000);
        assert_eq!(loaded.latest_version, "0.5.0");
        assert_eq!(loaded.install_source.as_deref(), Some("brew-stable"));
    }

    // -- Backend selection --

    #[tokio::test]
    async fn select_backend_falls_back_to_http_when_gh_is_missing() {
        let backend = select_backend_for_gh_command("fabro-test-gh-that-should-not-exist").await;
        assert!(matches!(backend, Backend::Http(_)));
    }

    // -- Release selection --

    fn release(tag: &str, draft: bool) -> ReleaseSummary {
        ReleaseSummary {
            tag_name: tag.to_string(),
            draft,
        }
    }

    #[test]
    fn pick_latest_tag_prefers_newer_prerelease_when_stable_is_older() {
        let releases = [
            release("v0.204.0-beta.0", false),
            release("v0.203.0", false),
            release("v0.202.0", false),
        ];
        assert_eq!(
            pick_latest_tag(&releases).as_deref(),
            Some("v0.204.0-beta.0")
        );
    }

    #[test]
    fn pick_latest_tag_prefers_newer_stable_over_older_prerelease() {
        let releases = [
            release("v0.205.0", false),
            release("v0.205.0-beta.1", false),
            release("v0.204.0", false),
        ];
        assert_eq!(pick_latest_tag(&releases).as_deref(), Some("v0.205.0"));
    }

    #[test]
    fn pick_latest_tag_filters_drafts() {
        let releases = [
            release("v0.300.0", true),
            release("v0.204.0-beta.0", false),
            release("v0.203.0", false),
        ];
        assert_eq!(
            pick_latest_tag(&releases).as_deref(),
            Some("v0.204.0-beta.0")
        );
    }

    #[test]
    fn pick_latest_tag_skips_unparseable_tags() {
        let releases = [
            release("weekly-build-3", false),
            release("v0.204.0-beta.0", false),
        ];
        assert_eq!(
            pick_latest_tag(&releases).as_deref(),
            Some("v0.204.0-beta.0")
        );
    }

    #[test]
    fn pick_latest_tag_returns_none_when_all_drafts() {
        let releases = [release("v0.300.0", true), release("v0.299.0", true)];
        assert_eq!(pick_latest_tag(&releases), None);
    }

    #[test]
    fn pick_latest_tag_returns_none_for_empty_input() {
        assert_eq!(pick_latest_tag(&[]), None);
    }

    // -- Install source detection --

    #[test]
    fn detect_install_source_macos_arm_cellar() {
        let p = PathBuf::from("/opt/homebrew/Cellar/fabro/0.176.2/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Brew {
            channel: BrewChannel::Stable,
        });
    }

    #[test]
    fn detect_install_source_macos_intel_cellar() {
        let p = PathBuf::from("/usr/local/Cellar/fabro/0.176.2/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Brew {
            channel: BrewChannel::Stable,
        });
    }

    #[test]
    fn detect_install_source_linuxbrew() {
        let p = PathBuf::from("/home/linuxbrew/.linuxbrew/Cellar/fabro/0.176.2/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Brew {
            channel: BrewChannel::Stable,
        });
    }

    #[test]
    fn detect_install_source_nightly() {
        let p = PathBuf::from("/opt/homebrew/Cellar/fabro-nightly/0.205.0-nightly.0/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Brew {
            channel: BrewChannel::Nightly,
        });
    }

    #[test]
    fn detect_install_source_tarball() {
        let p = PathBuf::from("/usr/local/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Tarball);
    }

    #[test]
    fn detect_install_source_home() {
        let p = PathBuf::from("/Users/foo/.fabro/bin/fabro");
        assert_eq!(detect_install_source(&p), InstallSource::Tarball);
    }

    // -- Tap versions.json parsing --

    const TAP_FIXTURE: &str = r#"{
        "stable":  { "version": "0.204.0",           "tag": "v0.204.0" },
        "nightly": { "version": "0.205.0-nightly.0", "tag": "v0.205.0-nightly.0" }
    }"#;

    #[test]
    fn parse_tap_versions_stable() {
        assert_eq!(
            parse_tap_versions(TAP_FIXTURE, BrewChannel::Stable).unwrap(),
            "0.204.0"
        );
    }

    #[test]
    fn parse_tap_versions_nightly() {
        assert_eq!(
            parse_tap_versions(TAP_FIXTURE, BrewChannel::Nightly).unwrap(),
            "0.205.0-nightly.0"
        );
    }

    #[test]
    fn parse_tap_versions_missing_channel_errors() {
        let body = r#"{ "stable": { "version": "0.1.0" } }"#;
        assert!(parse_tap_versions(body, BrewChannel::Nightly).is_err());
    }

    #[test]
    fn parse_tap_versions_malformed_errors() {
        assert!(parse_tap_versions("not json", BrewChannel::Stable).is_err());
    }

    // -- Source-aware cache --

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn cache_is_usable_when_source_matches() {
        let state = UpgradeCheckState {
            checked_at:     now_secs(),
            latest_version: "0.5.0".into(),
            install_source: Some("brew-stable".into()),
        };
        assert!(state.is_usable_for(InstallSource::Brew {
            channel: BrewChannel::Stable,
        }));
    }

    #[test]
    fn cache_ignored_when_channel_differs() {
        let state = UpgradeCheckState {
            checked_at:     now_secs(),
            latest_version: "0.5.0".into(),
            install_source: Some("brew-stable".into()),
        };
        assert!(!state.is_usable_for(InstallSource::Brew {
            channel: BrewChannel::Nightly,
        }));
    }

    #[test]
    fn cache_ignored_when_source_differs() {
        let state = UpgradeCheckState {
            checked_at:     now_secs(),
            latest_version: "0.5.0".into(),
            install_source: Some("tarball".into()),
        };
        assert!(!state.is_usable_for(InstallSource::Brew {
            channel: BrewChannel::Stable,
        }));
    }

    #[test]
    fn legacy_cache_without_install_source_is_not_usable() {
        let state = UpgradeCheckState {
            checked_at:     now_secs(),
            latest_version: "0.5.0".into(),
            install_source: None,
        };
        assert!(!state.is_usable_for(InstallSource::Tarball));
        assert!(!state.is_usable_for(InstallSource::Brew {
            channel: BrewChannel::Stable,
        }));
    }

    #[test]
    fn legacy_cache_json_deserializes_with_none_source() {
        // Old payload shape — no install_source field.
        let json = r#"{"checked_at":1710000000,"latest_version":"0.5.0"}"#;
        let state: UpgradeCheckState = serde_json::from_str(json).unwrap();
        assert_eq!(state.install_source, None);
    }

    // -- Notice command selection --

    #[test]
    fn upgrade_command_tarball() {
        assert_eq!(upgrade_command_for(InstallSource::Tarball), "fabro upgrade");
    }

    #[test]
    fn upgrade_command_brew_stable() {
        assert_eq!(
            upgrade_command_for(InstallSource::Brew {
                channel: BrewChannel::Stable,
            }),
            "brew upgrade fabro"
        );
    }

    #[test]
    fn upgrade_command_brew_nightly() {
        assert_eq!(
            upgrade_command_for(InstallSource::Brew {
                channel: BrewChannel::Nightly,
            }),
            "brew upgrade fabro-nightly"
        );
    }

    // -- Brew-managed run_upgrade path --

    fn brew_args(
        version: Option<&str>,
        prerelease: bool,
        force: bool,
        dry_run: bool,
    ) -> UpgradeArgs {
        UpgradeArgs {
            version: version.map(str::to_string),
            prerelease,
            force,
            dry_run,
        }
    }

    #[test]
    fn run_upgrade_brew_refuses_by_default() {
        let cli = CliNamespace::default();
        let err = run_upgrade_brew(
            &brew_args(None, false, false, false),
            &cli,
            Printer::Silent,
            BrewChannel::Stable,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Homebrew"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn run_upgrade_brew_dry_run_returns_ok() {
        let cli = CliNamespace::default();
        let result = run_upgrade_brew(
            &brew_args(None, false, false, true),
            &cli,
            Printer::Silent,
            BrewChannel::Nightly,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_upgrade_brew_rejects_version_flag() {
        let cli = CliNamespace::default();
        let err = run_upgrade_brew(
            &brew_args(Some("0.1.0"), false, false, false),
            &cli,
            Printer::Silent,
            BrewChannel::Stable,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Homebrew"));
    }

    #[test]
    fn run_upgrade_brew_rejects_prerelease_flag() {
        let cli = CliNamespace::default();
        let err = run_upgrade_brew(
            &brew_args(None, true, false, false),
            &cli,
            Printer::Silent,
            BrewChannel::Stable,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Homebrew"));
    }

    #[test]
    fn run_upgrade_brew_rejects_force_flag() {
        let cli = CliNamespace::default();
        let err = run_upgrade_brew(
            &brew_args(None, false, true, false),
            &cli,
            Printer::Silent,
            BrewChannel::Stable,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Homebrew"));
    }
}
