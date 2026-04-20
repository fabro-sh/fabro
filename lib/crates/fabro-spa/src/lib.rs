use std::borrow::Cow;

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
#[exclude = "*.map"]
#[exclude = "**/*.map"]
struct EmbeddedAssets;

pub struct AssetBytes(Cow<'static, [u8]>);

impl AssetBytes {
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0.into_owned()
    }
}

impl AsRef<[u8]> for AssetBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[must_use]
pub fn get(path: &str) -> Option<AssetBytes> {
    EmbeddedAssets::get(path).map(|file| AssetBytes(file.data))
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
    fn embeds_index_html() {
        let index = get("index.html").expect("expected embedded index.html");
        let html = std::str::from_utf8(index.as_ref()).expect("index.html should be valid UTF-8");
        assert!(html.contains("<div id=\"root\"></div>"));
    }

    #[test]
    fn committed_assets_do_not_include_source_maps() {
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
