//! Server-managed MCP server catalog domain model.
//!
//! These types describe MCP server definitions that are stored once on a Fabro
//! server and later referenced by name from workflow configs. They are
//! persistence-independent: the durable storage lives in the `fabro-mcp-store`
//! crate, which derives `id` (filename stem) and `revision` (content hash) and
//! never persists them inside the TOML body.
//!
//! Transport is the existing [`McpTransport`](crate::settings::McpTransport)
//! reused verbatim, so a stored definition uses the same `stdio`/`http`/
//! `sandbox` shape as inline MCP config.
//!
//! Read APIs do not return [`McpServerDefinition`] directly. They return
//! [`McpServerView`], a value-omitting projection whose transport carries only
//! the env/header *names* ([`McpTransportView`]) so stored secret-bearing
//! values are never sent back to a client.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::settings::McpTransport;
use crate::settings::run::McpHttpProtocol;

/// A server-managed MCP server definition.
///
/// `id` and `revision` are derived (filename stem + content hash of the
/// persisted TOML bytes) and are not stored in the persisted TOML body. This is
/// the internal/persistence model and carries full transport values; it is not
/// serialized to clients. Read APIs return [`McpServerView`] instead, which
/// omits env/header values.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerDefinition {
    pub id:                   McpServerId,
    pub revision:             McpServerRevision,
    pub display_name:         String,
    pub description:          Option<String>,
    pub transport:            McpTransport,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

/// Fields supplied when creating a new definition. Carries an `id` (the create
/// call assigns the filename) but no `revision` (the store derives it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerDraft {
    pub id:                   McpServerId,
    pub display_name:         String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description:          Option<String>,
    pub transport:            McpTransport,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

/// Fields supplied when replacing an existing definition. The id is fixed by
/// the path and the revision is supplied separately for optimistic concurrency,
/// so neither appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerReplace {
    pub display_name:         String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description:          Option<String>,
    pub transport:            McpTransport,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

impl From<McpServerDraft> for (McpServerId, McpServerReplace) {
    fn from(value: McpServerDraft) -> Self {
        (value.id, McpServerReplace {
            display_name:         value.display_name,
            description:          value.description,
            transport:            value.transport,
            startup_timeout_secs: value.startup_timeout_secs,
            tool_timeout_secs:    value.tool_timeout_secs,
        })
    }
}

/// The read-side projection of a definition returned by catalog read APIs.
///
/// Identical to [`McpServerDefinition`] except that the transport is the
/// value-omitting [`McpTransportView`], so stored env/header values never leave
/// the server. This is the wire type behind the OpenAPI `McpServer` schema
/// (reused by `fabro-api` via `with_replacement`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerView {
    pub id:                   McpServerId,
    pub revision:             McpServerRevision,
    pub display_name:         String,
    pub description:          Option<String>,
    pub transport:            McpTransportView,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

impl From<McpServerDefinition> for McpServerView {
    fn from(value: McpServerDefinition) -> Self {
        Self {
            id:                   value.id,
            revision:             value.revision,
            display_name:         value.display_name,
            description:          value.description,
            transport:            value.transport.into(),
            startup_timeout_secs: value.startup_timeout_secs,
            tool_timeout_secs:    value.tool_timeout_secs,
        }
    }
}

impl From<&McpServerDefinition> for McpServerView {
    fn from(value: &McpServerDefinition) -> Self {
        Self {
            id:                   value.id.clone(),
            revision:             value.revision.clone(),
            display_name:         value.display_name.clone(),
            description:          value.description.clone(),
            transport:            (&value.transport).into(),
            startup_timeout_secs: value.startup_timeout_secs,
            tool_timeout_secs:    value.tool_timeout_secs,
        }
    }
}

/// The read-side projection of [`McpTransport`]. It mirrors the transport shape
/// but replaces the `env`/`headers` value maps with sorted `env_keys`/
/// `header_keys` name lists, so secret-bearing values are never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportView {
    Stdio {
        command:  Vec<String>,
        env_keys: Vec<String>,
    },
    Http {
        #[serde(default)]
        protocol:    McpHttpProtocol,
        url:         String,
        header_keys: Vec<String>,
    },
    Sandbox {
        #[serde(default)]
        protocol: McpHttpProtocol,
        command:  Vec<String>,
        port:     u16,
        env_keys: Vec<String>,
    },
}

impl From<McpTransport> for McpTransportView {
    fn from(value: McpTransport) -> Self {
        match value {
            McpTransport::Stdio { command, env } => Self::Stdio {
                command,
                env_keys: sorted_keys(env),
            },
            McpTransport::Http {
                protocol,
                url,
                headers,
            } => Self::Http {
                protocol,
                url,
                header_keys: sorted_keys(headers),
            },
            McpTransport::Sandbox {
                protocol,
                command,
                port,
                env,
            } => Self::Sandbox {
                protocol,
                command,
                port,
                env_keys: sorted_keys(env),
            },
        }
    }
}

impl From<&McpTransport> for McpTransportView {
    fn from(value: &McpTransport) -> Self {
        match value {
            McpTransport::Stdio { command, env } => Self::Stdio {
                command:  command.clone(),
                env_keys: sorted_keys_ref(env),
            },
            McpTransport::Http {
                protocol,
                url,
                headers,
            } => Self::Http {
                protocol:    *protocol,
                url:         url.clone(),
                header_keys: sorted_keys_ref(headers),
            },
            McpTransport::Sandbox {
                protocol,
                command,
                port,
                env,
            } => Self::Sandbox {
                protocol: *protocol,
                command:  command.clone(),
                port:     *port,
                env_keys: sorted_keys_ref(env),
            },
        }
    }
}

/// Collect a value map's keys into a sorted list, so the projection is
/// deterministic regardless of the map's iteration order.
fn sorted_keys(map: HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = map.into_keys().collect();
    keys.sort();
    keys
}

fn sorted_keys_ref(map: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

/// Validation errors for MCP server domain fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerValidationError {
    InvalidMcpServerId { value: String },
    EmptyName,
    InvalidTransport { reason: String },
}

impl fmt::Display for McpServerValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMcpServerId { value } => {
                write!(
                    f,
                    "mcp server id {value:?} must match [a-z0-9][a-z0-9-]{{0,62}}"
                )
            }
            Self::EmptyName => f.write_str("mcp server display name must not be empty"),
            Self::InvalidTransport { reason } => {
                write!(f, "mcp server transport is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for McpServerValidationError {}

/// An MCP server id: lowercase, matches `^[a-z0-9][a-z0-9-]{0,62}$`, and equals
/// the persisted file's stem.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct McpServerId(String);

impl McpServerId {
    pub fn new(value: impl Into<String>) -> Result<Self, McpServerValidationError> {
        let value = value.into();
        if is_valid_mcp_server_id(&value) {
            Ok(Self(value))
        } else {
            Err(McpServerValidationError::InvalidMcpServerId { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for McpServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for McpServerId {
    type Err = McpServerValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for McpServerId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for McpServerId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

/// A revision: the lowercase SHA-256 hex of a definition's canonical persisted
/// TOML bytes. Used as an ETag for optimistic concurrency.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct McpServerRevision(String);

impl McpServerRevision {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for McpServerRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for McpServerRevision {
    type Err = McpServerRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_string()))
        } else {
            Err(McpServerRevisionParseError(value.to_string()))
        }
    }
}

impl Serialize for McpServerRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for McpServerRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerRevisionParseError(String);

impl fmt::Display for McpServerRevisionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid mcp server revision: {:?}", self.0)
    }
}

impl std::error::Error for McpServerRevisionParseError {}

fn is_valid_mcp_server_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    if value.len() > 63 {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Validate the structural invariants of a definition's fields.
///
/// Scope is intentionally structural for now: id format (enforced by
/// [`McpServerId`]), non-empty name, and a well-formed transport. It does not
/// reject credential-looking literal values in env vars or HTTP headers.
pub fn validate_mcp_server_fields(
    replace: &McpServerReplace,
) -> Result<(), McpServerValidationError> {
    if replace.display_name.trim().is_empty() {
        return Err(McpServerValidationError::EmptyName);
    }
    validate_transport(&replace.transport)
}

fn validate_transport(transport: &McpTransport) -> Result<(), McpServerValidationError> {
    match transport {
        McpTransport::Stdio { command, .. } | McpTransport::Sandbox { command, .. } => {
            if command
                .first()
                .is_none_or(|program| program.trim().is_empty())
            {
                return Err(McpServerValidationError::InvalidTransport {
                    reason: "command program must not be empty".to_string(),
                });
            }
        }
        McpTransport::Http { url, .. } => {
            if url.trim().is_empty() {
                return Err(McpServerValidationError::InvalidTransport {
                    reason: "url must not be empty".to_string(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        McpServerId, McpServerReplace, McpServerRevision, McpTransport, validate_mcp_server_fields,
    };
    use crate::settings::run::McpHttpProtocol;

    fn http_transport() -> McpTransport {
        McpTransport::Http {
            protocol: McpHttpProtocol::default(),
            url:      "https://example.com/mcp".to_string(),
            headers:  HashMap::new(),
        }
    }

    #[test]
    fn mcp_server_id_validation_matches_contract() {
        assert!("a".parse::<McpServerId>().is_ok());
        assert!("a-1".parse::<McpServerId>().is_ok());
        assert!("0".parse::<McpServerId>().is_ok());
        assert!("sentry-dev".parse::<McpServerId>().is_ok());
        assert!("A".parse::<McpServerId>().is_err());
        assert!("a_1".parse::<McpServerId>().is_err());
        assert!("-a".parse::<McpServerId>().is_err());
        assert!("".parse::<McpServerId>().is_err());
        assert!("a".repeat(64).parse::<McpServerId>().is_err());
    }

    #[test]
    fn revision_is_lowercase_sha256_hex() {
        let revision = McpServerRevision::from_bytes(b"hello");
        assert_eq!(
            revision.to_string(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(revision.to_string().parse::<McpServerRevision>().is_ok());
        assert!("ABC".parse::<McpServerRevision>().is_err());
    }

    #[test]
    fn validation_rejects_empty_name() {
        let replace = McpServerReplace {
            display_name:         "  ".to_string(),
            description:          None,
            transport:            http_transport(),
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        };
        assert!(validate_mcp_server_fields(&replace).is_err());
    }

    #[test]
    fn validation_rejects_empty_transport_command() {
        let replace = McpServerReplace {
            display_name:         "Local".to_string(),
            description:          None,
            transport:            McpTransport::Stdio {
                command: Vec::new(),
                env:     HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        };
        assert!(validate_mcp_server_fields(&replace).is_err());
    }

    #[test]
    fn validation_rejects_blank_transport_program() {
        let replace = McpServerReplace {
            display_name:         "Local".to_string(),
            description:          None,
            transport:            McpTransport::Stdio {
                command: vec![" ".to_string(), "--arg".to_string()],
                env:     HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        };
        assert!(validate_mcp_server_fields(&replace).is_err());
    }

    #[test]
    fn validation_accepts_well_formed_definition() {
        let replace = McpServerReplace {
            display_name:         "Sentry".to_string(),
            description:          Some("Issue tracker".to_string()),
            transport:            http_transport(),
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        };
        assert!(validate_mcp_server_fields(&replace).is_ok());
    }
}
