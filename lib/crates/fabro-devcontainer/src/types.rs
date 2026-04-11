use std::collections::HashMap;

use serde::Deserialize;

/// Top-level devcontainer.json schema (subset of the spec we support).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DevcontainerJson {
    /// Base image (image mode)
    pub image: Option<String>,

    /// Dockerfile build config
    pub build: Option<BuildSpec>,

    /// Docker Compose file path(s) (compose mode)
    pub docker_compose_file: Option<ComposeFileRef>,

    /// Service name for compose mode
    pub service: Option<String>,

    /// Features to install: feature ID → options object
    #[serde(default)]
    pub features: HashMap<String, serde_json::Value>,

    /// Ports to forward
    #[serde(default, alias = "forwardPorts")]
    pub forward_ports: Vec<serde_json::Value>,

    /// Environment variables set in the container
    #[serde(default)]
    pub remote_env: Option<HashMap<String, String>>,

    /// Environment variables set in the container (containerEnv)
    #[serde(default)]
    pub container_env: Option<HashMap<String, String>>,

    /// Non-root user to run as
    pub remote_user: Option<String>,

    /// Container user
    pub container_user: Option<String>,

    /// Workspace folder path inside container
    pub workspace_folder: Option<String>,

    /// Workspace mount string
    pub workspace_mount: Option<String>,

    /// Run on host before anything else
    pub initialize_command: Option<LifecycleCommand>,

    /// Run in container after first creation (before updateContentCommand)
    pub on_create_command: Option<LifecycleCommand>,

    /// Run in container after creation
    pub post_create_command: Option<LifecycleCommand>,

    /// Run in container on every start
    pub post_start_command: Option<LifecycleCommand>,

    /// Override the default command
    pub override_command: Option<bool>,
}

/// Build configuration for Dockerfile mode.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildSpec {
    /// Path to Dockerfile (relative to devcontainer.json)
    pub dockerfile: Option<String>,

    /// Build context directory (relative to devcontainer.json)
    pub context: Option<String>,

    /// Build arguments
    #[serde(default)]
    pub args: HashMap<String, String>,

    /// Multi-stage build target
    pub target: Option<String>,
}

/// A reference to one or more Docker Compose files.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ComposeFileRef {
    Single(String),
    Multiple(Vec<String>),
}

impl ComposeFileRef {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Single(s) => vec![s.as_str()],
            Self::Multiple(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// A lifecycle command can be a string, array of strings, or object of named
/// commands.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LifecycleCommand {
    String(String),
    Array(Vec<String>),
    Object(HashMap<String, String>),
}

/// Metadata from a devcontainer-feature.json file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeatureMetadata {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,

    #[serde(default)]
    pub options: HashMap<String, FeatureOption>,

    /// Feature IDs that this feature should be installed after
    #[serde(default)]
    pub installs_after: Vec<String>,

    /// Hard dependencies: feature IDs that must be present (auto-installed if
    /// missing)
    #[serde(default)]
    pub depends_on: HashMap<String, serde_json::Value>,

    /// Environment variables contributed by this feature
    #[serde(default)]
    pub container_env: HashMap<String, String>,

    /// Lifecycle hooks contributed by this feature
    pub on_create_command: Option<LifecycleCommand>,
    pub post_create_command: Option<LifecycleCommand>,
    pub post_start_command: Option<LifecycleCommand>,
}

/// A single option for a devcontainer feature.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FeatureOption {
    #[serde(rename = "type")]
    pub option_type: Option<String>,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_image_only() {
        let json = r#"{"image": "mcr.microsoft.com/devcontainers/base:ubuntu"}"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.image.as_deref(),
            Some("mcr.microsoft.com/devcontainers/base:ubuntu")
        );
    }

    #[test]
    fn parse_with_features() {
        let json = r#"{
            "image": "ubuntu",
            "features": {
                "ghcr.io/devcontainers/features/node:1": {"version": "20"},
                "ghcr.io/devcontainers/features/python:1": {}
            }
        }"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert_eq!(config.features.len(), 2);
    }

    #[test]
    fn parse_lifecycle_string() {
        let json = r#"{"postCreateCommand": "npm install"}"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert!(matches!(
            config.post_create_command,
            Some(LifecycleCommand::String(ref s)) if s == "npm install"
        ));
    }

    #[test]
    fn parse_lifecycle_array() {
        let json = r#"{"postCreateCommand": ["npm", "install"]}"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert!(matches!(
            config.post_create_command,
            Some(LifecycleCommand::Array(ref arr)) if arr == &["npm", "install"]
        ));
    }

    #[test]
    fn parse_lifecycle_object() {
        let json = r#"{"postCreateCommand": {"install": "npm install", "build": "npm run build"}}"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert!(matches!(
            config.post_create_command,
            Some(LifecycleCommand::Object(ref map)) if map.len() == 2
        ));
    }

    #[test]
    fn parse_build_config() {
        let json = r#"{
            "build": {
                "dockerfile": "Dockerfile",
                "context": "..",
                "args": {"VARIANT": "3.9"},
                "target": "dev"
            }
        }"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        let build = config.build.unwrap();
        assert_eq!(build.dockerfile.as_deref(), Some("Dockerfile"));
        assert_eq!(build.context.as_deref(), Some(".."));
        assert_eq!(build.args.get("VARIANT").map(String::as_str), Some("3.9"));
        assert_eq!(build.target.as_deref(), Some("dev"));
    }

    #[test]
    fn parse_compose_mode() {
        let json = r#"{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "workspaceFolder": "/workspace"
        }"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.docker_compose_file.as_ref().unwrap().paths(),
            vec!["docker-compose.yml"]
        );
        assert_eq!(config.service.as_deref(), Some("app"));
        assert_eq!(config.workspace_folder.as_deref(), Some("/workspace"));
    }

    #[test]
    fn parse_compose_mode_array() {
        let json = r#"{
            "dockerComposeFile": ["docker-compose.yml", "docker-compose.override.yml"],
            "service": "app"
        }"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.docker_compose_file.as_ref().unwrap().paths(),
            vec!["docker-compose.yml", "docker-compose.override.yml"]
        );
    }

    #[test]
    fn unknown_fields_ignored() {
        let json = r#"{"image": "ubuntu", "unknownField": true, "customizations": {}}"#;
        let config: DevcontainerJson = serde_json::from_str(json).unwrap();
        assert_eq!(config.image.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn parse_feature_metadata_lifecycle_hooks() {
        let json = r#"{
            "id": "python",
            "onCreateCommand": "pip install -r requirements.txt",
            "postCreateCommand": ["python", "setup.py"],
            "postStartCommand": {"server": "python app.py"}
        }"#;
        let meta: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert!(
            matches!(meta.on_create_command, Some(LifecycleCommand::String(ref s)) if s == "pip install -r requirements.txt")
        );
        assert!(
            matches!(meta.post_create_command, Some(LifecycleCommand::Array(ref arr)) if arr == &["python", "setup.py"])
        );
        assert!(
            matches!(meta.post_start_command, Some(LifecycleCommand::Object(ref map)) if map.len() == 1)
        );
    }

    #[test]
    fn parse_feature_metadata_container_env() {
        let json = r#"{
            "id": "node",
            "containerEnv": {
                "NODE_ENV": "development",
                "PATH": "/usr/local/bin:${PATH}"
            }
        }"#;
        let meta: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.container_env.len(), 2);
        assert_eq!(
            meta.container_env.get("NODE_ENV").map(String::as_str),
            Some("development")
        );
    }

    #[test]
    fn parse_feature_metadata_depends_on() {
        let json = r#"{
            "id": "python",
            "dependsOn": {
                "ghcr.io/devcontainers/features/common-utils:1": {},
                "ghcr.io/devcontainers/features/node:1": {"version": "20"}
            }
        }"#;
        let meta: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.depends_on.len(), 2);
        assert!(
            meta.depends_on
                .contains_key("ghcr.io/devcontainers/features/common-utils:1")
        );
        assert_eq!(
            meta.depends_on.get("ghcr.io/devcontainers/features/node:1"),
            Some(&serde_json::json!({"version": "20"}))
        );
    }

    #[test]
    fn parse_feature_metadata() {
        let json = r#"{
            "id": "node",
            "name": "Node.js",
            "version": "1.0.0",
            "options": {
                "version": {
                    "type": "string",
                    "default": "lts",
                    "description": "Node.js version"
                }
            },
            "installsAfter": ["ghcr.io/devcontainers/features/common-utils"]
        }"#;
        let meta: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.id.as_deref(), Some("node"));
        assert_eq!(meta.options.len(), 1);
        assert_eq!(meta.installs_after.len(), 1);
    }
}
