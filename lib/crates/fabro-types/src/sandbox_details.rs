use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::RunSandbox;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxDetails {
    pub sandbox:      RunSandbox,
    pub state:        SandboxState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region:       Option<String>,
    pub resources:    SandboxResources,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels:       BTreeMap<String, String>,
    pub timestamps:   SandboxTimestamps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Unknown,
    Provisioning,
    Starting,
    Running,
    Stopping,
    Stopped,
    Paused,
    Deleting,
    Deleted,
    Archived,
    Restoring,
    Resizing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores:    Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes:   Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxTimestamps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at:       Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_with_snake_case_state() {
        let details = SandboxDetails {
            sandbox:      RunSandbox {
                provider: crate::SandboxProvider::Docker,
                image:    Some("ghcr.io/fabro/sandbox:latest".to_string()),
                snapshot: None,
                runtime:  Some(crate::RunSandboxRuntime {
                    id:                "container-abc123".to_string(),
                    working_directory: "/workspace".to_string(),
                    repo_cloned:       None,
                    clone_origin_url:  None,
                    clone_branch:      None,
                    workspace_root:    None,
                    repos_root:        None,
                    primary_repo_path: None,
                    primary_repo_link: None,
                }),
            },
            state:        SandboxState::Running,
            native_state: Some("running".to_string()),
            region:       None,
            resources:    SandboxResources {
                cpu_cores:    Some(2.0),
                memory_bytes: Some(4 * 1024 * 1024 * 1024),
                disk_bytes:   None,
            },
            labels:       BTreeMap::from([("run".to_string(), "abc".to_string())]),
            timestamps:   SandboxTimestamps {
                created_at:       Some(Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap()),
                last_activity_at: None,
            },
        };

        assert_eq!(
            serde_json::to_value(&details).unwrap(),
            json!({
                "sandbox": {
                    "provider": "docker",
                    "image": "ghcr.io/fabro/sandbox:latest",
                    "runtime": {
                        "id": "container-abc123",
                        "working_directory": "/workspace"
                    }
                },
                "state": "running",
                "native_state": "running",
                "resources": {
                    "cpu_cores": 2.0,
                    "memory_bytes": 4_294_967_296_u64,
                },
                "labels": {
                    "run": "abc"
                },
                "timestamps": {
                    "created_at": "2026-05-09T12:00:00Z"
                }
            })
        );
    }

    #[test]
    fn deserializes_with_minimal_fields() {
        let details: SandboxDetails = serde_json::from_value(json!({
            "sandbox": {
                "provider": "local",
                "image": null,
                "snapshot": null,
                "runtime": {
                    "id": "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z",
                    "working_directory": "/Users/client/project"
                }
            },
            "state": "unknown",
            "resources": {},
            "timestamps": {}
        }))
        .unwrap();

        assert_eq!(details.sandbox.provider, crate::SandboxProvider::Local);
        assert_eq!(
            details
                .sandbox
                .runtime
                .as_ref()
                .map(|runtime| runtime.id.as_str()),
            Some("local:01JNQVR7M0EJ5GKAT2SC4ERS1Z")
        );
        assert_eq!(
            details
                .sandbox
                .runtime
                .as_ref()
                .map(|runtime| runtime.working_directory.as_str()),
            Some("/Users/client/project")
        );
        assert_eq!(details.state, SandboxState::Unknown);
        assert!(details.sandbox.image.is_none());
        assert!(details.labels.is_empty());
        assert_eq!(details.resources, SandboxResources::default());
        assert_eq!(details.timestamps, SandboxTimestamps::default());
    }

    #[test]
    fn state_serializes_each_variant_in_snake_case() {
        fn check(state: SandboxState, expected: &str) {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(expected.to_string()),
            );
        }
        check(SandboxState::Unknown, "unknown");
        check(SandboxState::Provisioning, "provisioning");
        check(SandboxState::Starting, "starting");
        check(SandboxState::Running, "running");
        check(SandboxState::Stopping, "stopping");
        check(SandboxState::Stopped, "stopped");
        check(SandboxState::Paused, "paused");
        check(SandboxState::Deleting, "deleting");
        check(SandboxState::Deleted, "deleted");
        check(SandboxState::Archived, "archived");
        check(SandboxState::Restoring, "restoring");
        check(SandboxState::Resizing, "resizing");
        check(SandboxState::Error, "error");
    }
}
