use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Eq, PartialEq)]
pub struct BuildGitMetadata {
    pub rerun_paths: Vec<PathBuf>,
    pub short_sha:   String,
}

pub fn collect_from(package_dir: &Path) -> BuildGitMetadata {
    let mut rerun_paths = Vec::new();

    if let Some(head_path) = git_output(package_dir, ["rev-parse", "--git-path", "HEAD"]) {
        rerun_paths.push(PathBuf::from(head_path));
    }

    if let Some(head_ref) = git_output(package_dir, ["symbolic-ref", "-q", "HEAD"]) {
        if let Some(ref_path) = git_output(package_dir, ["rev-parse", "--git-path", &head_ref]) {
            // Cargo treats a missing rerun-if-changed path as always stale, and a
            // packed ref has no loose file. Watch the nearest existing directory
            // instead: the branch's next commit writes its loose ref there.
            let ref_path = PathBuf::from(ref_path);
            let watched = ref_path
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .find(|path| package_dir.join(path).exists())
                .unwrap_or(&ref_path)
                .to_path_buf();
            rerun_paths.push(watched);
        }
    }

    let short_sha = git_output(package_dir, ["rev-list", "-1", "HEAD"])
        .map(|sha| {
            if sha.len() >= 7 {
                sha[..7].to_string()
            } else {
                sha
            }
        })
        .unwrap_or_default();

    BuildGitMetadata {
        rerun_paths,
        short_sha,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Build scripts read Cargo-provided PROFILE outside application runtime configuration."
)]
pub fn cargo_profile() -> String {
    std::env::var("PROFILE").unwrap_or_default()
}

#[expect(
    clippy::disallowed_methods,
    reason = "Build scripts run outside Tokio and need synchronous git probes for embedded build metadata."
)]
fn git_output<const N: usize>(package_dir: &Path, args: [&str; N]) -> Option<String> {
    Command::new("git")
        .current_dir(package_dir)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|output| output.trim().to_string())
            } else {
                None
            }
        })
        .filter(|output| !output.is_empty())
}
