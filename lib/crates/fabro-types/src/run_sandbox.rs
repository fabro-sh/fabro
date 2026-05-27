use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::SandboxProviderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSandboxKind {
    Planned,
    Initializing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxPlan {
    pub provider: SandboxProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxInstance {
    pub provider: SandboxProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub runtime:  RunSandboxRuntime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxFailure {
    pub provider:    String,
    pub error:       String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:      Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSandbox {
    kind:     RunSandboxKind,
    plan:     RunSandboxPlan,
    instance: Option<RunSandboxInstance>,
    failure:  Option<RunSandboxFailure>,
}

impl RunSandbox {
    pub fn planned(plan: RunSandboxPlan) -> Self {
        Self {
            kind: RunSandboxKind::Planned,
            plan,
            instance: None,
            failure: None,
        }
    }

    pub fn initializing(plan: RunSandboxPlan) -> Self {
        Self {
            kind: RunSandboxKind::Initializing,
            plan,
            instance: None,
            failure: None,
        }
    }

    pub fn ready(plan: RunSandboxPlan, instance: RunSandboxInstance) -> Self {
        Self {
            kind: RunSandboxKind::Ready,
            plan,
            instance: Some(instance),
            failure: None,
        }
    }

    pub fn failed(plan: RunSandboxPlan, failure: RunSandboxFailure) -> Self {
        Self {
            kind: RunSandboxKind::Failed,
            plan,
            instance: None,
            failure: Some(failure),
        }
    }

    pub fn instance(&self) -> Option<&RunSandboxInstance> {
        self.instance.as_ref()
    }

    pub fn into_instance(self) -> Option<RunSandboxInstance> {
        self.instance
    }

    pub fn kind(&self) -> RunSandboxKind {
        self.kind
    }

    pub fn plan(&self) -> &RunSandboxPlan {
        &self.plan
    }

    pub fn failure(&self) -> Option<&RunSandboxFailure> {
        self.failure.as_ref()
    }

    fn validate(&self) -> Result<(), String> {
        match self.kind {
            RunSandboxKind::Planned | RunSandboxKind::Initializing => {
                if self.instance.is_some() {
                    return Err(format!(
                        "{:?} sandbox must not carry an instance",
                        self.kind
                    ));
                }
                if self.failure.is_some() {
                    return Err(format!("{:?} sandbox must not carry a failure", self.kind));
                }
            }
            RunSandboxKind::Ready => {
                if self.instance.is_none() {
                    return Err("ready sandbox requires an instance".to_string());
                }
                if self.failure.is_some() {
                    return Err("ready sandbox must not carry a failure".to_string());
                }
            }
            RunSandboxKind::Failed => {
                if self.instance.is_some() {
                    return Err("failed sandbox must not carry an instance".to_string());
                }
                if self.failure.is_none() {
                    return Err("failed sandbox requires failure details".to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct RunSandboxWire {
    kind:     RunSandboxKind,
    plan:     RunSandboxPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<RunSandboxInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure:  Option<RunSandboxFailure>,
}

#[derive(Serialize)]
struct RunSandboxWireRef<'a> {
    kind:     RunSandboxKind,
    plan:     &'a RunSandboxPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<&'a RunSandboxInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure:  Option<&'a RunSandboxFailure>,
}

impl Serialize for RunSandbox {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        RunSandboxWireRef {
            kind:     self.kind,
            plan:     &self.plan,
            instance: self.instance.as_ref(),
            failure:  self.failure.as_ref(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RunSandbox {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RunSandboxWire::deserialize(deserializer)?;
        let sandbox = Self {
            kind:     wire.kind,
            plan:     wire.plan,
            instance: wire.instance,
            failure:  wire.failure,
        };
        sandbox.validate().map_err(D::Error::custom)?;
        Ok(sandbox)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxRuntime {
    pub id:                String,
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_cloned:       Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_origin_url:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_branch:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos_root:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_repo_link: Option<String>,
}
