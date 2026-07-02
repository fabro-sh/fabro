use std::borrow::Cow;

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
#[exclude = "*.map"]
#[exclude = "**/*.map"]
struct EmbeddedAssets;

pub struct AssetBytes {
    data:   Cow<'static, [u8]>,
    sha256: [u8; 32],
}

impl AssetBytes {
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.data.into_owned()
    }

    #[must_use]
    pub fn into_cow(self) -> Cow<'static, [u8]> {
        self.data
    }

    /// SHA-256 of the asset bytes. rust-embed computes it at compile time in
    /// release builds, so callers can use it as a validator without rehashing
    /// the body per request.
    #[must_use]
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
}

impl AsRef<[u8]> for AssetBytes {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
    }
}

#[must_use]
pub fn get(path: &str) -> Option<AssetBytes> {
    EmbeddedAssets::get(path).map(|file| AssetBytes {
        sha256: file.metadata.sha256_hash(),
        data:   file.data,
    })
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "test walks the assets/ directory with sync std::fs::read_dir to enforce a build invariant"
)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::get;

    #[test]
    #[ignore = "requires cargo dev spa refresh"]
    fn embeds_index_html() {
        let index = get("index.html").expect("expected embedded index.html");
        let html = std::str::from_utf8(index.as_ref()).expect("index.html should be valid UTF-8");
        assert!(html.contains("<div id=\"root\"></div>"));
    }

    #[test]
    fn embedded_assets_do_not_include_source_maps() {
        let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
        assert!(
            collect_source_maps(&assets_dir).is_empty(),
            "expected no source maps under {}",
            assets_dir.display()
        );
    }

    fn collect_source_maps(root: &Path) -> Vec<PathBuf> {
        let entries = std::fs::read_dir(root)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()));

        let mut source_maps = Vec::new();
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| {
                    panic!("failed to read entry in {}: {error}", root.display())
                })
                .path();
            if path.is_dir() {
                source_maps.extend(collect_source_maps(&path));
            } else if path.extension().is_some_and(|extension| extension == "map") {
                source_maps.push(path);
            }
        }

        source_maps
    }
}
