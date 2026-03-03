use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use tracing::info;

use crate::types::FeatureMetadata;
use crate::DevcontainerError;

/// A resolved feature layer ready to be inserted into a Dockerfile.
#[derive(Debug, Clone)]
pub struct FeatureLayer {
    /// Feature identifier (e.g. "ghcr.io/devcontainers/features/node:1")
    pub id: String,
    /// Directory name for COPY
    pub dir_name: String,
    /// Dockerfile snippet for this feature
    pub dockerfile_snippet: String,
}

/// All resolved feature data: layers, environment, and lifecycle hooks.
#[derive(Debug, Clone, Default)]
pub struct ResolvedFeatures {
    pub layers: Vec<FeatureLayer>,
    pub container_env: HashMap<String, String>,
}

/// Extract the directory name from a feature ID.
/// e.g. "ghcr.io/devcontainers/features/node:1" -> "node"
fn dir_name_from_id(feature_id: &str) -> String {
    let without_tag = feature_id.split(':').next().unwrap_or(feature_id);
    without_tag
        .rsplit('/')
        .next()
        .unwrap_or(without_tag)
        .to_string()
}

/// Ensure `oras` CLI is available, installing it if necessary.
async fn ensure_oras() -> crate::Result<()> {
    let check = tokio::process::Command::new("which")
        .arg("oras")
        .output()
        .await
        .map_err(|e| DevcontainerError::OrasInstall(format!("failed to check for oras: {e}")))?;

    if check.status.success() {
        return Ok(());
    }

    info!("oras not found, attempting to install");

    if cfg!(target_os = "macos") {
        let status = tokio::process::Command::new("brew")
            .args(["install", "oras"])
            .status()
            .await
            .map_err(|e| {
                DevcontainerError::OrasInstall(format!("failed to run brew install oras: {e}"))
            })?;

        if !status.success() {
            return Err(DevcontainerError::OrasInstall(
                "brew install oras failed".to_string(),
            ));
        }
    } else {
        // Linux: download from GitHub releases to ~/.local/bin/
        let home = std::env::var("HOME")
            .map_err(|_| DevcontainerError::OrasInstall("HOME not set".to_string()))?;
        let bin_dir = format!("{home}/.local/bin");

        tokio::fs::create_dir_all(&bin_dir).await.map_err(|e| {
            DevcontainerError::OrasInstall(format!("failed to create {bin_dir}: {e}"))
        })?;

        let version = "1.2.0";
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "amd64"
        };
        let url = format!(
            "https://github.com/oras-project/oras/releases/download/v{version}/oras_{version}_linux_{arch}.tar.gz"
        );

        let status = tokio::process::Command::new("sh")
            .args([
                "-c",
                &format!("curl -fsSL '{url}' | tar xzf - -C '{bin_dir}' oras"),
            ])
            .status()
            .await
            .map_err(|e| {
                DevcontainerError::OrasInstall(format!("failed to download oras: {e}"))
            })?;

        if !status.success() {
            return Err(DevcontainerError::OrasInstall(
                "downloading oras from GitHub releases failed".to_string(),
            ));
        }
    }

    Ok(())
}

/// Fetch a single feature using `oras pull` and extract its contents.
/// Returns the parsed feature metadata.
async fn fetch_feature(
    feature_id: &str,
    output_dir: &Path,
) -> crate::Result<FeatureMetadata> {
    let dir_name = dir_name_from_id(feature_id);
    let feature_dir = output_dir.join(&dir_name);
    tokio::fs::create_dir_all(&feature_dir)
        .await
        .map_err(|e| {
            DevcontainerError::Feature(format!(
                "failed to create dir {}: {e}",
                feature_dir.display()
            ))
        })?;

    info!(feature_id, "pulling feature with oras");

    let output = tokio::process::Command::new("oras")
        .args(["pull", feature_id, "-o"])
        .arg(&feature_dir)
        .output()
        .await
        .map_err(|e| DevcontainerError::OrasCommand(format!("failed to run oras pull: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DevcontainerError::OrasCommand(format!(
            "oras pull {feature_id} failed: {stderr}"
        )));
    }

    // Extract any tgz files
    let tgz_path = feature_dir.join("devcontainer-feature.tgz");
    if tgz_path.exists() {
        let status = tokio::process::Command::new("tar")
            .args(["xzf", "devcontainer-feature.tgz"])
            .current_dir(&feature_dir)
            .status()
            .await
            .map_err(|e| {
                DevcontainerError::Feature(format!("failed to extract tgz: {e}"))
            })?;

        if !status.success() {
            return Err(DevcontainerError::Feature(format!(
                "tar extraction failed for {feature_id}"
            )));
        }
    }

    // Read metadata
    let metadata_path = feature_dir.join("devcontainer-feature.json");
    let metadata_str = tokio::fs::read_to_string(&metadata_path)
        .await
        .map_err(|e| {
            DevcontainerError::Feature(format!(
                "failed to read {}: {e}",
                metadata_path.display()
            ))
        })?;

    let metadata: FeatureMetadata = serde_json::from_str(&metadata_str).map_err(|e| {
        DevcontainerError::Feature(format!(
            "failed to parse {}: {e}",
            metadata_path.display()
        ))
    })?;

    Ok(metadata)
}

/// Topological sort of features based on `installsAfter` and `dependsOn` dependencies.
/// Uses Kahn's algorithm. Features without ordering constraints maintain input order.
fn topo_sort(
    feature_ids: &[String],
    metadata_map: &HashMap<String, FeatureMetadata>,
) -> Vec<String> {
    if feature_ids.is_empty() {
        return Vec::new();
    }

    let id_set: HashSet<&str> = feature_ids.iter().map(|s| s.as_str()).collect();

    // Build adjacency list and in-degree count.
    // An edge from A -> B means "A must be installed before B".
    // Use a set of (from, to) pairs to deduplicate edges when the same dep
    // appears in both installsAfter and dependsOn.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut edge_set: HashSet<(&str, &str)> = HashSet::new();

    for id in feature_ids {
        in_degree.entry(id.as_str()).or_insert(0);
        edges.entry(id.as_str()).or_default();
    }

    for id in feature_ids {
        if let Some(meta) = metadata_map.get(id) {
            // Collect dependency refs from both installsAfter and dependsOn
            let mut dep_refs: Vec<&str> = meta.installs_after.iter().map(|s| s.as_str()).collect();
            for dep_id in meta.depends_on.keys() {
                dep_refs.push(dep_id.as_str());
            }

            for dep in dep_refs {
                let dep_dir = dir_name_from_id(dep);
                for candidate in feature_ids {
                    if candidate == dep || dir_name_from_id(candidate) == dep_dir {
                        if id_set.contains(candidate.as_str()) {
                            // candidate -> id (candidate must come before id)
                            let edge = (candidate.as_str(), id.as_str());
                            if edge_set.insert(edge) {
                                edges.entry(candidate.as_str()).or_default().push(id.as_str());
                                *in_degree.entry(id.as_str()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    // Kahn's algorithm preserving input order for ties
    let mut queue: VecDeque<&str> = VecDeque::new();
    for id in feature_ids {
        if in_degree.get(id.as_str()).copied().unwrap_or(0) == 0 {
            queue.push_back(id.as_str());
        }
    }

    let mut sorted: Vec<String> = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted.push(node.to_string());
        if let Some(neighbors) = edges.get(node) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    // If there are cycles, append remaining features in input order
    if sorted.len() < feature_ids.len() {
        for id in feature_ids {
            if !sorted.contains(id) {
                sorted.push(id.clone());
            }
        }
    }

    sorted
}

/// Generate a Dockerfile snippet for a single feature layer.
fn generate_layer(
    feature_id: &str,
    dir_name: &str,
    options: &serde_json::Value,
    metadata: &FeatureMetadata,
) -> String {
    let mut env_lines = Vec::new();

    // Collect all option names from metadata to set defaults
    let user_options: HashMap<String, String> = match options.as_object() {
        Some(obj) => obj
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), val)
            })
            .collect(),
        None => HashMap::new(),
    };

    // Merge metadata defaults with user-provided options
    let mut merged_options: Vec<(String, String)> = Vec::new();
    for (opt_name, opt_def) in &metadata.options {
        let value = if let Some(user_val) = user_options.get(opt_name) {
            user_val.clone()
        } else if let Some(default_val) = &opt_def.default {
            match default_val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                other => other.to_string(),
            }
        } else {
            continue;
        };
        merged_options.push((opt_name.clone(), value));
    }

    // Also add any user options not in metadata
    for (key, val) in &user_options {
        if !metadata.options.contains_key(key) {
            merged_options.push((key.clone(), val.clone()));
        }
    }

    // Sort for deterministic output
    merged_options.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, value) in &merged_options {
        let env_name = name.to_uppercase();
        env_lines.push(format!("    export {env_name}=\"{value}\" && \\"));
    }

    let mut snippet = format!("# Feature: {feature_id}\n");
    snippet.push_str(&format!(
        "COPY {dir_name}/ /tmp/devcontainer-features/{dir_name}/\n"
    ));
    snippet.push_str(&format!(
        "RUN cd /tmp/devcontainer-features/{dir_name} && \\\n"
    ));
    for line in &env_lines {
        snippet.push_str(line);
        snippet.push('\n');
    }
    snippet.push_str("    chmod +x install.sh && \\\n");
    snippet.push_str("    ./install.sh");

    snippet
}

/// Fetch, order, and resolve features into Dockerfile layers.
pub async fn resolve_features(
    features: &HashMap<String, serde_json::Value>,
    _build_context: &Path,
) -> crate::Result<ResolvedFeatures> {
    if features.is_empty() {
        return Ok(ResolvedFeatures::default());
    }

    ensure_oras().await?;

    let tmp_dir = std::env::temp_dir().join("devcontainer-features");
    tokio::fs::create_dir_all(&tmp_dir).await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to create temp dir: {e}"))
    })?;

    // Collect feature IDs in a stable order
    let mut feature_ids: Vec<String> = features.keys().cloned().collect();

    // Track options for each feature (including auto-injected ones)
    let mut all_options: HashMap<String, serde_json::Value> = features.clone();

    // Fetch all features and collect metadata
    let mut metadata_map: HashMap<String, FeatureMetadata> = HashMap::new();
    for feature_id in &feature_ids {
        let metadata = fetch_feature(feature_id, &tmp_dir).await?;
        metadata_map.insert(feature_id.clone(), metadata);
    }

    // Auto-inject missing dependsOn targets
    let mut injected = true;
    while injected {
        injected = false;
        let current_ids: Vec<String> = feature_ids.clone();
        for id in &current_ids {
            if let Some(meta) = metadata_map.get(id).cloned() {
                for (dep_id, dep_options) in &meta.depends_on {
                    // Check if dep is already present (by full ID or dir name)
                    let dep_dir = dir_name_from_id(dep_id);
                    let already_present = feature_ids.iter().any(|existing| {
                        existing == dep_id || dir_name_from_id(existing) == dep_dir
                    });
                    if !already_present {
                        info!(dep_id, "auto-injecting missing dependsOn target");
                        let dep_metadata = fetch_feature(dep_id, &tmp_dir).await?;
                        metadata_map.insert(dep_id.clone(), dep_metadata);
                        feature_ids.push(dep_id.clone());
                        all_options.insert(dep_id.clone(), dep_options.clone());
                        injected = true;
                    }
                }
            }
        }
    }

    // Topologically sort features
    let sorted_ids = topo_sort(&feature_ids, &metadata_map);

    // Generate layers and collect container_env
    let mut resolved = ResolvedFeatures::default();
    for id in &sorted_ids {
        let dir_name = dir_name_from_id(id);
        let options = all_options.get(id).cloned().unwrap_or(serde_json::Value::Object(
            serde_json::Map::new(),
        ));
        let metadata = metadata_map
            .get(id)
            .cloned()
            .unwrap_or_else(|| FeatureMetadata {
                id: None,
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            });

        // Collect feature containerEnv (later features override earlier)
        for (k, v) in &metadata.container_env {
            resolved.container_env.insert(k.clone(), v.clone());
        }

        let dockerfile_snippet = generate_layer(id, &dir_name, &options, &metadata);
        resolved.layers.push(FeatureLayer {
            id: id.clone(),
            dir_name,
            dockerfile_snippet,
        });
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FeatureOption;

    #[test]
    fn dir_name_from_full_id() {
        assert_eq!(
            dir_name_from_id("ghcr.io/devcontainers/features/node:1"),
            "node"
        );
    }

    #[test]
    fn dir_name_from_id_no_tag() {
        assert_eq!(
            dir_name_from_id("ghcr.io/devcontainers/features/python"),
            "python"
        );
    }

    #[test]
    fn dir_name_from_id_simple() {
        assert_eq!(dir_name_from_id("node"), "node");
    }

    #[test]
    fn topo_sort_no_dependencies() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let metadata: HashMap<String, FeatureMetadata> = ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    FeatureMetadata {
                        id: Some(id.clone()),
                        name: None,
                        version: None,
                        options: HashMap::new(),
                        installs_after: Vec::new(),
                    depends_on: HashMap::new(),
                    container_env: HashMap::new(),
                    },
                )
            })
            .collect();

        let sorted = topo_sort(&ids, &metadata);
        assert_eq!(sorted, vec!["a", "b", "c"]);
    }

    #[test]
    fn topo_sort_simple_chain() {
        // A depends on B (A installs after B), so B should come first
        let ids = vec!["a".to_string(), "b".to_string()];
        let mut metadata: HashMap<String, FeatureMetadata> = HashMap::new();
        metadata.insert(
            "a".to_string(),
            FeatureMetadata {
                id: Some("a".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: vec!["b".to_string()],
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "b".to_string(),
            FeatureMetadata {
                id: Some("b".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );

        let sorted = topo_sort(&ids, &metadata);
        assert_eq!(sorted, vec!["b", "a"]);
    }

    #[test]
    fn topo_sort_diamond() {
        // D depends on B and C; B and C depend on A
        // Expected: A, B, C, D (or A, C, B, D — both valid, but we preserve input order for ties)
        let ids = vec![
            "d".to_string(),
            "b".to_string(),
            "c".to_string(),
            "a".to_string(),
        ];
        let mut metadata: HashMap<String, FeatureMetadata> = HashMap::new();
        metadata.insert(
            "a".to_string(),
            FeatureMetadata {
                id: Some("a".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "b".to_string(),
            FeatureMetadata {
                id: Some("b".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: vec!["a".to_string()],
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "c".to_string(),
            FeatureMetadata {
                id: Some("c".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: vec!["a".to_string()],
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "d".to_string(),
            FeatureMetadata {
                id: Some("d".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: vec!["b".to_string(), "c".to_string()],
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );

        let sorted = topo_sort(&ids, &metadata);
        // A must come before B and C; B and C must come before D
        let pos_a = sorted.iter().position(|x| x == "a").unwrap();
        let pos_b = sorted.iter().position(|x| x == "b").unwrap();
        let pos_c = sorted.iter().position(|x| x == "c").unwrap();
        let pos_d = sorted.iter().position(|x| x == "d").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_a < pos_c);
        assert!(pos_b < pos_d);
        assert!(pos_c < pos_d);
    }

    #[test]
    fn generate_layer_with_options() {
        let options = serde_json::json!({"version": "20"});
        let mut meta_options = HashMap::new();
        meta_options.insert(
            "version".to_string(),
            FeatureOption {
                option_type: Some("string".to_string()),
                default: Some(serde_json::Value::String("lts".to_string())),
                description: Some("Node.js version".to_string()),
            },
        );
        let metadata = FeatureMetadata {
            id: Some("node".to_string()),
            name: Some("Node.js".to_string()),
            version: Some("1.0.0".to_string()),
            options: meta_options,
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: HashMap::new(),
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
        );

        assert!(snippet.contains("# Feature: ghcr.io/devcontainers/features/node:1"));
        assert!(snippet.contains("COPY node/ /tmp/devcontainer-features/node/"));
        assert!(snippet.contains("export VERSION=\"20\""));
        assert!(snippet.contains("chmod +x install.sh"));
        assert!(snippet.contains("./install.sh"));
    }

    #[test]
    fn generate_layer_with_defaults() {
        let options = serde_json::json!({});
        let mut meta_options = HashMap::new();
        meta_options.insert(
            "version".to_string(),
            FeatureOption {
                option_type: Some("string".to_string()),
                default: Some(serde_json::Value::String("lts".to_string())),
                description: Some("Node.js version".to_string()),
            },
        );
        let metadata = FeatureMetadata {
            id: Some("node".to_string()),
            name: None,
            version: None,
            options: meta_options,
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: HashMap::new(),
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
        );

        // Default value "lts" should be used
        assert!(snippet.contains("export VERSION=\"lts\""));
    }

    #[test]
    fn generate_layer_no_options() {
        let options = serde_json::json!({});
        let metadata = FeatureMetadata {
            id: Some("common-utils".to_string()),
            name: None,
            version: None,
            options: HashMap::new(),
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: HashMap::new(),
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/common-utils:1",
            "common-utils",
            &options,
            &metadata,
        );

        assert!(snippet.contains("# Feature: ghcr.io/devcontainers/features/common-utils:1"));
        assert!(snippet.contains("COPY common-utils/ /tmp/devcontainer-features/common-utils/"));
        assert!(snippet.contains("chmod +x install.sh"));
        assert!(!snippet.contains("export "));
    }

    #[test]
    fn topo_sort_depends_on_present() {
        // A dependsOn B, both present → B before A
        let ids = vec!["a".to_string(), "b".to_string()];
        let mut metadata: HashMap<String, FeatureMetadata> = HashMap::new();
        let mut depends = HashMap::new();
        depends.insert("b".to_string(), serde_json::json!({}));
        metadata.insert(
            "a".to_string(),
            FeatureMetadata {
                id: Some("a".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: depends,
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "b".to_string(),
            FeatureMetadata {
                id: Some("b".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );

        let sorted = topo_sort(&ids, &metadata);
        assert_eq!(sorted, vec!["b", "a"]);
    }

    #[test]
    fn topo_sort_depends_on_and_installs_after_deduped() {
        // A has both dependsOn B and installsAfter B — should not double-count
        let ids = vec!["a".to_string(), "b".to_string()];
        let mut metadata: HashMap<String, FeatureMetadata> = HashMap::new();
        let mut depends = HashMap::new();
        depends.insert("b".to_string(), serde_json::json!({}));
        metadata.insert(
            "a".to_string(),
            FeatureMetadata {
                id: Some("a".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: vec!["b".to_string()],
                depends_on: depends,
                container_env: HashMap::new(),
            },
        );
        metadata.insert(
            "b".to_string(),
            FeatureMetadata {
                id: Some("b".to_string()),
                name: None,
                version: None,
                options: HashMap::new(),
                installs_after: Vec::new(),
                depends_on: HashMap::new(),
                container_env: HashMap::new(),
            },
        );

        let sorted = topo_sort(&ids, &metadata);
        assert_eq!(sorted, vec!["b", "a"]);
    }

    #[tokio::test]
    #[ignore = "requires oras"]
    async fn fetch_feature_integration() {
        let tmp = tempfile::tempdir().unwrap();
        let metadata =
            fetch_feature("ghcr.io/devcontainers/features/node:1", tmp.path())
                .await
                .unwrap();
        assert!(metadata.id.is_some());
        assert!(tmp.path().join("node/install.sh").exists());
    }

    #[tokio::test]
    #[ignore = "requires oras"]
    async fn resolve_features_integration() {
        let tmp = tempfile::tempdir().unwrap();
        let mut features = HashMap::new();
        features.insert(
            "ghcr.io/devcontainers/features/node:1".to_string(),
            serde_json::json!({"version": "20"}),
        );
        let resolved = resolve_features(&features, tmp.path()).await.unwrap();
        assert_eq!(resolved.layers.len(), 1);
        assert_eq!(resolved.layers[0].dir_name, "node");
        assert!(resolved.layers[0].dockerfile_snippet.contains("export VERSION=\"20\""));
    }

    #[test]
    fn feature_container_env_collected() {
        // Simulate what resolve_features does: collect container_env from metadata in sort order
        let mut resolved = ResolvedFeatures::default();

        let meta_a = FeatureMetadata {
            id: Some("a".to_string()),
            name: None,
            version: None,
            options: HashMap::new(),
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: {
                let mut env = HashMap::new();
                env.insert("FOO".to_string(), "from_a".to_string());
                env.insert("BAR".to_string(), "from_a".to_string());
                env
            },
        };
        let meta_b = FeatureMetadata {
            id: Some("b".to_string()),
            name: None,
            version: None,
            options: HashMap::new(),
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: {
                let mut env = HashMap::new();
                env.insert("FOO".to_string(), "from_b".to_string());
                env
            },
        };

        // A is sorted first, then B — B's FOO overrides A's
        for meta in [&meta_a, &meta_b] {
            for (k, v) in &meta.container_env {
                resolved.container_env.insert(k.clone(), v.clone());
            }
        }

        assert_eq!(resolved.container_env.get("FOO").map(String::as_str), Some("from_b"));
        assert_eq!(resolved.container_env.get("BAR").map(String::as_str), Some("from_a"));
    }
}
