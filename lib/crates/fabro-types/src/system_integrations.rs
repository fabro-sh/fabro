use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemIntegrationsResponse {
    pub data: Vec<SystemIntegrationStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemIntegrationStatus {
    pub provider:            IntegrationProvider,
    pub enabled:             bool,
    pub configured:          bool,
    pub status:              IntegrationStatus,
    pub missing_credentials: Vec<String>,
    pub connection:          Option<IntegrationConnectionStatus>,
    #[serde(default)]
    pub metadata:            BTreeMap<String, String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum IntegrationProvider {
    Github,
    Slack,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum IntegrationStatus {
    Disabled,
    MissingCredentials,
    Configured,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationConnectionStatus {
    pub kind:              IntegrationConnectionKind,
    pub status:            IntegrationConnectionState,
    pub last_connected_at: Option<DateTime<Utc>>,
    pub last_error:        Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum IntegrationConnectionKind {
    SocketMode,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum IntegrationConnectionState {
    Connecting,
    Connected,
    Error,
}
