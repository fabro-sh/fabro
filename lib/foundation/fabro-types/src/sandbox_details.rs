use sandbox_driver::SandboxStatus;
use serde::{Deserialize, Serialize};

use crate::RunSandboxInstance;

/// The sandbox owned by a run: fabro's record of it, and the status the
/// sandbox driver reports for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxDetails {
    pub sandbox: RunSandboxInstance,
    pub status:  SandboxStatus,
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use chrono::DateTime;
    use sandbox_driver::{SandboxId, SandboxState};
    use serde_json::json;

    use super::*;
    use crate::{RunSandboxRuntime, SandboxProviderKind};

    #[test]
    fn details_carry_the_record_and_the_drivers_status() {
        let mut status = SandboxStatus::new(
            SandboxId::try_new("container-abc123").unwrap(),
            SandboxState::Running,
        );
        status.provider_state = "running".to_string();
        status.image = Some("ghcr.io/fabro/sandbox:latest".to_string());
        status.created_at = Some(SystemTime::from(
            DateTime::parse_from_rfc3339("2026-05-09T12:00:00Z").unwrap(),
        ));
        let details = SandboxDetails {
            sandbox: RunSandboxInstance {
                provider:          SandboxProviderKind::DOCKER,
                image:             Some("ghcr.io/fabro/sandbox:latest".to_string()),
                snapshot:          None,
                runtime:           RunSandboxRuntime {
                    id:                "container-abc123".to_string(),
                    working_directory: "/workspace".to_string(),
                    repo_cloned:       None,
                    clone_origin_url:  None,
                    clone_branch:      None,
                    workspace_root:    None,
                    repos_root:        None,
                    primary_repo_path: None,
                    primary_repo_link: None,
                },
                ready_duration_ms: None,
                retained:          None,
            },
            status,
        };

        let value = serde_json::to_value(&details).unwrap();
        assert_eq!(value["sandbox"]["provider"], "docker");
        assert_eq!(value["sandbox"]["runtime"]["id"], "container-abc123");
        assert_eq!(value["status"]["id"], "container-abc123");
        assert_eq!(value["status"]["state"], "running");
        assert_eq!(value["status"]["image"], "ghcr.io/fabro/sandbox:latest");
        assert_eq!(value["status"]["snapshot"], json!(null));
        assert_eq!(value["status"]["created_at"], "2026-05-09T12:00:00Z");

        let decoded: SandboxDetails = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.status.state, SandboxState::Running);
        assert_eq!(decoded.status.provider_state, "running");
    }

    #[test]
    fn a_status_with_only_its_required_fields_decodes() {
        let details: SandboxDetails = serde_json::from_value(json!({
            "sandbox": {
                "provider": "local",
                "runtime": {
                    "id": "host-dir-2f746d70",
                    "working_directory": "/tmp"
                }
            },
            "status": { "id": "host-dir-2f746d70", "state": "running" }
        }))
        .unwrap();
        assert_eq!(details.sandbox.provider, SandboxProviderKind::LOCAL);
        assert_eq!(details.status.id.as_str(), "host-dir-2f746d70");
        assert!(details.status.labels.is_empty());
        assert!(details.status.created_at.is_none());
    }
}
