//! A sandbox's status as Fabro's API shows it: a projection of what the
//! sandbox driver reports (`sandbox_driver::Status`), in the wire shape the
//! sandbox page reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sandbox_driver::{Isolation, NetworkPolicy, Resources, Status};
use serde::{Deserialize, Serialize};

/// What the sandbox driver reports about a sandbox. Only `id` and `state`
/// are always present; every other field is empty when the driver does not
/// report it.
///
/// TODO(parity): sandbox-driver's `Status` does not report the provider's
/// own state string, an error reason, the region, who owns a Host
/// workspace, or when the sandbox was created and last updated. Those
/// fields stay empty until it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub id:                  String,
    #[serde(default)]
    pub name:                Option<String>,
    pub state:               SandboxState,
    #[serde(default)]
    pub provider_state:      String,
    #[serde(default)]
    pub error_reason:        Option<String>,
    #[serde(default)]
    pub resources:           Option<SandboxResources>,
    #[serde(default)]
    pub sandbox_kind:        Option<SandboxKind>,
    #[serde(default)]
    pub region:              Option<String>,
    #[serde(default)]
    pub labels:              BTreeMap<String, String>,
    #[serde(default)]
    pub image:               Option<String>,
    #[serde(default)]
    pub snapshot:            Option<String>,
    #[serde(default)]
    pub network:             Option<SandboxNetworkPolicy>,
    #[serde(default)]
    pub workspace_ownership: Option<SandboxWorkspaceOwnership>,
    #[serde(default)]
    pub web_url:             Option<String>,
    #[serde(default)]
    pub created_at:          Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at:          Option<DateTime<Utc>>,
}

impl SandboxStatus {
    /// A status with the required fields; everything else empty.
    #[must_use]
    pub fn new(id: impl Into<String>, state: SandboxState) -> Self {
        Self {
            id: id.into(),
            name: None,
            state,
            provider_state: String::new(),
            error_reason: None,
            resources: None,
            sandbox_kind: None,
            region: None,
            labels: BTreeMap::new(),
            image: None,
            snapshot: None,
            network: None,
            workspace_ownership: None,
            web_url: None,
            created_at: None,
            updated_at: None,
        }
    }

    /// The driver's status for a sandbox whose provider has `isolation`.
    /// The driver's own state name is kept as `provider_state`, so a state
    /// the API folds (`unhealthy`, `lost`, `unavailable`) stays visible.
    #[must_use]
    pub fn from_driver(status: &Status, isolation: Option<Isolation>) -> Self {
        let mut projected = Self::new(status.reference.id.as_str(), status.state.into());
        projected.name.clone_from(&status.name);
        projected.provider_state = state_name(status.state);
        projected.resources = Some(SandboxResources::from(&status.resources));
        projected.sandbox_kind = isolation.and_then(SandboxKind::from_isolation);
        projected.labels.clone_from(&status.labels);
        projected.image.clone_from(&status.image);
        projected.snapshot = status.snapshot.as_ref().map(ToString::to_string);
        projected.network = status.network.as_ref().map(SandboxNetworkPolicy::from);
        projected.web_url.clone_from(&status.web_url);
        projected
    }
}

fn state_name(state: sandbox_driver::SandboxState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The sandbox's lifecycle state on the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Creating,
    Starting,
    Running,
    Stopping,
    Stopped,
    Pausing,
    Paused,
    Resuming,
    Archiving,
    Archived,
    Restoring,
    Resizing,
    Forking,
    Snapshotting,
    Deleting,
    Deleted,
    Error,
    #[serde(other)]
    Unknown,
}

impl From<sandbox_driver::SandboxState> for SandboxState {
    fn from(state: sandbox_driver::SandboxState) -> Self {
        use sandbox_driver::SandboxState as Driver;
        match state {
            Driver::Creating => Self::Creating,
            Driver::Starting => Self::Starting,
            Driver::Running => Self::Running,
            Driver::Stopping => Self::Stopping,
            Driver::Stopped => Self::Stopped,
            Driver::Paused => Self::Paused,
            Driver::Archived => Self::Archived,
            Driver::Deleting => Self::Deleting,
            Driver::Deleted => Self::Deleted,
            Driver::Unhealthy | Driver::Lost => Self::Error,
            Driver::Unavailable | Driver::Unknown => Self::Unknown,
        }
    }
}

/// The kind of isolation a sandbox's provider gives, from its descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxKind {
    Container,
    VirtualMachine,
    #[serde(other)]
    Unknown,
}

impl SandboxKind {
    /// `None` for a provider without isolation (Host).
    #[must_use]
    pub fn from_isolation(isolation: Isolation) -> Option<Self> {
        match isolation {
            Isolation::None => None,
            Isolation::Container => Some(Self::Container),
            Isolation::Vm => Some(Self::VirtualMachine),
            _ => Some(Self::Unknown),
        }
    }
}

/// Who owns a Host sandbox's workspace directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxWorkspaceOwnership {
    Designated,
    Managed,
}

/// A sandbox's compute resources, in the units the field names give.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxResources {
    pub cpu_cores: Option<u32>,
    pub memory_mb: Option<u64>,
    pub disk_mb:   Option<u64>,
    pub gpus:      Option<u32>,
}

impl From<&Resources> for SandboxResources {
    fn from(resources: &Resources) -> Self {
        Self {
            cpu_cores: resources.cpu_cores.map(u32::from),
            memory_mb: resources.memory_mb.map(u64::from),
            disk_mb:   resources.disk_mb.map(u64::from),
            gpus:      resources.gpus,
        }
    }
}

/// The network policy in force for a sandbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetworkPolicy {
    ProviderDefault,
    AllowAll,
    Block,
    CidrAllowList { cidrs: Vec<String> },
    DomainAllowList { domains: Vec<String> },
}

impl From<&NetworkPolicy> for SandboxNetworkPolicy {
    fn from(policy: &NetworkPolicy) -> Self {
        match policy {
            NetworkPolicy::ProviderDefault => Self::ProviderDefault,
            NetworkPolicy::AllowAll => Self::AllowAll,
            NetworkPolicy::Block => Self::Block,
            NetworkPolicy::CidrAllowList { cidrs } => Self::CidrAllowList {
                cidrs: cidrs.clone(),
            },
            NetworkPolicy::DomainAllowList { domains } => Self::DomainAllowList {
                domains: domains.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use sandbox_driver::{NamespaceId, ProviderName, SandboxId, SandboxRef};
    use serde_json::json;

    use super::*;

    fn driver_status(state: sandbox_driver::SandboxState) -> Status {
        serde_json::from_value(json!({
            "reference": {
                "provider": "docker",
                "namespace": "local",
                "id": "container-abc123"
            },
            "state": state,
            "labels": { "petri.run": "run-1" },
            "image": "ghcr.io/fabro/sandbox:latest",
            "resources": { "cpu_cores": 2, "memory_mb": 4096 },
            "network": { "mode": "domain_allow_list", "allow": ["github.com"] }
        }))
        .unwrap()
    }

    #[test]
    fn the_drivers_status_projects_onto_the_api_shape() {
        let status = driver_status(sandbox_driver::SandboxState::Running);
        assert_eq!(status.reference, SandboxRef {
            provider:  ProviderName::try_new("docker").unwrap(),
            namespace: NamespaceId::try_new("local").unwrap(),
            id:        SandboxId::try_new("container-abc123").unwrap(),
        });
        let value = serde_json::to_value(SandboxStatus::from_driver(
            &status,
            Some(Isolation::Container),
        ))
        .unwrap();
        assert_eq!(value["id"], "container-abc123");
        assert_eq!(value["state"], "running");
        assert_eq!(value["provider_state"], "running");
        assert_eq!(value["sandbox_kind"], "container");
        assert_eq!(value["labels"]["petri.run"], "run-1");
        assert_eq!(value["resources"]["cpu_cores"], 2);
        assert_eq!(value["resources"]["memory_mb"], 4096);
        assert_eq!(value["resources"]["disk_mb"], json!(null));
        assert_eq!(
            value["network"],
            json!({ "domain_allow_list": { "domains": ["github.com"] } })
        );
        assert_eq!(value["created_at"], json!(null));
    }

    #[test]
    fn folded_states_keep_the_drivers_name() {
        let projected = SandboxStatus::from_driver(
            &driver_status(sandbox_driver::SandboxState::Lost),
            Some(Isolation::None),
        );
        assert_eq!(projected.state, SandboxState::Error);
        assert_eq!(projected.provider_state, "lost");
        assert_eq!(projected.sandbox_kind, None);
    }

    #[test]
    fn a_status_with_only_its_required_fields_decodes() {
        let status: SandboxStatus =
            serde_json::from_value(json!({ "id": "host-dir-2f746d70", "state": "running" }))
                .unwrap();
        assert_eq!(
            status,
            SandboxStatus::new("host-dir-2f746d70", SandboxState::Running)
        );
    }
}
