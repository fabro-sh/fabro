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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:      Option<String>,
    pub resources:    SandboxResources,
    #[serde(default)]
    pub network:      SandboxNetwork,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxNetwork {
    pub egress:  SandboxNetworkPolicy,
    pub ingress: SandboxNetworkPolicy,
}

impl SandboxNetwork {
    pub fn unknown() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxNetworkPolicy {
    pub allow: SandboxNetworkRuleSet,
    pub block: SandboxNetworkRuleSet,
}

impl SandboxNetworkPolicy {
    pub fn unknown() -> Self {
        Self::default()
    }

    pub fn open() -> Self {
        Self {
            allow: SandboxNetworkRuleSet::all(),
            block: SandboxNetworkRuleSet::none(),
        }
    }

    pub fn blocked() -> Self {
        Self {
            allow: SandboxNetworkRuleSet::none(),
            block: SandboxNetworkRuleSet::all(),
        }
    }

    pub fn allow_cidrs<I, S>(cidrs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allow: SandboxNetworkRuleSet::cidrs(cidrs),
            block: SandboxNetworkRuleSet::all(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxNetworkRuleSet {
    pub mode:  SandboxNetworkRuleSetMode,
    pub cidrs: Vec<String>,
}

impl SandboxNetworkRuleSet {
    pub fn unknown() -> Self {
        Self {
            mode:  SandboxNetworkRuleSetMode::Unknown,
            cidrs: Vec::new(),
        }
    }

    pub fn none() -> Self {
        Self {
            mode:  SandboxNetworkRuleSetMode::None,
            cidrs: Vec::new(),
        }
    }

    pub fn all() -> Self {
        Self {
            mode:  SandboxNetworkRuleSetMode::All,
            cidrs: Vec::new(),
        }
    }

    pub fn cidrs<I, S>(cidrs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            mode:  SandboxNetworkRuleSetMode::Cidrs,
            cidrs: cidrs.into_iter().map(Into::into).collect(),
        }
    }

    pub fn essentials() -> Self {
        Self {
            mode:  SandboxNetworkRuleSetMode::Essentials,
            cidrs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetworkRuleSetMode {
    #[default]
    Unknown,
    None,
    All,
    Cidrs,
    Essentials,
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
            web_url:      Some(
                "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
                    .to_string(),
            ),
            resources:    SandboxResources {
                cpu_cores:    Some(2.0),
                memory_bytes: Some(4 * 1024 * 1024 * 1024),
                disk_bytes:   None,
            },
            network:      SandboxNetwork {
                egress:  SandboxNetworkPolicy {
                    allow: SandboxNetworkRuleSet::cidrs(["10.0.0.0/8"]),
                    block: SandboxNetworkRuleSet::all(),
                },
                ingress: SandboxNetworkPolicy::unknown(),
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
                "web_url": "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
                "resources": {
                    "cpu_cores": 2.0,
                    "memory_bytes": 4_294_967_296_u64,
                },
                "network": {
                    "egress": {
                        "allow": {
                            "mode": "cidrs",
                            "cidrs": ["10.0.0.0/8"]
                        },
                        "block": {
                            "mode": "all",
                            "cidrs": []
                        }
                    },
                    "ingress": {
                        "allow": {
                            "mode": "unknown",
                            "cidrs": []
                        },
                        "block": {
                            "mode": "unknown",
                            "cidrs": []
                        }
                    }
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
        assert_eq!(details.network, SandboxNetwork::unknown());
        assert_eq!(details.timestamps, SandboxTimestamps::default());
    }

    #[test]
    fn network_rule_set_helpers_cover_supported_modes() {
        assert_eq!(SandboxNetworkRuleSet::unknown(), SandboxNetworkRuleSet {
            mode:  SandboxNetworkRuleSetMode::Unknown,
            cidrs: Vec::new(),
        });
        assert_eq!(SandboxNetworkRuleSet::none(), SandboxNetworkRuleSet {
            mode:  SandboxNetworkRuleSetMode::None,
            cidrs: Vec::new(),
        });
        assert_eq!(SandboxNetworkRuleSet::all(), SandboxNetworkRuleSet {
            mode:  SandboxNetworkRuleSetMode::All,
            cidrs: Vec::new(),
        });
        assert_eq!(
            SandboxNetworkRuleSet::cidrs(["192.168.0.0/16", "10.0.0.0/8"]),
            SandboxNetworkRuleSet {
                mode:  SandboxNetworkRuleSetMode::Cidrs,
                cidrs: vec!["192.168.0.0/16".to_string(), "10.0.0.0/8".to_string()],
            }
        );
        assert_eq!(SandboxNetworkRuleSet::essentials(), SandboxNetworkRuleSet {
            mode:  SandboxNetworkRuleSetMode::Essentials,
            cidrs: Vec::new(),
        });
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
