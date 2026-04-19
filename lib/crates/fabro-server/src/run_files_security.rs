#![allow(unreachable_pub, dead_code)]

//! Security helpers shared by the Run Files Changed endpoint: a globset-based
//! sensitive-path denylist, a sandbox-git env-hardening helper, and a
//! structured metrics emitter that enforces the tracing allowlist.
//!
//! All matching is path-based and case-insensitive. The denylist is a
//! defense-in-depth control — it is not a content scanner and will not
//! catch arbitrary secrets hidden inside non-secret file extensions.
//!
//! `sandbox_git_env` and `RunFilesMetrics` are intentionally public APIs
//! even though they're currently consumed by a single caller — the module
//! is designed as a reusable surface for any future sensitive-data-adjacent
//! endpoint.

use std::collections::HashMap;
use std::sync::OnceLock;

use fabro_types::RunId;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tracing::info;

/// Basename patterns — applied to the final path segment only.
const BASENAME_GLOBS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "id_rsa",
    "id_rsa.*",
    "id_ed25519",
    "id_ed25519.*",
    "*.p12",
    "*.keystore",
    "*.key",
];

/// Path-suffix patterns — applied to the whole repo-relative path.
const PATH_SUFFIX_GLOBS: &[&str] = &[
    "**/.aws/credentials",
    ".aws/credentials",
    "**/.git/config",
    ".git/config",
    "**/.ssh/**",
    ".ssh/**",
];

/// Lazily-constructed globsets. Building from a static pattern list never
/// fails in practice, but we defensively unwrap into an always-false matcher
/// so a pattern typo in a future edit doesn't take the whole endpoint down.
struct Denylist {
    basename: GlobSet,
    path:     GlobSet,
}

fn build_set(patterns: &[&str]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(glob) = Glob::new(pat) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

fn denylist() -> &'static Denylist {
    static SET: OnceLock<Denylist> = OnceLock::new();
    SET.get_or_init(|| Denylist {
        basename: build_set(BASENAME_GLOBS),
        path:     build_set(PATH_SUFFIX_GLOBS),
    })
}

/// Return `true` if `path` matches any entry in the sensitive-path denylist.
///
/// Matching semantics:
/// - Inputs are normalized to lowercase and POSIX separators before matching.
/// - Ancestor `../` / `./` components are stripped so attempts to sneak a
///   sensitive file via traversal can't evade the check.
/// - Basename globs fire against the last path segment only (prevents
///   `log/.env_audit/data.txt` from matching the `.env.*` pattern).
/// - Path-suffix globs fire against the whole normalized path.
/// - Empty paths, pure `.` paths, and paths that normalize to empty are
///   considered sensitive as a safe default — no legitimate diff produces
///   these; treating them as sensitive prevents accidental exposure of
///   pathological git output.
#[must_use]
pub fn is_sensitive(path: &str) -> bool {
    let normalized = normalize_for_match(path);
    if normalized.is_empty() {
        return true;
    }
    let set = denylist();

    let basename = normalized
        .rsplit_once('/')
        .map_or(normalized.as_str(), |(_, n)| n);
    if set.basename.is_match(basename) {
        return true;
    }
    set.path.is_match(normalized.as_str())
}

fn normalize_for_match(path: &str) -> String {
    // Replace backslashes with forward slashes so Windows-style paths (if
    // they ever leak through git output) match the same way; lowercase so
    // the patterns are effectively case-insensitive.
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        let c = if ch == '\\' { '/' } else { ch };
        out.extend(c.to_lowercase());
    }
    // Drop leading `./` and consecutive `../` prefixes; keep inner `..` alone
    // since git doesn't emit those in normal diffs.
    while let Some(rest) = out
        .strip_prefix("./")
        .or_else(|| out.strip_prefix("../"))
        .or_else(|| out.strip_prefix("/"))
    {
        out = rest.to_string();
    }
    out
}

/// Environment additions applied to every sandbox-side git invocation under
/// the Run Files endpoint. Pairs with the hardened `-c` flags the sandbox
/// git helpers already use.
#[must_use]
pub fn sandbox_git_env() -> HashMap<String, String> {
    HashMap::from([
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_EXTERNAL_DIFF".to_string(), String::new()),
    ])
}

/// Metrics emitted at the tail of every Run Files response. The field set is
/// deliberately the only shape of tracing output the endpoint produces —
/// enforced by `emit`, which never interpolates or logs individual paths,
/// file contents, or raw git stderr.
pub struct RunFilesMetrics {
    pub file_count:      usize,
    pub bytes_total:     u64,
    pub duration_ms:     u64,
    pub truncated:       bool,
    pub binary_count:    u64,
    pub sensitive_count: u64,
    pub symlink_count:   u64,
    pub submodule_count: u64,
}

impl RunFilesMetrics {
    pub fn emit(&self, run_id: &RunId) {
        info!(
            run_id = %run_id,
            file_count = self.file_count,
            bytes_total = self.bytes_total,
            duration_ms = self.duration_ms,
            truncated = self.truncated,
            binary_count = self.binary_count,
            sensitive_count = self.sensitive_count,
            symlink_count = self.symlink_count,
            submodule_count = self.submodule_count,
            "Run files response produced"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sensitive_matches_basename_patterns() {
        assert!(is_sensitive(".env"));
        assert!(is_sensitive("apps/web/.env.production"));
        assert!(is_sensitive("keys/id_rsa"));
        assert!(is_sensitive("keys/id_rsa.pub"));
        assert!(is_sensitive("keys/id_ed25519"));
        assert!(is_sensitive("certs/Server.PEM"));
        assert!(is_sensitive("data/vault.keystore"));
        assert!(is_sensitive("config/service.key"));
    }

    #[test]
    fn is_sensitive_matches_path_suffix_patterns() {
        assert!(is_sensitive(".ssh/authorized_keys"));
        assert!(is_sensitive("home/user/.ssh/id_custom"));
        assert!(is_sensitive(".aws/credentials"));
        assert!(is_sensitive("home/user/.aws/credentials"));
        assert!(is_sensitive(".git/config"));
    }

    #[test]
    fn is_sensitive_rejects_benign_paths() {
        assert!(!is_sensitive("src/main.rs"));
        assert!(!is_sensitive("README.md"));
        // A file whose name merely contains `env` as a substring must not
        // match the `.env` / `.env.*` basename globs.
        assert!(!is_sensitive("src/environment.ts"));
        // Ancestor-directory-only match: `.env_audit` is a directory, the
        // actual file is `data.txt` — shouldn't hit the basename pattern.
        assert!(!is_sensitive("log/.env_audit/data.txt"));
    }

    #[test]
    fn is_sensitive_handles_path_traversal_safely() {
        // `../` components strip to an otherwise-benign path rather than
        // letting the attacker evade matching. The ultimate segment drives
        // the basename check.
        assert!(is_sensitive("../.ssh/id_rsa"));
        assert!(is_sensitive("./.env"));
        assert!(!is_sensitive("../../src/main.rs"));
    }

    #[test]
    fn is_sensitive_empty_paths_fail_closed() {
        assert!(is_sensitive(""));
        assert!(is_sensitive("./"));
        assert!(is_sensitive("/"));
    }

    #[test]
    fn is_sensitive_is_case_insensitive() {
        assert!(is_sensitive(".ENV"));
        assert!(is_sensitive(".Env.Production"));
        assert!(is_sensitive("keys/ID_RSA"));
    }

    #[test]
    fn sandbox_git_env_sets_expected_pairs() {
        let env = sandbox_git_env();
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        assert_eq!(env.get("GIT_EXTERNAL_DIFF").map(String::as_str), Some(""));
    }
}
