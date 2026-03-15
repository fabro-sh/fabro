use std::path::{Path, PathBuf};

/// Expand `~/` prefix to the user's home directory.
pub fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_with_home_prefix() {
        let result = expand_tilde(Path::new("~/foo/bar"));
        assert!(result != Path::new("~/foo/bar"));
        assert!(result.ends_with("foo/bar"));
    }

    #[test]
    fn expand_tilde_without_prefix() {
        assert_eq!(expand_tilde(Path::new("/abs/path")), Path::new("/abs/path"));
    }
}
