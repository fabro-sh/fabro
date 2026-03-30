use fabro_agent::Sandbox;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::fs;
use tracing::{debug, warn};

/// A file discovered by the find command.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub relative_path: String,
    pub size: u64,
    pub mtime_epoch_secs: f64,
}

/// Metadata for a single captured asset file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedAssetInfo {
    pub path: String,
    pub mime: String,
    pub content_md5: String,
    pub content_sha256: String,
    pub bytes: u64,
}

/// Summary of an asset collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetCollectionSummary {
    pub files_copied: usize,
    pub total_bytes: u64,
    pub files_skipped: usize,
    pub download_errors: usize,
    pub hash_errors: usize,
    pub captured_assets: Vec<CapturedAssetInfo>,
}

/// Directories to exclude from the find search and checkpoint commits.
pub const EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".pnpm-store",
    ".npm",
    "target",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    ".cache",
    ".tox",
    ".pytest_cache",
    ".mypy_cache",
    "dist",
];

/// Maximum number of files to collect.
const MAX_FILE_COUNT: usize = 100;

/// Maximum size for a single file (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum total size for all collected files (50 MB).
const MAX_TOTAL_SIZE: u64 = 50 * 1024 * 1024;

/// Build a platform-aware find command to discover asset files matching the given globs.
///
/// Globs without `/` are treated as filename patterns (`-name`).
/// Globs with `/` are treated as directory patterns: the trailing `/**` (if any) is stripped
/// and the remainder is matched via `-path '*/{dir}/*'`.
pub fn build_find_command(root: &str, platform: &str, globs: &[String]) -> String {
    let mut cmd = format!("find {root}");

    // Prune excluded directories
    let prune_parts: Vec<String> = EXCLUDE_DIRS
        .iter()
        .map(|d| format!("-name '{d}'"))
        .collect();
    cmd.push_str(" \\( ");
    cmd.push_str(&prune_parts.join(" -o "));
    cmd.push_str(" \\) -prune -o");

    // Match conditions: not a symlink, is a file, matches user globs
    cmd.push_str(" -not -type l -type f \\(");

    let mut conditions: Vec<String> = Vec::new();
    for glob in globs {
        if glob.contains('/') {
            // Directory-style glob: strip trailing /** and match as path
            let dir = glob.trim_end_matches("/**").trim_end_matches("/*");
            conditions.push(format!(" -path '*/{dir}/*'"));
        } else {
            // Filename glob
            conditions.push(format!(" -name '{glob}'"));
        }
    }
    cmd.push_str(&conditions.join(" -o"));
    cmd.push_str(" \\)");

    // Platform-specific output format
    match platform {
        "darwin" => {
            cmd.push_str(" -exec stat -f '%z %m' {} \\; -print");
        }
        _ => {
            // Linux: use -printf for size, mtime, and relative path
            cmd.push_str(" -printf '%s\\t%T@\\t%P\\n'");
        }
    }

    cmd
}

/// Parse the output of the find command into discovered files.
pub fn parse_find_output(output: &str, platform: &str) -> Vec<DiscoveredFile> {
    match platform {
        "darwin" => parse_find_output_darwin(output),
        _ => parse_find_output_linux(output),
    }
}

fn parse_find_output_linux(output: &str) -> Vec<DiscoveredFile> {
    let mut files = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let Ok(size) = parts[0].parse::<u64>() else {
            continue;
        };
        let Ok(mtime) = parts[1].parse::<f64>() else {
            continue;
        };
        let path = parts[2].to_string();
        if path.is_empty() {
            continue;
        }
        files.push(DiscoveredFile {
            relative_path: path,
            size,
            mtime_epoch_secs: mtime,
        });
    }
    files
}

fn parse_find_output_darwin(output: &str) -> Vec<DiscoveredFile> {
    let mut files = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    // Darwin output comes in pairs: "size mtime" then "path"
    let mut i = 0;
    while i + 1 < lines.len() {
        let stat_line = lines[i].trim();
        let path_line = lines[i + 1].trim();
        i += 2;

        if stat_line.is_empty() || path_line.is_empty() {
            continue;
        }

        let stat_parts: Vec<&str> = stat_line.splitn(2, ' ').collect();
        if stat_parts.len() != 2 {
            continue;
        }

        let Ok(size) = stat_parts[0].parse::<u64>() else {
            continue;
        };
        let Ok(mtime) = stat_parts[1].parse::<f64>() else {
            continue;
        };

        files.push(DiscoveredFile {
            relative_path: path_line.to_string(),
            size,
            mtime_epoch_secs: mtime,
        });
    }
    files
}

/// Select which files should be collected based on timing and size budgets.
pub fn select_files_to_collect(
    discovered: &[DiscoveredFile],
    command_start_epoch: f64,
) -> Vec<DiscoveredFile> {
    let mut candidates: Vec<DiscoveredFile> = discovered
        .iter()
        .filter(|f| {
            // Skip files older than command start
            if f.mtime_epoch_secs < command_start_epoch {
                return false;
            }

            // Skip oversized files
            if f.size > MAX_FILE_SIZE {
                return false;
            }

            true
        })
        .cloned()
        .collect();

    // Sort by size ascending (smallest first)
    candidates.sort_by_key(|f| f.size);

    // Enforce total budget and count limit
    let mut total: u64 = 0;
    let mut selected = Vec::new();
    for f in candidates {
        if selected.len() >= MAX_FILE_COUNT {
            break;
        }
        if total + f.size > MAX_TOTAL_SIZE {
            break;
        }
        total += f.size;
        selected.push(f);
    }

    selected
}

/// Timeout for the find command (30 seconds).
const FIND_TIMEOUT_MS: u64 = 30_000;

/// Normalize discovered file paths to be relative to the working directory.
/// On darwin, find outputs absolute paths; on linux, `-printf '%P'` gives relative paths.
/// This strips the working directory prefix and leading `./` to ensure consistent relative paths.
fn normalize_paths(discovered: Vec<DiscoveredFile>, root: &str) -> Vec<DiscoveredFile> {
    let root_with_slash = if root.ends_with('/') {
        root.to_string()
    } else {
        format!("{root}/")
    };
    discovered
        .into_iter()
        .map(|mut f| {
            if let Some(stripped) = f.relative_path.strip_prefix(&root_with_slash) {
                f.relative_path = stripped.to_string();
            } else if let Some(stripped) = f.relative_path.strip_prefix(root) {
                f.relative_path = stripped.strip_prefix('/').unwrap_or(stripped).to_string();
            }
            if let Some(stripped) = f.relative_path.strip_prefix("./") {
                f.relative_path = stripped.to_string();
            }
            f
        })
        .filter(|f| !f.relative_path.is_empty())
        .collect()
}

fn compute_asset_info(
    relative_path: &str,
    local_path: &Path,
) -> std::result::Result<CapturedAssetInfo, String> {
    let mime = mime_guess::from_path(relative_path)
        .first_or_octet_stream()
        .to_string();
    let data = std::fs::read(local_path)
        .map_err(|e| format!("failed to read {}: {e}", local_path.display()))?;
    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let content_md5 = format!("{:x}", md5::compute(&data));
    let content_sha256 = hex::encode(Sha256::digest(&data));
    Ok(CapturedAssetInfo {
        path: relative_path.to_string(),
        mime,
        content_md5,
        content_sha256,
        bytes,
    })
}

/// Collect asset files matching the configured globs that were created during this stage.
pub async fn collect_assets(
    sandbox: &dyn Sandbox,
    stage_dir: &Path,
    globs: &[String],
    command_start_epoch: f64,
) -> Result<AssetCollectionSummary, String> {
    let root = sandbox.working_directory();
    let platform = sandbox.platform();
    let cmd = build_find_command(root, platform, globs);

    debug!(cmd = cmd.as_str(), "Collecting assets");
    let result = sandbox
        .exec_command(&cmd, FIND_TIMEOUT_MS, None, None, None)
        .await?;

    let discovered = parse_find_output(&result.stdout, platform);
    let discovered = normalize_paths(discovered, root);

    let total_discovered = discovered.len();
    let to_collect = select_files_to_collect(&discovered, command_start_epoch);
    let files_skipped = total_discovered - to_collect.len();

    let mut files_copied: usize = 0;
    let mut total_bytes: u64 = 0;
    let mut download_errors: usize = 0;
    let mut hash_errors: usize = 0;
    let mut captured_assets: Vec<CapturedAssetInfo> = Vec::new();

    for file in &to_collect {
        let dest = stage_dir.join(&file.relative_path);
        match sandbox
            .download_file_to_local(&file.relative_path, &dest)
            .await
        {
            Ok(()) => match compute_asset_info(&file.relative_path, &dest) {
                Ok(info) => {
                    files_copied += 1;
                    total_bytes += info.bytes;
                    captured_assets.push(info);
                }
                Err(e) => {
                    warn!(
                        path = file.relative_path.as_str(),
                        error = e.as_str(),
                        "Asset hash failed"
                    );
                    let _ = std::fs::remove_file(&dest);
                    hash_errors += 1;
                }
            },
            Err(e) => {
                warn!(
                    path = file.relative_path.as_str(),
                    error = e.as_str(),
                    "Asset download failed"
                );
                download_errors += 1;
            }
        }
    }

    // Write manifest.json
    let summary = AssetCollectionSummary {
        files_copied,
        total_bytes,
        files_skipped,
        download_errors,
        hash_errors,
        captured_assets,
    };

    if files_copied > 0 {
        if let Ok(json) = serde_json::to_string_pretty(&summary) {
            let manifest_path = stage_dir.join("manifest.json");
            if let Some(parent) = manifest_path.parent() {
                let _ = fs::create_dir_all(parent).await;
            }
            let _ = fs::write(&manifest_path, json).await;
        }
    }

    Ok(summary)
}

/// Collect all asset paths from manifest files under `{assets_dir}/*/retry_*/manifest.json`.
///
/// Returns the full on-disk paths to the downloaded asset files.
pub fn collect_asset_paths(assets_dir: &Path) -> Vec<String> {
    let Ok(nodes) = std::fs::read_dir(assets_dir) else {
        return Vec::new();
    };

    let mut all_paths = Vec::new();
    for node_entry in nodes.flatten() {
        if !node_entry.path().is_dir() {
            continue;
        }
        let Ok(retries) = std::fs::read_dir(node_entry.path()) else {
            continue;
        };
        for retry_entry in retries.flatten() {
            let manifest = retry_entry.path().join("manifest.json");
            let Ok(contents) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(summary) = serde_json::from_str::<AssetCollectionSummary>(&contents) else {
                continue;
            };
            let retry_dir = retry_entry.path();
            for asset in &summary.captured_assets {
                let full_path = retry_dir.join(&asset.path);
                all_paths.push(full_path.to_string_lossy().into_owned());
            }
        }
    }
    all_paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_agent::sandbox::ExecResult;
    use std::collections::HashMap;

    /// Minimal mock sandbox for asset_snapshot tests.
    struct AssetMockSandbox {
        files: HashMap<String, String>,
        exec_result: ExecResult,
        working_dir: &'static str,
        platform_str: &'static str,
    }

    impl AssetMockSandbox {
        fn new(files: HashMap<String, String>, exec_stdout: &str, platform: &'static str) -> Self {
            Self {
                files,
                exec_result: ExecResult {
                    stdout: exec_stdout.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    duration_ms: 10,
                },
                working_dir: "/home/test",
                platform_str: platform,
            }
        }
    }

    #[async_trait::async_trait]
    impl Sandbox for AssetMockSandbox {
        async fn read_file(
            &self,
            _: &str,
            _: Option<usize>,
            _: Option<usize>,
        ) -> Result<String, String> {
            Err("not implemented".into())
        }
        async fn write_file(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn delete_file(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn file_exists(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _: &str,
            _: Option<usize>,
        ) -> Result<Vec<fabro_agent::sandbox::DirEntry>, String> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            _: &str,
            _: u64,
            _: Option<&str>,
            _: Option<&std::collections::HashMap<String, String>>,
            _: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<ExecResult, String> {
            Ok(self.exec_result.clone())
        }
        async fn grep(
            &self,
            _: &str,
            _: &str,
            _: &fabro_agent::sandbox::GrepOptions,
        ) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn glob(&self, _: &str, _: Option<&str>) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn download_file_to_local(
            &self,
            remote_path: &str,
            local_path: &std::path::Path,
        ) -> Result<(), String> {
            let content = self
                .files
                .get(remote_path)
                .ok_or_else(|| format!("File not found: {remote_path}"))?;
            if let Some(parent) = local_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("Failed to create dirs: {e}"))?;
            }
            tokio::fs::write(local_path, content.as_bytes())
                .await
                .map_err(|e| format!("Failed to write: {e}"))?;
            Ok(())
        }
        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn initialize(&self) -> Result<(), String> {
            Ok(())
        }
        async fn cleanup(&self) -> Result<(), String> {
            Ok(())
        }
        fn working_directory(&self) -> &str {
            self.working_dir
        }
        fn platform(&self) -> &str {
            self.platform_str
        }
        fn os_version(&self) -> String {
            "Linux 6.1.0".into()
        }
    }

    #[test]
    fn parse_find_output_linux() {
        let output = "1024\t1709312400.0\ttest-results/r.xml\n";
        let files = parse_find_output(output, "linux");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "test-results/r.xml");
        assert_eq!(files[0].size, 1024);
        assert!((files[0].mtime_epoch_secs - 1_709_312_400.0).abs() < 0.01);
    }

    #[test]
    fn parse_find_output_darwin() {
        let output = "1024 1709312400\n/tmp/test/test-results/r.xml\n";
        let files = parse_find_output(output, "darwin");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "/tmp/test/test-results/r.xml");
        assert_eq!(files[0].size, 1024);
        assert!((files[0].mtime_epoch_secs - 1_709_312_400.0).abs() < 0.01);
    }

    #[test]
    fn parse_find_output_skips_malformed_lines() {
        let output = "not-a-number\t1709312400.0\tfile.xml\n\
                       1024\t1709312400.0\ttest-results/good.xml\n\
                       incomplete\n";
        let files = parse_find_output(output, "linux");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "test-results/good.xml");
    }

    #[test]
    fn select_files_skips_old_mtime() {
        let discovered = vec![DiscoveredFile {
            relative_path: "test-results/old.xml".to_string(),
            size: 1024,
            mtime_epoch_secs: 500.0,
        }];
        let selected = select_files_to_collect(&discovered, 1000.0);
        assert_eq!(selected.len(), 0);
    }

    #[test]
    fn select_files_skips_oversized() {
        let discovered = vec![DiscoveredFile {
            relative_path: "test-results/huge.xml".to_string(),
            size: MAX_FILE_SIZE + 1,
            mtime_epoch_secs: 2000.0,
        }];
        let selected = select_files_to_collect(&discovered, 1000.0);
        assert_eq!(selected.len(), 0);
    }

    #[test]
    fn select_files_sorts_smallest_first() {
        let discovered = vec![
            DiscoveredFile {
                relative_path: "a.xml".to_string(),
                size: 3000,
                mtime_epoch_secs: 2000.0,
            },
            DiscoveredFile {
                relative_path: "b.xml".to_string(),
                size: 1000,
                mtime_epoch_secs: 2000.0,
            },
            DiscoveredFile {
                relative_path: "c.xml".to_string(),
                size: 2000,
                mtime_epoch_secs: 2000.0,
            },
        ];
        let selected = select_files_to_collect(&discovered, 1000.0);
        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].size, 1000);
        assert_eq!(selected[1].size, 2000);
        assert_eq!(selected[2].size, 3000);
    }

    #[test]
    fn select_files_enforces_total_budget() {
        let discovered: Vec<DiscoveredFile> = (0..6)
            .map(|i| DiscoveredFile {
                relative_path: format!("file{i}.xml"),
                size: 9 * 1024 * 1024, // 9 MB each
                mtime_epoch_secs: 2000.0,
            })
            .collect();
        let selected = select_files_to_collect(&discovered, 1000.0);
        // 50 MB budget / 9 MB each = 5 fit (45 MB), 6th would be 54 MB
        assert_eq!(selected.len(), 5);
    }

    #[test]
    fn build_find_command_filename_glob() {
        let globs = vec!["*.trace.zip".to_string()];
        let cmd = build_find_command("/workspace", "linux", &globs);
        assert!(cmd.contains("-name '*.trace.zip'"));
        assert!(cmd.contains("-printf"));
        assert!(cmd.contains("-prune"));
        assert!(cmd.contains("node_modules"));
    }

    #[test]
    fn build_find_command_directory_glob() {
        let globs = vec!["test-results/**".to_string()];
        let cmd = build_find_command("/workspace", "linux", &globs);
        assert!(cmd.contains("-path '*/test-results/*'"));
    }

    #[test]
    fn build_find_command_mixed_globs() {
        let globs = vec![
            "test-results/**".to_string(),
            "playwright-report/**".to_string(),
            "*.trace.zip".to_string(),
        ];
        let cmd = build_find_command("/workspace", "linux", &globs);
        assert!(cmd.contains("-path '*/test-results/*'"));
        assert!(cmd.contains("-path '*/playwright-report/*'"));
        assert!(cmd.contains("-name '*.trace.zip'"));
    }

    #[test]
    fn build_find_command_darwin() {
        let globs = vec!["test-results/**".to_string()];
        let cmd = build_find_command("/workspace", "darwin", &globs);
        assert!(cmd.contains("-exec stat -f"));
        assert!(!cmd.contains("-printf"));
    }

    #[test]
    fn normalize_paths_strips_root_prefix() {
        let files = vec![
            DiscoveredFile {
                relative_path: "/workspace/test-results/r.xml".to_string(),
                size: 100,
                mtime_epoch_secs: 1000.0,
            },
            DiscoveredFile {
                relative_path: "./test-results/s.xml".to_string(),
                size: 200,
                mtime_epoch_secs: 1000.0,
            },
            DiscoveredFile {
                relative_path: "test-results/t.xml".to_string(),
                size: 300,
                mtime_epoch_secs: 1000.0,
            },
        ];
        let normalized = normalize_paths(files, "/workspace");
        assert_eq!(normalized[0].relative_path, "test-results/r.xml");
        assert_eq!(normalized[1].relative_path, "test-results/s.xml");
        assert_eq!(normalized[2].relative_path, "test-results/t.xml");
    }

    #[tokio::test]
    async fn collect_assets_downloads_and_writes_manifest() {
        let stage_dir = tempfile::tempdir().unwrap();

        let mut files = HashMap::new();
        files.insert("test-results/r.xml".to_string(), "<test/>".to_string());

        let mock = AssetMockSandbox::new(files, "1024\t2000.0\ttest-results/r.xml\n", "linux");

        let globs = vec!["test-results/**".to_string()];
        let summary = collect_assets(&mock, stage_dir.path(), &globs, 1000.0)
            .await
            .unwrap();

        assert_eq!(summary.files_copied, 1);
        assert_eq!(summary.total_bytes, 7);
        assert_eq!(summary.download_errors, 0);
        assert_eq!(summary.hash_errors, 0);
        assert_eq!(summary.captured_assets.len(), 1);
        let asset = &summary.captured_assets[0];
        assert_eq!(asset.path, "test-results/r.xml");
        assert_eq!(asset.mime, "text/xml");
        assert_eq!(asset.bytes, 7);
        assert_eq!(asset.content_md5, "f1430934c390c118ed2f148e1d44d36c");
        assert_eq!(
            asset.content_sha256,
            "28e51ddac37391b99c2b9053f1122d0bf84b02365e6fd8c6e8667378bd00f436"
        );

        // Check that the file was written to the stage dir
        let dest = stage_dir.path().join("test-results/r.xml");
        assert!(dest.exists());
        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, "<test/>");

        // Check manifest
        let manifest = stage_dir.path().join("manifest.json");
        assert!(manifest.exists());
    }

    #[tokio::test]
    async fn collect_assets_skips_old_files() {
        let stage_dir = tempfile::tempdir().unwrap();

        let mut files = HashMap::new();
        files.insert("test-results/r.xml".to_string(), "<test/>".to_string());

        // File mtime (500.0) is before command_start_epoch (1000.0)
        let mock = AssetMockSandbox::new(files, "1024\t500.0\ttest-results/r.xml\n", "linux");

        let globs = vec!["test-results/**".to_string()];
        let summary = collect_assets(&mock, stage_dir.path(), &globs, 1000.0)
            .await
            .unwrap();

        assert_eq!(summary.files_copied, 0);
    }

    #[tokio::test]
    async fn collect_assets_non_fatal_on_download_error() {
        let stage_dir = tempfile::tempdir().unwrap();

        // Don't add the file to the mock files map — download will fail
        let mock = AssetMockSandbox::new(
            HashMap::new(),
            "100\t2000.0\ttest-results/missing.xml\n200\t2000.0\ttest-results/also-missing.xml\n",
            "linux",
        );

        let globs = vec!["test-results/**".to_string()];
        let summary = collect_assets(&mock, stage_dir.path(), &globs, 1000.0)
            .await
            .unwrap();

        assert_eq!(summary.files_copied, 0);
        assert_eq!(summary.download_errors, 2);
        assert_eq!(summary.hash_errors, 0);
    }

    #[test]
    fn collect_asset_paths_from_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let assets_dir = base.join("cache/artifacts/assets");

        // Create two node directories with manifests
        let node_a = assets_dir.join("node_a/retry_1");
        std::fs::create_dir_all(&node_a).unwrap();
        std::fs::write(
            node_a.join("manifest.json"),
            serde_json::to_string(&AssetCollectionSummary {
                files_copied: 2,
                total_bytes: 2048,
                files_skipped: 0,
                download_errors: 0,
                hash_errors: 0,
                captured_assets: vec![
                    CapturedAssetInfo {
                        path: "test-results/report.xml".to_string(),
                        mime: "text/xml".to_string(),
                        content_md5: "md5-report".to_string(),
                        content_sha256: "sha256-report".to_string(),
                        bytes: 1024,
                    },
                    CapturedAssetInfo {
                        path: "test-results/screenshot.png".to_string(),
                        mime: "image/png".to_string(),
                        content_md5: "md5-screenshot".to_string(),
                        content_sha256: "sha256-screenshot".to_string(),
                        bytes: 1024,
                    },
                ],
            })
            .unwrap(),
        )
        .unwrap();

        let node_b = assets_dir.join("node_b/retry_1");
        std::fs::create_dir_all(&node_b).unwrap();
        std::fs::write(
            node_b.join("manifest.json"),
            serde_json::to_string(&AssetCollectionSummary {
                files_copied: 1,
                total_bytes: 512,
                files_skipped: 0,
                download_errors: 0,
                hash_errors: 0,
                captured_assets: vec![CapturedAssetInfo {
                    path: "coverage/lcov.info".to_string(),
                    mime: "application/octet-stream".to_string(),
                    content_md5: "md5-lcov".to_string(),
                    content_sha256: "sha256-lcov".to_string(),
                    bytes: 512,
                }],
            })
            .unwrap(),
        )
        .unwrap();

        let paths = collect_asset_paths(&assets_dir);
        assert_eq!(paths.len(), 3);
        let base_str = base.to_string_lossy();
        assert!(paths.contains(&format!(
            "{base_str}/cache/artifacts/assets/node_a/retry_1/test-results/report.xml"
        )));
        assert!(paths.contains(&format!(
            "{base_str}/cache/artifacts/assets/node_a/retry_1/test-results/screenshot.png"
        )));
        assert!(paths.contains(&format!(
            "{base_str}/cache/artifacts/assets/node_b/retry_1/coverage/lcov.info"
        )));
    }

    #[test]
    fn collect_asset_paths_empty_when_no_assets() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = collect_asset_paths(&tmp.path().join("cache/artifacts/assets"));
        assert!(paths.is_empty());
    }

    #[test]
    fn select_files_enforces_count_limit() {
        // Create 150 small, recent files — should be capped at MAX_FILE_COUNT (100)
        let discovered: Vec<DiscoveredFile> = (0..150)
            .map(|i| DiscoveredFile {
                relative_path: format!("file{i}.txt"),
                size: 100, // tiny files, well within total budget
                mtime_epoch_secs: 2000.0,
            })
            .collect();
        let selected = select_files_to_collect(&discovered, 1000.0);
        assert_eq!(selected.len(), MAX_FILE_COUNT);
    }

    #[test]
    fn build_find_command_excludes_venv() {
        let globs = vec!["*.xml".to_string()];
        let cmd = build_find_command("/workspace", "linux", &globs);
        assert!(cmd.contains(".venv"), "expected .venv in prune clause");
        assert!(cmd.contains("venv"), "expected venv in prune clause");
        assert!(cmd.contains(".cache"), "expected .cache in prune clause");
        assert!(cmd.contains(".tox"), "expected .tox in prune clause");
        assert!(
            cmd.contains(".pytest_cache"),
            "expected .pytest_cache in prune clause"
        );
        assert!(
            cmd.contains(".mypy_cache"),
            "expected .mypy_cache in prune clause"
        );
        assert!(cmd.contains("dist"), "expected dist in prune clause");
    }
}
