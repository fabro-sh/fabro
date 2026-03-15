use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use semver::Version;
use sha2::{Digest, Sha256};
use tracing::debug;

// ── Clap args ──────────────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct UpgradeArgs {
    /// Target version (e.g. "0.5.0" or "v0.5.0")
    #[arg(long)]
    version: Option<String>,

    /// Upgrade even if already on the target version
    #[arg(long)]
    force: bool,

    /// Preview what would happen without making changes
    #[arg(long)]
    dry_run: bool,
}

// ── Download backend abstraction ───────────────────────────────────────────

enum Backend {
    Gh,
    Http,
}

impl Backend {
    async fn fetch_latest_release_tag(&self) -> Result<String> {
        match self {
            Backend::Gh => {
                let output = tokio::process::Command::new("gh")
                    .args([
                        "release",
                        "view",
                        "--repo",
                        "fabro-sh/fabro",
                        "--json",
                        "tagName",
                        "-q",
                        ".tagName",
                    ])
                    .output()
                    .await
                    .context("failed to run `gh release view`")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("gh release view failed: {stderr}");
                }
                Ok(String::from_utf8(output.stdout)?.trim().to_string())
            }
            Backend::Http => {
                let client = reqwest::Client::builder().user_agent("fabro-cli").build()?;
                let resp = client
                    .get("https://api.github.com/repos/fabro-sh/fabro/releases/latest")
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

    async fn download_release(&self, tag: &str, asset: &str, dest_dir: &Path) -> Result<PathBuf> {
        let dest = dest_dir.join(asset);
        match self {
            Backend::Gh => {
                let status = tokio::process::Command::new("gh")
                    .args([
                        "release",
                        "download",
                        tag,
                        "--repo",
                        "fabro-sh/fabro",
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
            Backend::Http => {
                let url =
                    format!("https://github.com/fabro-sh/fabro/releases/download/{tag}/{asset}");
                let client = reqwest::Client::builder().user_agent("fabro-cli").build()?;
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

fn select_backend() -> Backend {
    // Check if gh is available
    let gh_version = std::process::Command::new("gh").arg("--version").output();
    let Ok(output) = gh_version else {
        debug!("gh CLI not found, using HTTP backend");
        return Backend::Http;
    };
    if !output.status.success() {
        debug!("gh --version failed, using HTTP backend");
        return Backend::Http;
    }

    // Check if gh is authenticated
    let auth_status = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output();
    match auth_status {
        Ok(o) if o.status.success() => {
            debug!("gh CLI available and authenticated, using Gh backend");
            Backend::Gh
        }
        _ => {
            debug!("gh not authenticated, using HTTP backend");
            Backend::Http
        }
    }
}

// ── Platform detection ─────────────────────────────────────────────────────

fn detect_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
}

// ── Version helpers ────────────────────────────────────────────────────────

fn parse_version_from_tag(tag: &str) -> Result<Version> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(stripped).with_context(|| format!("invalid version: {tag}"))
}

#[derive(Debug, PartialEq)]
enum VersionComparison {
    /// A newer version is available
    Newer,
    /// Already on the target version
    AlreadyCurrent,
    /// Target is older than current (downgrade)
    Downgrade,
}

fn compare_versions(current: &Version, target: &Version) -> VersionComparison {
    use std::cmp::Ordering;
    match target.cmp(current) {
        Ordering::Greater => VersionComparison::Newer,
        Ordering::Equal => VersionComparison::AlreadyCurrent,
        Ordering::Less => VersionComparison::Downgrade,
    }
}

// ── SHA256 verification ────────────────────────────────────────────────────

fn verify_checksum(data: &[u8], expected_hex: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(data);
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
    checked_at: u64,
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

pub async fn run_upgrade(args: UpgradeArgs) -> Result<()> {
    let backend = select_backend();

    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).context("failed to parse current version")?;

    // Determine target version
    let (target, tag) = if let Some(ref v) = args.version {
        let version = parse_version_from_tag(v)?;
        let tag = format!("v{version}");
        (version, tag)
    } else {
        let tag = backend.fetch_latest_release_tag().await?;
        let version = parse_version_from_tag(&tag)?;
        (version, tag)
    };

    // Downgrade protection
    match compare_versions(&current, &target) {
        VersionComparison::Downgrade => {
            if args.version.is_none() {
                bail!(
                    "latest release ({target}) is older than installed version ({current}), skipping"
                );
            }
            // Explicit --version: warn + prompt
            eprintln!("Warning: downgrading from {current} to {target}");
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
        VersionComparison::AlreadyCurrent if !args.force => {
            eprintln!("Already on version {current}");
            return Ok(());
        }
        _ => {}
    }

    if args.dry_run {
        eprintln!("Would upgrade fabro from {current} to {target}");
        eprintln!("  tag: {tag}");
        eprintln!("  target: {}", detect_target()?);
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

    // Download tarball and checksum
    eprintln!("Downloading fabro {target}...");
    let tarball_path = backend
        .download_release(&tag, &tarball_name, tmp_dir.path())
        .await?;
    let checksum_path = backend
        .download_release(&tag, &checksum_name, tmp_dir.path())
        .await?;

    // Verify SHA256
    let tarball_data = fs::read(&tarball_path)?;
    let checksum_content = fs::read_to_string(&checksum_path)?;
    verify_checksum(&tarball_data, &checksum_content)?;
    debug!("SHA256 checksum verified");

    // Extract tarball
    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            &tarball_path.to_string_lossy(),
            "-C",
            &tmp_dir.path().to_string_lossy(),
        ])
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        bail!("tar extraction failed");
    }

    // Find extracted binary
    let extracted_binary = tmp_dir.path().join("fabro");
    if !extracted_binary.exists() {
        bail!("extracted archive does not contain 'fabro' binary");
    }

    // Atomic binary replacement
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

    eprintln!("Upgraded fabro to {target}");
    Ok(())
}

// ── Auto version check ────────────────────────────────────────────────────

pub async fn maybe_print_upgrade_notice(no_upgrade_check: bool) {
    if no_upgrade_check {
        return;
    }

    if let Err(e) = check_and_print_notice().await {
        debug!(%e, "Upgrade check failed (silently swallowed)");
    }
}

async fn check_and_print_notice() -> Result<()> {
    let cli_config = fabro_config::cli::load_cli_config(None)?;
    if !cli_config.upgrade_check {
        return Ok(());
    }

    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let state_path = home.join(".fabro").join(LAST_CHECK_FILE);

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;

    // Check cached state first
    if let Some(state) = UpgradeCheckState::load(&state_path) {
        if !state.is_stale() {
            if let Ok(latest) = Version::parse(&state.latest_version) {
                if latest > current {
                    print_notice(&current, &latest);
                }
            }
            return Ok(());
        }
    }

    // Fetch latest version
    let backend = select_backend();
    let tag = backend.fetch_latest_release_tag().await?;
    let latest = parse_version_from_tag(&tag)?;

    // Save state
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let state = UpgradeCheckState {
        checked_at: now,
        latest_version: latest.to_string(),
    };
    let _ = state.save(&state_path);

    if latest > current {
        print_notice(&current, &latest);
    }

    Ok(())
}

fn print_notice(current: &Version, latest: &Version) {
    eprintln!("A new version of fabro is available: {latest} (current: {current})");
    eprintln!("Run `fabro upgrade` to update.");
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

    // -- Version comparison --

    #[test]
    fn compare_newer() {
        let current = Version::new(0, 4, 0);
        let target = Version::new(0, 5, 0);
        assert_eq!(
            compare_versions(&current, &target),
            VersionComparison::Newer
        );
    }

    #[test]
    fn compare_already_current() {
        let v = Version::new(0, 5, 0);
        assert_eq!(compare_versions(&v, &v), VersionComparison::AlreadyCurrent);
    }

    #[test]
    fn compare_downgrade() {
        let current = Version::new(0, 5, 0);
        let target = Version::new(0, 4, 0);
        assert_eq!(
            compare_versions(&current, &target),
            VersionComparison::Downgrade
        );
    }

    // -- SHA256 verification --

    #[test]
    fn verify_checksum_valid() {
        let data = b"hello world";
        // sha256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_checksum(data, expected).is_ok());
    }

    #[test]
    fn verify_checksum_with_filename_suffix() {
        let data = b"hello world";
        let expected =
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  fabro.tar.gz";
        assert!(verify_checksum(data, expected).is_ok());
    }

    #[test]
    fn verify_checksum_mismatch() {
        let data = b"hello world";
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_checksum(data, wrong).is_err());
    }

    // -- Upgrade check state --

    #[test]
    fn upgrade_check_state_roundtrip() {
        let state = UpgradeCheckState {
            checked_at: 1710000000,
            latest_version: "0.5.0".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: UpgradeCheckState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.checked_at, 1710000000);
        assert_eq!(parsed.latest_version, "0.5.0");
    }

    #[test]
    fn upgrade_check_state_stale() {
        let old = UpgradeCheckState {
            checked_at: 0, // epoch — definitely stale
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
            checked_at: now,
            latest_version: "0.5.0".to_string(),
        };
        assert!(!fresh.is_stale());
    }

    #[test]
    fn upgrade_check_state_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = UpgradeCheckState {
            checked_at: 1710000000,
            latest_version: "0.5.0".to_string(),
        };
        state.save(&path).unwrap();
        let loaded = UpgradeCheckState::load(&path).unwrap();
        assert_eq!(loaded.checked_at, 1710000000);
        assert_eq!(loaded.latest_version, "0.5.0");
    }

    // -- Backend selection --

    #[test]
    fn select_backend_returns_a_variant() {
        // Just ensure it doesn't panic; actual variant depends on environment
        let _backend = select_backend();
    }
}
