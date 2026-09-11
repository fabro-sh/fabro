use std::path::Path;

use anyhow::{Result, bail};
use fabro_sandbox::daytona::detect_repo_info;
use git2::Repository;

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

    if !origin_matches_expected(&origin_url, expected_origin_url, &cwd) {
        let current_origin_url = fabro_github::normalize_repo_origin_url(&origin_url);
        bail!(
            "Current repository origin '{current_origin_url}' does not match run repository '{expected_origin_url}'; refusing to {action} this run from the wrong checkout"
        );
    }

    Ok(())
}

/// Whether the raw local `origin` URL denotes the same repository as
/// `expected`, honoring `url.<replacement>.insteadOf` config rewrites.
///
/// A checkout can store its origin in rewritten form (for example an SSH host
/// alias used for account separation) while the run spec stores the canonical
/// URL, or the other way around. Both rewrite directions are therefore
/// compared, after normalization, before the guard rejects the operation.
fn origin_matches_expected(raw_origin: &str, expected: &str, repo_path: &Path) -> bool {
    let expected_normalized = fabro_github::normalize_repo_origin_url(expected);
    rewrite_candidates(
        raw_origin,
        &insteadof_rewrites(repo_path).unwrap_or_default(),
    )
    .iter()
    .any(|candidate| fabro_github::normalize_repo_origin_url(candidate) == expected_normalized)
}

/// Every URL the raw origin can denote: itself, git's forward `insteadOf`
/// rewrite, and the inverse rewrite that recovers the pre-rewrite form for
/// origins stored in rewritten form.
fn rewrite_candidates(raw_origin: &str, rewrites: &[(String, String)]) -> Vec<String> {
    let mut candidates = vec![raw_origin.to_string()];
    for (replacement, matcher) in rewrites {
        if let Some(rest) = raw_origin.strip_prefix(matcher.as_str()) {
            candidates.push(format!("{replacement}{rest}"));
        }
        if let Some(rest) = raw_origin.strip_prefix(replacement.as_str()) {
            candidates.push(format!("{matcher}{rest}"));
        }
    }
    candidates
}

/// `url.<replacement>.insteadOf` pairs visible from `repo_path`, following
/// git's config cascade (repo-local, global, system). Git allows multiple
/// `insteadOf` values per replacement key; each becomes its own pair.
fn insteadof_rewrites(repo_path: &Path) -> Option<Vec<(String, String)>> {
    let repo = Repository::discover(repo_path).ok()?;
    let config = repo.config().ok()?;
    let mut entries = config.entries(Some("url.*.insteadof")).ok()?;
    let mut pairs = Vec::new();
    while let Some(entry) = entries.next() {
        let entry = entry.ok()?;
        let Some(name) = entry.name() else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        let Some(replacement) = name
            .strip_prefix("url.")
            .and_then(|rest| rest.strip_suffix(".insteadof"))
        else {
            continue;
        };
        if let Some(value) = entry.value() {
            pairs.push((replacement.to_string(), value.to_string()));
        }
    }
    Some(pairs)
}

#[cfg(test)]
mod tests {
    use super::{ensure_matching_repo_origin, rewrite_candidates};

    #[test]
    fn missing_expected_origin_skips_guard() {
        ensure_matching_repo_origin(None, "fork").unwrap();
    }

    #[test]
    fn candidates_keep_the_raw_url() {
        let candidates = rewrite_candidates("https://example.com/owner/repo.git", &[]);
        assert_eq!(candidates, vec!["https://example.com/owner/repo.git"]);
    }

    #[test]
    fn candidates_recover_canonical_form_from_alias_origin() {
        // Origin stored in rewritten form: the inverse rewrite recovers the
        // canonical URL the run spec stores.
        let rewrites = vec![(
            "git@denkhaus.github.com:denkhaus/".to_string(),
            "https://github.com/denkhaus/".to_string(),
        )];
        let candidates =
            rewrite_candidates("git@denkhaus.github.com:denkhaus/fabro.git", &rewrites);
        assert!(candidates.contains(&"https://github.com/denkhaus/fabro.git".to_string()));
    }

    #[test]
    fn candidates_apply_forward_rewrite_to_canonical_origin() {
        // Origin stored canonically: git's forward rewrite yields the alias
        // form actually used for fetches and pushes.
        let rewrites = vec![(
            "git@denkhaus.github.com:denkhaus/".to_string(),
            "git@github.com:denkhaus/".to_string(),
        )];
        let candidates = rewrite_candidates("git@github.com:denkhaus/fabro.git", &rewrites);
        assert!(candidates.contains(&"git@denkhaus.github.com:denkhaus/fabro.git".to_string()));
    }

    #[test]
    fn insteadof_rewrites_reads_git_config_cascade() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");
        let mut config = repo.config().expect("repo config");
        config
            .set_str(
                "url.git@denkhaus.github.com:denkhaus/.insteadOf",
                "https://github.com/denkhaus/",
            )
            .expect("set insteadOf");
        config
            .set_multivar(
                "url.git@denkhaus.github.com:denkhaus/.insteadOf",
                "^$",
                "git@github.com:denkhaus/",
            )
            .expect("set second insteadOf value");

        let pairs = super::insteadof_rewrites(dir.path())
            .expect("rewrites should be readable from the repo config");
        assert!(pairs.contains(&(
            "git@denkhaus.github.com:denkhaus/".to_string(),
            "https://github.com/denkhaus/".to_string(),
        )));
        assert!(pairs.contains(&(
            "git@denkhaus.github.com:denkhaus/".to_string(),
            "git@github.com:denkhaus/".to_string(),
        )));
    }

    #[test]
    fn candidates_use_every_insteadof_value_of_one_replacement() {
        // Git allows several insteadOf values per replacement; each match
        // pattern must produce its own inverse candidate.
        let rewrites = vec![
            (
                "git@denkhaus.github.com:denkhaus/".to_string(),
                "https://github.com/denkhaus/".to_string(),
            ),
            (
                "git@denkhaus.github.com:denkhaus/".to_string(),
                "git@github.com:denkhaus/".to_string(),
            ),
        ];
        let candidates =
            rewrite_candidates("git@denkhaus.github.com:denkhaus/fabro.git", &rewrites);
        assert!(candidates.contains(&"https://github.com/denkhaus/fabro.git".to_string()));
        assert!(candidates.contains(&"git@github.com:denkhaus/fabro.git".to_string()));
    }
}
