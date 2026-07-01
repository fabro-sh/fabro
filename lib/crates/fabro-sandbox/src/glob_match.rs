use std::path::Path;

pub(crate) struct GlobMatcher {
    pattern: glob::Pattern,
}

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive:              true,
    require_literal_separator:   true,
    require_literal_leading_dot: false,
};

impl GlobMatcher {
    pub(crate) fn new(base: &str, pattern: &str) -> crate::Result<Self> {
        let full_pattern = full_pattern(base, pattern);
        let pattern = glob::Pattern::new(&full_pattern)
            .map_err(|err| crate::Error::context("Invalid glob pattern", err))?;
        Ok(Self { pattern })
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        self.pattern.matches_with(path, MATCH_OPTIONS)
    }
}

pub(crate) fn traversal_root(base: &str, pattern: &str) -> String {
    let full_pattern = full_pattern(base, pattern);
    literal_traversal_root(&full_pattern)
}

pub(crate) fn join_path(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_string();
    }
    if is_absolute(path) {
        return path.to_string();
    }
    if base.is_empty() {
        return path.to_string();
    }
    if base == "/" {
        return format!("/{path}");
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

fn full_pattern(base: &str, pattern: &str) -> String {
    if is_absolute(pattern) {
        pattern.to_string()
    } else {
        join_path(base, pattern)
    }
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || Path::new(path).is_absolute()
}

fn literal_traversal_root(pattern: &str) -> String {
    let absolute = pattern.starts_with('/');
    let mut literal_segments = Vec::new();
    let mut saw_meta = false;

    for segment in pattern.split('/').filter(|segment| !segment.is_empty()) {
        if has_glob_meta(segment) {
            saw_meta = true;
            break;
        }
        literal_segments.push(segment);
    }

    if saw_meta {
        return build_path(absolute, &literal_segments);
    }

    parent_path(&build_path(absolute, &literal_segments))
}

fn has_glob_meta(segment: &str) -> bool {
    segment
        .chars()
        .any(|character| matches!(character, '*' | '?' | '['))
}

fn build_path(absolute: bool, segments: &[&str]) -> String {
    if segments.is_empty() {
        return if absolute {
            "/".to_string()
        } else {
            ".".to_string()
        };
    }

    let joined = segments.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

fn parent_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }

    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return ".".to_string();
    }

    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "glob matcher tests stage filesystem fixtures with sync std::fs writes"
)]
mod tests {
    use std::path::Path;

    use super::GlobMatcher;

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn match_glob(
        base: &str,
        pattern: &str,
        candidate_paths: &[String],
    ) -> crate::Result<Vec<String>> {
        let matcher = GlobMatcher::new(base, pattern)?;
        Ok(candidate_paths
            .iter()
            .filter(|path| matcher.matches(path))
            .cloned()
            .collect())
    }

    #[test]
    fn skill_pattern_matches_exactly_one_directory_level() {
        let candidates = vec![
            "/workspace/SKILL.md".to_string(),
            "/workspace/a/SKILL.md".to_string(),
            "/workspace/a/b/SKILL.md".to_string(),
            "/workspace/a/README.md".to_string(),
        ];

        let results = match_glob("/workspace", "*/SKILL.md", &candidates).unwrap();

        assert_eq!(results, vec!["/workspace/a/SKILL.md"]);
    }

    #[test]
    fn star_matches_only_top_level_files_under_base() {
        let candidates = vec![
            "/workspace/a.rs".to_string(),
            "/workspace/src/lib.rs".to_string(),
            "/workspace/b.txt".to_string(),
        ];

        let results = match_glob("/workspace", "*.rs", &candidates).unwrap();

        assert_eq!(results, vec!["/workspace/a.rs"]);
    }

    #[test]
    fn recursive_glob_matches_files_at_any_depth() {
        let candidates = vec![
            "/workspace/a.rs".to_string(),
            "/workspace/src/lib.rs".to_string(),
            "/workspace/src/nested/main.rs".to_string(),
            "/workspace/src/nested/readme.md".to_string(),
        ];

        let results = match_glob("/workspace", "**/*.rs", &candidates).unwrap();

        assert_eq!(results, vec![
            "/workspace/a.rs",
            "/workspace/src/lib.rs",
            "/workspace/src/nested/main.rs",
        ]);
    }

    #[test]
    fn matcher_matches_glob_crate_on_fixture_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        let nested = src.join("nested");
        let skills = root.join("skills");
        let skill_a = skills.join("a");
        let skill_b = skill_a.join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&skill_b).unwrap();
        std::fs::write(root.join("a.rs"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(src.join("lib.rs"), "").unwrap();
        std::fs::write(nested.join("main.rs"), "").unwrap();
        std::fs::write(skills.join("SKILL.md"), "").unwrap();
        std::fs::write(skill_a.join("SKILL.md"), "").unwrap();
        std::fs::write(skill_b.join("SKILL.md"), "").unwrap();

        let candidates = [
            root.join("a.rs"),
            root.join("b.txt"),
            src.join("lib.rs"),
            nested.join("main.rs"),
            skills.join("SKILL.md"),
            skill_a.join("SKILL.md"),
            skill_b.join("SKILL.md"),
        ]
        .into_iter()
        .map(|path| path_string(&path))
        .collect::<Vec<_>>();

        for (base, pattern) in [
            (root, "*.rs"),
            (root, "**/*.rs"),
            (root, "src/*.rs"),
            (skills.as_path(), "*/SKILL.md"),
        ] {
            let full_pattern = format!("{}/{pattern}", base.display());
            let mut expected = glob::glob(&full_pattern)
                .unwrap()
                .filter_map(Result::ok)
                .map(|path| path_string(&path))
                .collect::<Vec<_>>();
            expected.sort();

            let mut actual = match_glob(&path_string(base), pattern, &candidates).unwrap();
            actual.sort();

            assert_eq!(actual, expected, "pattern {full_pattern}");
        }
    }
}
