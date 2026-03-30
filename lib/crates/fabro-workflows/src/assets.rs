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
pub fn scan_assets(
    assets_dir: &Path,
    node_filter: Option<&str>,
    retry_filter: Option<u32>,
) -> Result<Vec<AssetEntry>> {
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

            if let Some(filter) = retry_filter {
                if retry != filter {
                    continue;
                }
            }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_snapshot::{AssetCollectionSummary, CapturedAssetInfo};

    #[test]
    fn scan_assets_filters_by_node_and_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let assets_dir = tmp.path().join("cache/artifacts/assets");

        let retry_1 = assets_dir.join("work/retry_1");
        std::fs::create_dir_all(&retry_1).unwrap();
        std::fs::write(
            retry_1.join("manifest.json"),
            serde_json::to_string(&AssetCollectionSummary {
                files_copied: 1,
                total_bytes: 5,
                files_skipped: 0,
                download_errors: 0,
                hash_errors: 0,
                captured_assets: vec![CapturedAssetInfo {
                    path: "report.txt".to_string(),
                    mime: "text/plain".to_string(),
                    content_md5: "a".repeat(32),
                    content_sha256: "b".repeat(64),
                    bytes: 5,
                }],
            })
            .unwrap(),
        )
        .unwrap();

        let retry_2 = assets_dir.join("work/retry_2");
        std::fs::create_dir_all(&retry_2).unwrap();
        std::fs::write(
            retry_2.join("manifest.json"),
            serde_json::to_string(&AssetCollectionSummary {
                files_copied: 1,
                total_bytes: 6,
                files_skipped: 0,
                download_errors: 0,
                hash_errors: 0,
                captured_assets: vec![CapturedAssetInfo {
                    path: "report.txt".to_string(),
                    mime: "text/plain".to_string(),
                    content_md5: "c".repeat(32),
                    content_sha256: "d".repeat(64),
                    bytes: 6,
                }],
            })
            .unwrap(),
        )
        .unwrap();

        let entries = scan_assets(&assets_dir, Some("work"), Some(2)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].retry, 2);
        assert_eq!(entries[0].relative_path, "report.txt");
        assert_eq!(entries[0].size, 6);
    }
}
