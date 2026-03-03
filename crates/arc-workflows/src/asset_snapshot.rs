use arc_agent::Sandbox;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// Fingerprint of a file for change detection between snapshots.
#[derive(Debug, Clone, PartialEq)]
pub struct FileFingerprint {
    pub size: u64,
    pub mtime_epoch_secs: f64,
}

/// A file discovered by the find command.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub relative_path: String,
    pub size: u64,
    pub mtime_epoch_secs: f64,
}

/// Summary of an asset collection run.
#[derive(Debug, Clone, Serialize)]
pub struct AssetCollectionSummary {
    pub files_copied: usize,
    pub total_bytes: u64,
    pub files_skipped: usize,
    pub download_errors: usize,
    pub copied_paths: Vec<String>,
}

/// Directory path segments that identify asset directories.
const DIRECTORY_SEGMENTS: &[&str] = &[
    "playwright-report",
    "test-results",
    "cypress/videos",
    "cypress/screenshots",
];

/// Filename glob patterns for individual asset files.
const FILENAME_GLOBS: &[&str] = &["junit*.xml", "*.trace.zip"];

/// Directories to exclude from the find search.
const EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".pnpm-store",
    ".npm",
    "target",
    ".next",
    "__pycache__",
];

/// Path segments that indicate excluded tool cache directories.
const EXCLUDE_SEGMENTS: &[&str] = &[
    ".cache/ms-playwright",
    "playwright/.cache",
    ".yarn/cache",
];

/// Maximum size for a single file (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum total size for all collected files (50 MB).
const MAX_TOTAL_SIZE: u64 = 50 * 1024 * 1024;

/// Build a platform-aware find command to discover asset files.
pub fn build_find_command(root: &str, platform: &str) -> String {
    let mut cmd = format!("find {root}");

    // Prune excluded directories
    let prune_parts: Vec<String> = EXCLUDE_DIRS
        .iter()
        .map(|d| format!("-name '{d}'"))
        .collect();
    cmd.push_str(" \\( ");
    cmd.push_str(&prune_parts.join(" -o "));
    cmd.push_str(" \\) -prune -o");

    // Match conditions: not a symlink, is a file, matches asset patterns
    cmd.push_str(" -not -type l -type f \\(");

    let mut path_conditions: Vec<String> = Vec::new();
    for segment in DIRECTORY_SEGMENTS {
        path_conditions.push(format!(" -path '*/{segment}/*'"));
    }
    for glob in FILENAME_GLOBS {
        path_conditions.push(format!(" -name '{glob}'"));
    }
    cmd.push_str(&path_conditions.join(" -o"));
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
        let size = match parts[0].parse::<u64>() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mtime = match parts[1].parse::<f64>() {
            Ok(m) => m,
            Err(_) => continue,
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

        let size = match stat_parts[0].parse::<u64>() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mtime = match stat_parts[1].parse::<f64>() {
            Ok(m) => m,
            Err(_) => continue,
        };

        files.push(DiscoveredFile {
            relative_path: path_line.to_string(),
            size,
            mtime_epoch_secs: mtime,
        });
    }
    files
}

/// Check whether a path matches known asset patterns.
pub fn is_asset_candidate(path: &str) -> bool {
    // Check excluded segments first
    for seg in EXCLUDE_SEGMENTS {
        if path.contains(seg) {
            return false;
        }
    }

    // Check directory segments — must appear as a complete path segment
    for segment in DIRECTORY_SEGMENTS {
        // segment may contain a slash (e.g., "cypress/videos"), so check
        // that it appears bounded by / or start/end of string
        if let Some(pos) = path.find(segment) {
            let before_ok = pos == 0 || path.as_bytes()[pos - 1] == b'/';
            let after_pos = pos + segment.len();
            let after_ok =
                after_pos >= path.len() || path.as_bytes()[after_pos] == b'/';
            if before_ok && after_ok {
                return true;
            }
        }
    }

    // Check filename globs against the last path component
    if let Some(filename) = path.rsplit('/').next() {
        for glob_pattern in FILENAME_GLOBS {
            if matches_simple_glob(glob_pattern, filename) {
                return true;
            }
        }
        // Also check at root level (no slash in path)
        if !path.contains('/') {
            for glob_pattern in FILENAME_GLOBS {
                if matches_simple_glob(glob_pattern, path) {
                    return true;
                }
            }
        }
    }

    false
}

/// Simple glob matching supporting only `*` wildcard.
fn matches_simple_glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    // Check prefix
    if !text.starts_with(parts[0]) {
        return false;
    }
    // Check suffix
    if !text.ends_with(parts[parts.len() - 1]) {
        return false;
    }

    // For patterns like "junit*.xml", verify the middle parts appear in order
    let mut pos = parts[0].len();
    for part in &parts[1..parts.len() - 1] {
        if let Some(found) = text[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }

    true
}

/// Select which files should be collected based on fingerprint changes, timing, and size budgets.
pub fn select_files_to_collect(
    discovered: &[DiscoveredFile],
    baseline: &HashMap<String, FileFingerprint>,
    command_start_epoch: f64,
) -> Vec<DiscoveredFile> {
    let mut candidates: Vec<DiscoveredFile> = discovered
        .iter()
        .filter(|f| {
            // Skip files that haven't changed since baseline
            if let Some(fp) = baseline.get(&f.relative_path) {
                if fp.size == f.size && (fp.mtime_epoch_secs - f.mtime_epoch_secs).abs() < 0.01 {
                    return false;
                }
            }

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

    // Enforce total budget
    let mut total: u64 = 0;
    let mut selected = Vec::new();
    for f in candidates {
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

/// Take a snapshot of current asset files in the sandbox.
/// Returns a fingerprint map of discovered files.
pub async fn snapshot(
    sandbox: &dyn Sandbox,
) -> Result<HashMap<String, FileFingerprint>, String> {
    let root = sandbox.working_directory();
    let platform = sandbox.platform();
    let cmd = build_find_command(root, platform);

    debug!("Taking asset snapshot");
    let result = sandbox
        .exec_command(&cmd, FIND_TIMEOUT_MS, None, None, None)
        .await?;

    // Ignore non-zero exit codes — find may return 1 if some dirs are unreadable
    let discovered = parse_find_output(&result.stdout, platform);
    let discovered = normalize_paths(discovered, root);

    let mut fingerprints = HashMap::new();
    for f in discovered {
        if is_asset_candidate(&f.relative_path) {
            fingerprints.insert(
                f.relative_path,
                FileFingerprint {
                    size: f.size,
                    mtime_epoch_secs: f.mtime_epoch_secs,
                },
            );
        }
    }

    Ok(fingerprints)
}

/// Collect asset files that changed since the baseline snapshot.
pub async fn collect_assets(
    sandbox: &dyn Sandbox,
    stage_dir: &Path,
    baseline: &HashMap<String, FileFingerprint>,
    command_start_epoch: f64,
) -> Result<AssetCollectionSummary, String> {
    let root = sandbox.working_directory();
    let platform = sandbox.platform();
    let cmd = build_find_command(root, platform);

    let result = sandbox
        .exec_command(&cmd, FIND_TIMEOUT_MS, None, None, None)
        .await?;

    let discovered = parse_find_output(&result.stdout, platform);
    let discovered = normalize_paths(discovered, root);
    let candidates: Vec<DiscoveredFile> = discovered
        .into_iter()
        .filter(|f| is_asset_candidate(&f.relative_path))
        .collect();

    let total_discovered = candidates.len();
    let to_collect = select_files_to_collect(&candidates, baseline, command_start_epoch);
    let files_skipped = total_discovered - to_collect.len();

    let mut files_copied: usize = 0;
    let mut total_bytes: u64 = 0;
    let mut download_errors: usize = 0;
    let mut copied_paths: Vec<String> = Vec::new();

    for file in &to_collect {
        let dest = stage_dir.join(&file.relative_path);
        match sandbox
            .download_file_to_local(&file.relative_path, &dest)
            .await
        {
            Ok(()) => {
                files_copied += 1;
                total_bytes += file.size;
                copied_paths.push(file.relative_path.clone());
            }
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
        copied_paths,
    };

    if files_copied > 0 {
        if let Ok(json) = serde_json::to_string_pretty(&summary) {
            let manifest_path = stage_dir.join("manifest.json");
            if let Some(parent) = manifest_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let _ = tokio::fs::write(&manifest_path, json).await;
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_agent::sandbox::ExecResult;

    /// Minimal mock sandbox for asset_snapshot tests.
    struct AssetMockSandbox {
        files: HashMap<String, String>,
        exec_result: ExecResult,
        working_dir: &'static str,
        platform_str: &'static str,
    }

    impl AssetMockSandbox {
        fn new(
            files: HashMap<String, String>,
            exec_stdout: &str,
            platform: &'static str,
        ) -> Self {
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
        async fn read_file(&self, _: &str, _: Option<usize>, _: Option<usize>) -> Result<String, String> {
            Err("not implemented".into())
        }
        async fn write_file(&self, _: &str, _: &str) -> Result<(), String> { Ok(()) }
        async fn delete_file(&self, _: &str) -> Result<(), String> { Ok(()) }
        async fn file_exists(&self, _: &str) -> Result<bool, String> { Ok(false) }
        async fn list_directory(&self, _: &str, _: Option<usize>) -> Result<Vec<arc_agent::sandbox::DirEntry>, String> { Ok(vec![]) }
        async fn exec_command(&self, _: &str, _: u64, _: Option<&str>, _: Option<&std::collections::HashMap<String, String>>, _: Option<tokio_util::sync::CancellationToken>) -> Result<ExecResult, String> {
            Ok(self.exec_result.clone())
        }
        async fn grep(&self, _: &str, _: &str, _: &arc_agent::sandbox::GrepOptions) -> Result<Vec<String>, String> { Ok(vec![]) }
        async fn glob(&self, _: &str, _: Option<&str>) -> Result<Vec<String>, String> { Ok(vec![]) }
        async fn download_file_to_local(&self, remote_path: &str, local_path: &std::path::Path) -> Result<(), String> {
            let content = self.files.get(remote_path)
                .ok_or_else(|| format!("File not found: {remote_path}"))?;
            if let Some(parent) = local_path.parent() {
                tokio::fs::create_dir_all(parent).await
                    .map_err(|e| format!("Failed to create dirs: {e}"))?;
            }
            tokio::fs::write(local_path, content.as_bytes()).await
                .map_err(|e| format!("Failed to write: {e}"))?;
            Ok(())
        }
        async fn initialize(&self) -> Result<(), String> { Ok(()) }
        async fn cleanup(&self) -> Result<(), String> { Ok(()) }
        fn working_directory(&self) -> &str { self.working_dir }
        fn platform(&self) -> &str { self.platform_str }
        fn os_version(&self) -> String { "Linux 6.1.0".into() }
    }

    #[test]
    fn is_asset_candidate_matches_directory_segments() {
        assert!(is_asset_candidate("playwright-report/index.html"));
    }

    #[test]
    fn is_asset_candidate_matches_nested_segments() {
        assert!(is_asset_candidate("frontend/playwright-report/index.html"));
    }

    #[test]
    fn is_asset_candidate_rejects_partial_segments() {
        assert!(!is_asset_candidate("playwright-reporter/index.html"));
    }

    #[test]
    fn is_asset_candidate_matches_filename_globs() {
        assert!(is_asset_candidate("junit-report.xml"));
        assert!(is_asset_candidate("some/dir/junit.xml"));
        assert!(is_asset_candidate("output/results.trace.zip"));
    }

    #[test]
    fn is_asset_candidate_rejects_excluded_paths() {
        assert!(!is_asset_candidate(".cache/ms-playwright/chromium/file.txt"));
        assert!(!is_asset_candidate("playwright/.cache/some-file"));
        assert!(!is_asset_candidate(".yarn/cache/something.zip"));
    }

    #[test]
    fn is_asset_candidate_matches_cypress_directories() {
        assert!(is_asset_candidate("cypress/videos/test.mp4"));
        assert!(is_asset_candidate("cypress/screenshots/fail.png"));
    }

    #[test]
    fn is_asset_candidate_rejects_unrelated_paths() {
        assert!(!is_asset_candidate("src/main.rs"));
        assert!(!is_asset_candidate("package.json"));
        assert!(!is_asset_candidate("report.xml"));
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
    fn select_files_skips_unchanged() {
        let discovered = vec![DiscoveredFile {
            relative_path: "test-results/r.xml".to_string(),
            size: 1024,
            mtime_epoch_secs: 1000.0,
        }];
        let mut baseline = HashMap::new();
        baseline.insert(
            "test-results/r.xml".to_string(),
            FileFingerprint {
                size: 1024,
                mtime_epoch_secs: 1000.0,
            },
        );
        let selected = select_files_to_collect(&discovered, &baseline, 500.0);
        assert_eq!(selected.len(), 0);
    }

    #[test]
    fn select_files_skips_old_mtime() {
        let discovered = vec![DiscoveredFile {
            relative_path: "test-results/old.xml".to_string(),
            size: 1024,
            mtime_epoch_secs: 500.0,
        }];
        let baseline = HashMap::new();
        let selected = select_files_to_collect(&discovered, &baseline, 1000.0);
        assert_eq!(selected.len(), 0);
    }

    #[test]
    fn select_files_skips_oversized() {
        let discovered = vec![DiscoveredFile {
            relative_path: "test-results/huge.xml".to_string(),
            size: MAX_FILE_SIZE + 1,
            mtime_epoch_secs: 2000.0,
        }];
        let baseline = HashMap::new();
        let selected = select_files_to_collect(&discovered, &baseline, 1000.0);
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
        let baseline = HashMap::new();
        let selected = select_files_to_collect(&discovered, &baseline, 1000.0);
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
        let baseline = HashMap::new();
        let selected = select_files_to_collect(&discovered, &baseline, 1000.0);
        // 50 MB budget / 9 MB each = 5 fit (45 MB), 6th would be 54 MB
        assert_eq!(selected.len(), 5);
    }

    #[test]
    fn build_find_command_linux() {
        let cmd = build_find_command("/workspace", "linux");
        assert!(cmd.contains("-printf"));
        assert!(cmd.contains("playwright-report"));
        assert!(cmd.contains("junit*.xml"));
        assert!(cmd.contains("-prune"));
        assert!(cmd.contains("node_modules"));
    }

    #[test]
    fn build_find_command_darwin() {
        let cmd = build_find_command("/workspace", "darwin");
        assert!(cmd.contains("-exec stat -f"));
        assert!(cmd.contains("playwright-report"));
        assert!(cmd.contains("junit*.xml"));
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
    async fn snapshot_uses_exec_command_and_parses() {
        let mock = AssetMockSandbox::new(
            HashMap::new(),
            "1024\t2000.0\ttest-results/r.xml\n512\t2000.0\tsrc/main.rs\n",
            "linux",
        );

        let fingerprints = snapshot(&mock).await.unwrap();
        // Only test-results/r.xml is an asset candidate, src/main.rs is not
        assert_eq!(fingerprints.len(), 1);
        assert!(fingerprints.contains_key("test-results/r.xml"));
        assert_eq!(fingerprints["test-results/r.xml"].size, 1024);
    }

    #[tokio::test]
    async fn collect_assets_downloads_and_writes_manifest() {
        let stage_dir = tempfile::tempdir().unwrap();

        let mut files = HashMap::new();
        files.insert("test-results/r.xml".to_string(), "<test/>".to_string());

        let mock = AssetMockSandbox::new(
            files,
            "1024\t2000.0\ttest-results/r.xml\n",
            "linux",
        );

        let baseline = HashMap::new();
        let summary = collect_assets(&mock, stage_dir.path(), &baseline, 1000.0)
            .await
            .unwrap();

        assert_eq!(summary.files_copied, 1);
        assert_eq!(summary.total_bytes, 1024);
        assert_eq!(summary.download_errors, 0);
        assert_eq!(summary.copied_paths, vec!["test-results/r.xml"]);

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
    async fn collect_assets_skips_unchanged_files() {
        let stage_dir = tempfile::tempdir().unwrap();

        let mut files = HashMap::new();
        files.insert("test-results/r.xml".to_string(), "<test/>".to_string());

        let mock = AssetMockSandbox::new(
            files,
            "1024\t2000.0\ttest-results/r.xml\n",
            "linux",
        );

        // Provide a baseline with the same fingerprint
        let mut baseline = HashMap::new();
        baseline.insert(
            "test-results/r.xml".to_string(),
            FileFingerprint {
                size: 1024,
                mtime_epoch_secs: 2000.0,
            },
        );

        let summary = collect_assets(&mock, stage_dir.path(), &baseline, 1000.0)
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

        let baseline = HashMap::new();
        let summary = collect_assets(&mock, stage_dir.path(), &baseline, 1000.0)
            .await
            .unwrap();

        assert_eq!(summary.files_copied, 0);
        assert_eq!(summary.download_errors, 2);
    }
}
