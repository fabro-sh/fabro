use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    /// Provider type: "local", "docker", "daytona", "exe"
    pub provider: String,
    /// Working directory inside the sandbox
    pub working_directory: String,
    /// Provider-specific identifier (container_id / sandbox_name / vm_name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// Docker: host path that is bind-mounted into the container
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_working_directory: Option<String>,
    /// Docker: mount point inside the container
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_mount_point: Option<String>,
    /// Exe: SSH destination for the data plane
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_host: Option<String>,
}

impl SandboxRecord {
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::save_json(self, path, "sandbox_record")
    }

    pub fn load(path: &Path) -> Result<Self> {
        crate::load_json(path, "sandbox_record")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip_local() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.json");

        let record = SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp/work".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        record.save(&path).unwrap();
        let loaded = SandboxRecord::load(&path).unwrap();

        assert_eq!(loaded.provider, "local");
        assert_eq!(loaded.working_directory, "/tmp/work");
        assert!(loaded.identifier.is_none());
    }

    #[test]
    fn save_and_load_roundtrip_docker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.json");

        let record = SandboxRecord {
            provider: "docker".to_string(),
            working_directory: "/workspace".to_string(),
            identifier: Some("abc123container".to_string()),
            host_working_directory: Some("/home/user/project".to_string()),
            container_mount_point: Some("/workspace".to_string()),
            data_host: None,
        };
        record.save(&path).unwrap();
        let loaded = SandboxRecord::load(&path).unwrap();

        assert_eq!(loaded.provider, "docker");
        assert_eq!(loaded.identifier.as_deref(), Some("abc123container"));
        assert_eq!(
            loaded.host_working_directory.as_deref(),
            Some("/home/user/project")
        );
        assert_eq!(loaded.container_mount_point.as_deref(), Some("/workspace"));
        assert!(loaded.data_host.is_none());
    }

    #[test]
    fn save_and_load_roundtrip_exe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.json");

        let record = SandboxRecord {
            provider: "exe".to_string(),
            working_directory: "/home/exedev".to_string(),
            identifier: Some("my-vm".to_string()),
            host_working_directory: None,
            container_mount_point: None,
            data_host: Some("my-vm.exe.xyz".to_string()),
        };
        record.save(&path).unwrap();
        let loaded = SandboxRecord::load(&path).unwrap();

        assert_eq!(loaded.provider, "exe");
        assert_eq!(loaded.identifier.as_deref(), Some("my-vm"));
        assert_eq!(loaded.data_host.as_deref(), Some("my-vm.exe.xyz"));
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.json");

        let record = SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/work".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        record.save(&path).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("identifier").is_none());
        assert!(raw.get("host_working_directory").is_none());
        assert!(raw.get("container_mount_point").is_none());
        assert!(raw.get("data_host").is_none());
    }

    #[test]
    fn load_nonexistent_file() {
        let result = SandboxRecord::load(Path::new("/nonexistent/sandbox.json"));
        assert!(result.is_err());
    }
}
