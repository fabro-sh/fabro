use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::asset_snapshot::AssetCollectionSummary;

/// An individual asset file discovered from a run's asset manifests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssetEntry {
    pub node_slug: String,
    pub retry: u32,
    pub relative_path: String,
    #[serde(serialize_with = "serialize_path")]
    pub absolute_path: PathBuf,
    pub size: u64,
}

fn serialize_path<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.display().to_string())
}

/// Walk `{assets_dir}/*/retry_*/manifest.json`, stat each file, and return entries.
pub fn scan_assets(assets_dir: &Path, node_filter: Option<&str>) -> Result<Vec<AssetEntry>> {
    let Ok(nodes) = std::fs::read_dir(assets_dir) else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    for node_entry in nodes.flatten() {
        if !node_entry.path().is_dir() {
            continue;
        }
        let node_slug = node_entry.file_name().to_string_lossy().into_owned();

        if let Some(filter) = node_filter {
            if node_slug != filter {
                continue;
            }
        }

        let Ok(retries) = std::fs::read_dir(node_entry.path()) else {
            continue;
        };
        for retry_entry in retries.flatten() {
            let retry_dir = retry_entry.path();
            let dir_name = retry_entry.file_name().to_string_lossy().into_owned();
            let retry: u32 = dir_name
                .strip_prefix("retry_")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);

            let manifest = retry_dir.join("manifest.json");
            let Ok(contents) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(summary) = serde_json::from_str::<AssetCollectionSummary>(&contents) else {
                continue;
            };

            for asset in &summary.captured_assets {
                let absolute_path = retry_dir.join(&asset.path);
                entries.push(AssetEntry {
                    node_slug: node_slug.clone(),
                    retry,
                    relative_path: asset.path.clone(),
                    absolute_path,
                    size: asset.bytes,
                });
            }
        }
    }

    Ok(entries)
}
