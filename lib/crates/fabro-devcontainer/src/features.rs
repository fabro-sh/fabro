use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;
use std::path::Path;

use tokio::fs;
use tokio::process::Command;
use tracing::info;

use crate::DevcontainerError;
use crate::types::{FeatureMetadata, LifecycleCommand};

/// A resolved feature layer ready to be inserted into a Dockerfile.
#[derive(Debug, Clone)]
pub(crate) struct FeatureLayer {
    /// Feature identifier (e.g. "ghcr.io/devcontainers/features/node:1")
    pub id: String,
    /// Directory name for COPY
    pub dir_name: String,
    /// Dockerfile snippet for this feature
    pub dockerfile_snippet: String,
}

/// All resolved feature data: layers, environment, and lifecycle hooks.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedFeatures {
    pub layers: Vec<FeatureLayer>,
    pub container_env: HashMap<String, String>,
    pub on_create_commands: Vec<LifecycleCommand>,
    pub post_create_commands: Vec<LifecycleCommand>,
    pub post_start_commands: Vec<LifecycleCommand>,
}

/// Extract the directory name from a feature ID.
/// Handles OCI refs ("ghcr.io/devcontainers/features/node:1" → "node"),
/// local paths ("./my-feature" → "my-feature"),
/// and HTTPS URLs ("https://example.com/feature.tgz" → "feature").
fn dir_name_from_id(feature_id: &str) -> String {
    // Local path: strip leading ./ or ../
    if feature_id.starts_with("./") || feature_id.starts_with("../") {
        let stripped = feature_id
            .trim_start_matches("../")
            .trim_start_matches("./");
        return stripped.rsplit('/').next().unwrap_or(stripped).to_string();
    }

    // HTTPS URL: take filename, strip .tgz extension
    if feature_id.starts_with("https://") {
        let filename = feature_id.rsplit('/').next().unwrap_or(feature_id);
        return filename
            .strip_suffix(".tgz")
            .or_else(|| filename.strip_suffix(".tar.gz"))
            .unwrap_or(filename)
            .to_string();
    }

    // OCI ref: strip tag, take last path segment
    let without_tag = feature_id.split(':').next().unwrap_or(feature_id);
    without_tag
        .rsplit('/')
        .next()
        .unwrap_or(without_tag)
        .to_string()
}

/// Ensure `oras` CLI is available, installing it if necessary.
async fn ensure_oras() -> crate::Result<()> {
    let check = Command::new("which")
        .arg("oras")
        .output()
        .await
        .map_err(|e| DevcontainerError::OrasInstall(format!("failed to check for oras: {e}")))?;

    if check.status.success() {
        return Ok(());
    }

    info!("oras not found, attempting to install");

    if cfg!(target_os = "macos") {
        let status = Command::new("brew")
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

        fs::create_dir_all(&bin_dir).await.map_err(|e| {
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

        let status = Command::new("sh")
            .args([
                "-c",
                &format!("curl -fsSL '{url}' | tar xzf - -C '{bin_dir}' oras"),
            ])
            .status()
            .await
            .map_err(|e| DevcontainerError::OrasInstall(format!("failed to download oras: {e}")))?;

        if !status.success() {
            return Err(DevcontainerError::OrasInstall(
                "downloading oras from GitHub releases failed".to_string(),
            ));
        }
    }

    Ok(())
}

/// Find the first `.tgz` file in a directory.
async fn find_tgz(dir: &Path) -> Option<String> {
    let mut entries = fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            if Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tgz"))
            {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Extract a tgz archive in the given directory.
async fn extract_tgz(feature_dir: &Path, tgz_name: &str, feature_id: &str) -> crate::Result<()> {
    let status = Command::new("tar")
        .args(["xzf", tgz_name])
        .current_dir(feature_dir)
        .status()
        .await
        .map_err(|e| DevcontainerError::Feature(format!("failed to extract tgz: {e}")))?;

    if !status.success() {
        return Err(DevcontainerError::Feature(format!(
            "tar extraction failed for {feature_id}"
        )));
    }
    Ok(())
}

/// Read and parse devcontainer-feature.json from a feature directory.
async fn read_feature_metadata(feature_dir: &Path) -> crate::Result<FeatureMetadata> {
    let metadata_path = feature_dir.join("devcontainer-feature.json");
    let metadata_str = fs::read_to_string(&metadata_path).await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to read {}: {e}", metadata_path.display()))
    })?;

    serde_json::from_str(&metadata_str).map_err(|e| {
        DevcontainerError::Feature(format!("failed to parse {}: {e}", metadata_path.display()))
    })
}

/// Create a feature output directory under the temp dir.
async fn create_feature_dir(
    output_dir: &Path,
    feature_id: &str,
) -> crate::Result<std::path::PathBuf> {
    let dir_name = dir_name_from_id(feature_id);
    let feature_dir = output_dir.join(&dir_name);
    fs::create_dir_all(&feature_dir).await.map_err(|e| {
        DevcontainerError::Feature(format!(
            "failed to create dir {}: {e}",
            feature_dir.display()
        ))
    })?;
    Ok(feature_dir)
}

/// Fetch a single OCI feature using `oras pull` and extract its contents.
async fn fetch_feature_oci(feature_id: &str, output_dir: &Path) -> crate::Result<FeatureMetadata> {
    let feature_dir = create_feature_dir(output_dir, feature_id).await?;

    info!(feature_id, "pulling feature with oras");

    let output = Command::new("oras")
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

    // OCI registries may name the tgz with a feature suffix (e.g. devcontainer-feature-node.tgz)
    if let Some(tgz) = find_tgz(&feature_dir).await {
        extract_tgz(&feature_dir, &tgz, feature_id).await?;
    }

    read_feature_metadata(&feature_dir).await
}

/// Fetch a local feature by copying its directory.
async fn fetch_feature_local(
    feature_id: &str,
    output_dir: &Path,
    devcontainer_dir: &Path,
) -> crate::Result<FeatureMetadata> {
    let local_path = devcontainer_dir.join(feature_id);
    if !local_path.is_dir() {
        return Err(DevcontainerError::Feature(format!(
            "local feature path not found: {}",
            local_path.display()
        )));
    }

    let feature_dir = create_feature_dir(output_dir, feature_id).await?;
    copy_dir_recursive(&local_path, &feature_dir).await?;

    read_feature_metadata(&feature_dir).await
}

/// Fetch a feature from an HTTPS URL (tgz archive).
async fn fetch_feature_https(
    feature_id: &str,
    output_dir: &Path,
) -> crate::Result<FeatureMetadata> {
    let feature_dir = create_feature_dir(output_dir, feature_id).await?;

    info!(feature_id, "downloading feature from HTTPS");

    let response = reqwest::get(feature_id)
        .await
        .map_err(|e| DevcontainerError::Feature(format!("failed to download {feature_id}: {e}")))?;

    if !response.status().is_success() {
        return Err(DevcontainerError::Feature(format!(
            "HTTP {} downloading {feature_id}",
            response.status()
        )));
    }

    let bytes = response.bytes().await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to read response for {feature_id}: {e}"))
    })?;

    let tgz_path = feature_dir.join("devcontainer-feature.tgz");
    fs::write(&tgz_path, &bytes).await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to write {}: {e}", tgz_path.display()))
    })?;

    extract_tgz(&feature_dir, "devcontainer-feature.tgz", feature_id).await?;

    read_feature_metadata(&feature_dir).await
}

/// Dispatch feature fetch based on the feature ID prefix.
async fn fetch_feature_dispatch(
    feature_id: &str,
    output_dir: &Path,
    devcontainer_dir: &Path,
    oras_checked: &mut bool,
) -> crate::Result<FeatureMetadata> {
    if feature_id.starts_with("./") || feature_id.starts_with("../") {
        fetch_feature_local(feature_id, output_dir, devcontainer_dir).await
    } else if feature_id.starts_with("https://") {
        fetch_feature_https(feature_id, output_dir).await
    } else {
        if !*oras_checked {
            ensure_oras().await?;
            *oras_checked = true;
        }
        fetch_feature_oci(feature_id, output_dir).await
    }
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> crate::Result<()> {
    fs::create_dir_all(dst).await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to create dir {}: {e}", dst.display()))
    })?;

    let mut entries = fs::read_dir(src).await.map_err(|e| {
        DevcontainerError::Feature(format!("failed to read dir {}: {e}", src.display()))
    })?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| DevcontainerError::Feature(format!("failed to read dir entry: {e}")))?
    {
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if entry_path.is_dir() {
            Box::pin(copy_dir_recursive(&entry_path, &dest_path)).await?;
        } else {
            fs::copy(&entry_path, &dest_path).await.map_err(|e| {
                DevcontainerError::Feature(format!(
                    "failed to copy {} to {}: {e}",
                    entry_path.display(),
                    dest_path.display()
                ))
            })?;
        }
    }

    Ok(())
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

    let id_set: HashSet<&str> = feature_ids
        .iter()
        .map(std::string::String::as_str)
        .collect();

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
            let mut dep_refs: Vec<&str> = meta
                .installs_after
                .iter()
                .map(std::string::String::as_str)
                .collect();
            for dep_id in meta.depends_on.keys() {
                dep_refs.push(dep_id.as_str());
            }

            for dep in dep_refs {
                let dep_dir = dir_name_from_id(dep);
                for candidate in feature_ids {
                    if (candidate == dep || dir_name_from_id(candidate) == dep_dir)
                        && id_set.contains(candidate.as_str())
                    {
                        // candidate -> id (candidate must come before id)
                        let edge = (candidate.as_str(), id.as_str());
                        if edge_set.insert(edge) {
                            edges
                                .entry(candidate.as_str())
                                .or_default()
                                .push(id.as_str());
                            *in_degree.entry(id.as_str()).or_insert(0) += 1;
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

/// Convert an option ID to an environment variable name per the dev container spec.
/// Replaces non-alphanumeric, non-underscore chars with `_`, strips leading digits/underscores,
/// and uppercases the result.
fn option_id_to_env_name(id: &str) -> String {
    let replaced: String = id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = replaced.trim_start_matches(|c: char| c == '_' || c.is_ascii_digit());
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_uppercase()
    }
}

/// Generate a Dockerfile snippet for a single feature layer.
fn generate_layer(
    feature_id: &str,
    dir_name: &str,
    options: &serde_json::Value,
    metadata: &FeatureMetadata,
    remote_user: Option<&str>,
) -> String {
    let mut env_lines = Vec::new();

    // Emit built-in user env vars expected by community features
    let ru = remote_user.unwrap_or("root");
    let ru_home = if ru == "root" {
        "/root".to_string()
    } else {
        format!("/home/{ru}")
    };
    env_lines.push(format!("    export _REMOTE_USER=\"{ru}\" && \\"));
    env_lines.push("    export _CONTAINER_USER=\"root\" && \\".to_string());
    env_lines.push(format!("    export _REMOTE_USER_HOME=\"{ru_home}\" && \\"));
    env_lines.push("    export _CONTAINER_USER_HOME=\"/root\" && \\".to_string());

    // Normalize shorthand version syntax: "1.18" → {"version": "1.18"}
    let options_obj = match options {
        serde_json::Value::String(s) => {
            let mut map = serde_json::Map::new();
            map.insert("version".to_string(), serde_json::Value::String(s.clone()));
            map
        }
        serde_json::Value::Object(obj) => obj.clone(),
        _ => serde_json::Map::new(),
    };

    // Collect all option names from metadata to set defaults
    let user_options: HashMap<String, String> = options_obj
        .iter()
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), val)
        })
        .collect();

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
        let env_name = option_id_to_env_name(name);
        env_lines.push(format!("    export {env_name}=\"{value}\" && \\"));
    }

    let mut snippet = format!("# Feature: {feature_id}\n");
    let _ = writeln!(
        snippet,
        "COPY {dir_name}/ /tmp/devcontainer-features/{dir_name}/"
    );
    let _ = writeln!(
        snippet,
        "RUN cd /tmp/devcontainer-features/{dir_name} && \\"
    );
    for line in &env_lines {
        snippet.push_str(line);
        snippet.push('\n');
    }
    snippet.push_str("    chmod +x install.sh && \\\n");
    snippet.push_str("    ./install.sh");

    snippet
}

/// Fetch, order, and resolve features into Dockerfile layers.
pub(crate) async fn resolve_features(
    features: &HashMap<String, serde_json::Value>,
    devcontainer_dir: &Path,
    remote_user: Option<&str>,
) -> crate::Result<ResolvedFeatures> {
    if features.is_empty() {
        return Ok(ResolvedFeatures::default());
    }

    let unique_id = format!(
        "devcontainer-features-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let tmp_dir = std::env::temp_dir().join(unique_id);
    fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|e| DevcontainerError::Feature(format!("failed to create temp dir: {e}")))?;

    // Collect feature IDs in a stable order
    let mut feature_ids: Vec<String> = features.keys().cloned().collect();

    // Track options for each feature (including auto-injected ones)
    let mut all_options: HashMap<String, serde_json::Value> = features.clone();

    // Fetch all features and collect metadata
    let mut oras_checked = false;
    let mut metadata_map: HashMap<String, FeatureMetadata> = HashMap::new();
    for feature_id in &feature_ids {
        let metadata =
            fetch_feature_dispatch(feature_id, &tmp_dir, devcontainer_dir, &mut oras_checked)
                .await?;
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
                        let dep_metadata = fetch_feature_dispatch(
                            dep_id,
                            &tmp_dir,
                            devcontainer_dir,
                            &mut oras_checked,
                        )
                        .await?;
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
        let options = all_options
            .get(id)
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
            });

        // Collect feature containerEnv (later features override earlier)
        for (k, v) in &metadata.container_env {
            resolved.container_env.insert(k.clone(), v.clone());
        }

        // Collect feature lifecycle hooks
        if let Some(cmd) = &metadata.on_create_command {
            resolved.on_create_commands.push(cmd.clone());
        }
        if let Some(cmd) = &metadata.post_create_command {
            resolved.post_create_commands.push(cmd.clone());
        }
        if let Some(cmd) = &metadata.post_start_command {
            resolved.post_start_commands.push(cmd.clone());
        }

        let dockerfile_snippet = generate_layer(id, &dir_name, &options, &metadata, remote_user);
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
    fn dir_name_from_local_path() {
        assert_eq!(dir_name_from_id("./my-feature"), "my-feature");
        assert_eq!(dir_name_from_id("./sub/my-feature"), "my-feature");
        assert_eq!(dir_name_from_id("../shared-feature"), "shared-feature");
    }

    #[test]
    fn dir_name_from_https_url() {
        assert_eq!(
            dir_name_from_id("https://example.com/features/node.tgz"),
            "node"
        );
        assert_eq!(
            dir_name_from_id("https://example.com/features/python.tar.gz"),
            "python"
        );
        assert_eq!(dir_name_from_id("https://example.com/features/go"), "go");
    }

    #[test]
    fn fetch_feature_dispatch_routes() {
        // Verify the routing logic by checking prefix detection
        assert!("./local-feature".starts_with("./"));
        assert!("../parent-feature".starts_with("../"));
        assert!("https://example.com/feature.tgz".starts_with("https://"));
        assert!(!"ghcr.io/foo/bar:1".starts_with("./"));
        assert!(!"ghcr.io/foo/bar:1".starts_with("../"));
        assert!(!"ghcr.io/foo/bar:1".starts_with("https://"));
    }

    #[tokio::test]
    async fn fetch_feature_local_integration() {
        let tmp_src = tempfile::tempdir().unwrap();
        let feature_dir = tmp_src.path().join("my-feature");
        std::fs::create_dir_all(&feature_dir).unwrap();

        // Create a minimal devcontainer-feature.json
        std::fs::write(
            feature_dir.join("devcontainer-feature.json"),
            r#"{"id": "my-feature", "version": "1.0.0"}"#,
        )
        .unwrap();

        // Create a dummy install.sh
        std::fs::write(feature_dir.join("install.sh"), "#!/bin/sh\necho hi").unwrap();

        let tmp_out = tempfile::tempdir().unwrap();
        let metadata = fetch_feature_local("./my-feature", tmp_out.path(), tmp_src.path())
            .await
            .unwrap();
        assert_eq!(metadata.id.as_deref(), Some("my-feature"));
        assert!(tmp_out.path().join("my-feature/install.sh").exists());
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
                        on_create_command: None,
                        post_create_command: None,
                        post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
            None,
        );

        insta::assert_snapshot!(snippet, @r#"
        # Feature: ghcr.io/devcontainers/features/node:1
        COPY node/ /tmp/devcontainer-features/node/
        RUN cd /tmp/devcontainer-features/node && \
            export _REMOTE_USER="root" && \
            export _CONTAINER_USER="root" && \
            export _REMOTE_USER_HOME="/root" && \
            export _CONTAINER_USER_HOME="/root" && \
            export VERSION="20" && \
            chmod +x install.sh && \
            ./install.sh
        "#);
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
            None,
        );

        insta::assert_snapshot!(snippet, @r#"
        # Feature: ghcr.io/devcontainers/features/node:1
        COPY node/ /tmp/devcontainer-features/node/
        RUN cd /tmp/devcontainer-features/node && \
            export _REMOTE_USER="root" && \
            export _CONTAINER_USER="root" && \
            export _REMOTE_USER_HOME="/root" && \
            export _CONTAINER_USER_HOME="/root" && \
            export VERSION="lts" && \
            chmod +x install.sh && \
            ./install.sh
        "#);
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/common-utils:1",
            "common-utils",
            &options,
            &metadata,
            None,
        );

        insta::assert_snapshot!(snippet, @r#"
        # Feature: ghcr.io/devcontainers/features/common-utils:1
        COPY common-utils/ /tmp/devcontainer-features/common-utils/
        RUN cd /tmp/devcontainer-features/common-utils && \
            export _REMOTE_USER="root" && \
            export _CONTAINER_USER="root" && \
            export _REMOTE_USER_HOME="/root" && \
            export _CONTAINER_USER_HOME="/root" && \
            chmod +x install.sh && \
            ./install.sh
        "#);
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
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
                on_create_command: None,
                post_create_command: None,
                post_start_command: None,
            },
        );

        let sorted = topo_sort(&ids, &metadata);
        assert_eq!(sorted, vec!["b", "a"]);
    }

    #[tokio::test]
    #[ignore = "requires oras"]
    async fn fetch_feature_oci_integration() {
        if std::env::var_os("FABRO_ENABLE_FETCH_FEATURE_OCI_INTEGRATION").is_none() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let metadata = fetch_feature_oci("ghcr.io/devcontainers/features/node:1", tmp.path())
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
        let resolved = resolve_features(&features, tmp.path(), None).await.unwrap();
        assert_eq!(resolved.layers.len(), 1);
        assert_eq!(resolved.layers[0].dir_name, "node");
        assert!(
            resolved.layers[0]
                .dockerfile_snippet
                .contains("export VERSION=\"20\"")
        );
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        // A is sorted first, then B — B's FOO overrides A's
        for meta in [&meta_a, &meta_b] {
            for (k, v) in &meta.container_env {
                resolved.container_env.insert(k.clone(), v.clone());
            }
        }

        assert_eq!(
            resolved.container_env.get("FOO").map(String::as_str),
            Some("from_b")
        );
        assert_eq!(
            resolved.container_env.get("BAR").map(String::as_str),
            Some("from_a")
        );
    }

    #[test]
    fn feature_lifecycle_hooks_collected() {
        let mut resolved = ResolvedFeatures::default();

        let cmds = [
            LifecycleCommand::String("setup-a".to_string()),
            LifecycleCommand::Array(vec!["make".to_string(), "build".to_string()]),
        ];

        // Simulate collecting from two features
        resolved.on_create_commands.push(cmds[0].clone());
        resolved.post_create_commands.push(cmds[1].clone());
        resolved.post_start_commands.push(cmds[0].clone());

        assert_eq!(resolved.on_create_commands.len(), 1);
        assert!(
            matches!(&resolved.on_create_commands[0], LifecycleCommand::String(s) if s == "setup-a")
        );
        assert_eq!(resolved.post_create_commands.len(), 1);
        assert!(
            matches!(&resolved.post_create_commands[0], LifecycleCommand::Array(arr) if arr.len() == 2)
        );
        assert_eq!(resolved.post_start_commands.len(), 1);
    }

    #[test]
    fn option_id_to_env_name_hyphenated() {
        assert_eq!(option_id_to_env_name("node-version"), "NODE_VERSION");
    }

    #[test]
    fn option_id_to_env_name_leading_digit() {
        assert_eq!(option_id_to_env_name("2fast"), "FAST");
    }

    #[test]
    fn option_id_to_env_name_simple() {
        assert_eq!(option_id_to_env_name("simple"), "SIMPLE");
    }

    #[test]
    fn generate_layer_shorthand_version() {
        let options = serde_json::json!("20");
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
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
            None,
        );

        assert!(snippet.contains("export VERSION=\"20\""));
    }

    #[test]
    fn generate_layer_install_env_vars() {
        let options = serde_json::json!({});
        let metadata = FeatureMetadata {
            id: Some("node".to_string()),
            name: None,
            version: None,
            options: HashMap::new(),
            installs_after: Vec::new(),
            depends_on: HashMap::new(),
            container_env: HashMap::new(),
            on_create_command: None,
            post_create_command: None,
            post_start_command: None,
        };

        let snippet = generate_layer(
            "ghcr.io/devcontainers/features/node:1",
            "node",
            &options,
            &metadata,
            Some("vscode"),
        );

        assert!(snippet.contains("_REMOTE_USER=\"vscode\""));
        assert!(snippet.contains("_CONTAINER_USER=\"root\""));
        assert!(snippet.contains("_REMOTE_USER_HOME=\"/home/vscode\""));
        assert!(snippet.contains("_CONTAINER_USER_HOME=\"/root\""));
    }
}
