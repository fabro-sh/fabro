use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::process::Command as TokioCommand;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::args::UpgradeArgs;
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
                let mut file = fs::File::create(&dest)?;
                file.write_all(&bytes)?;
            }
        }
        Ok(dest)
    }
}

async fn select_backend() -> Backend {
    // Check if gh is available
    let gh_version = TokioCommand::new("gh").arg("--version").output().await;
    let Ok(output) = gh_version else {
        debug!("gh CLI not found, using HTTP backend");
        return Backend::Http(http_client().expect("failed to build HTTP client"));
    };
    if !output.status.success() {
        debug!("gh --version failed, using HTTP backend");
        return Backend::Http(http_client().expect("failed to build HTTP client"));
    }

    // Check if gh is authenticated
    let auth_status = TokioCommand::new("gh")
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
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
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
    std::io::copy(&mut file, &mut hasher)?;
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
}

impl UpgradeCheckState {
    fn is_stale(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.checked_at) >= CHECK_INTERVAL_SECS
    }

    fn load(path: &Path) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}

// ── Main upgrade command ───────────────────────────────────────────────────

pub(crate) async fn run_upgrade(
    args: UpgradeArgs,
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
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

    let current_exe = std::env::current_exe()?.canonicalize()?;
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
    let checksum_content = fs::read_to_string(&checksum_path)?;
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

    // Check cached state first
    if let Some(state) = UpgradeCheckState::load(&state_path) {
        if !state.is_stale() {
            if let Ok(latest) = Version::parse(&state.latest_version) {
                if latest > current {
                    print_notice(&current, &latest, printer);
                }
            }
            return Ok(());
        }
    }

    // Fetch latest version
    let backend = select_backend().await;
    let tag = backend.fetch_latest_release_tag().await?;
    let latest = parse_version_from_tag(&tag)?;

    // Save state
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let state = UpgradeCheckState {
        checked_at:     now,
        latest_version: latest.to_string(),
    };
    let _ = state.save(&state_path);

    if latest > current {
        print_notice(&current, &latest, printer);
    }

    Ok(())
}

fn print_notice(current: &Version, latest: &Version, printer: Printer) {
    fabro_util::printerr!(
        printer,
        "A new version of fabro is available: {latest} (current: {current})"
    );
    fabro_util::printerr!(printer, "Run `fabro upgrade` to update.");
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
            assert_eq!(result.unwrap(), "x86_64-unknown-linux-gnu");
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            assert_eq!(result.unwrap(), "aarch64-apple-darwin");
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            assert_eq!(result.unwrap(), "aarch64-unknown-linux-gnu");
        }
        // On other platforms it would return an error, which is fine
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
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: UpgradeCheckState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.checked_at, 1_710_000_000);
        assert_eq!(parsed.latest_version, "0.5.0");
    }

    #[test]
    fn upgrade_check_state_stale() {
        let old = UpgradeCheckState {
            checked_at:     0, // epoch — definitely stale
            latest_version: "0.1.0".to_string(),
        };
        assert!(old.is_stale());
    }

    #[test]
    fn upgrade_check_state_fresh() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let fresh = UpgradeCheckState {
            checked_at:     now,
            latest_version: "0.5.0".to_string(),
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
        };
        state.save(&path).unwrap();
        let loaded = UpgradeCheckState::load(&path).unwrap();
        assert_eq!(loaded.checked_at, 1_710_000_000);
        assert_eq!(loaded.latest_version, "0.5.0");
    }

    // -- Backend selection --

    #[tokio::test]
    async fn select_backend_returns_a_variant() {
        // Just ensure it doesn't panic; actual variant depends on environment
        let _backend = select_backend().await;
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
}
