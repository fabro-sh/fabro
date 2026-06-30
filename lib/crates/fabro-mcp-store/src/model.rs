//! Construction and persistence glue for [`McpServerDefinition`].
//!
//! The domain types (`McpServerDefinition`, `McpServerDraft`,
//! `McpServerReplace`, `McpServerId`, `McpServerRevision`) live in
//! `fabro-types` so they stay persistence-independent. This module owns the
//! store-side glue: validating, serializing to canonical TOML bytes, deriving
//! the revision, and reconstructing definitions from persisted bytes.

use std::path::PathBuf;

use fabro_types::settings::McpTransport;
use fabro_types::{
    McpServerDefinition, McpServerId, McpServerReplace, McpServerRevision, mcp_store,
};
use serde::{Deserialize, Serialize};

use crate::error::McpServerStoreError;

/// The on-disk body of a definition. Excludes `id`/`revision`, which are
/// derived from the filename and content hash rather than persisted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedMcpServer {
    display_name:         String,
    #[serde(default)]
    description:          Option<String>,
    transport:            McpTransport,
    startup_timeout_secs: u64,
    tool_timeout_secs:    u64,
}

#[derive(Serialize)]
struct PersistedMcpServerRef<'a> {
    display_name:         &'a str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description:          Option<&'a str>,
    transport:            &'a McpTransport,
    startup_timeout_secs: u64,
    tool_timeout_secs:    u64,
}

impl<'a> From<&'a McpServerReplace> for PersistedMcpServerRef<'a> {
    fn from(value: &'a McpServerReplace) -> Self {
        Self {
            display_name:         &value.display_name,
            description:          value.description.as_deref(),
            transport:            &value.transport,
            startup_timeout_secs: value.startup_timeout_secs,
            tool_timeout_secs:    value.tool_timeout_secs,
        }
    }
}

impl From<PersistedMcpServer> for McpServerReplace {
    fn from(value: PersistedMcpServer) -> Self {
        Self {
            display_name:         value.display_name,
            description:          value.description,
            transport:            value.transport,
            startup_timeout_secs: value.startup_timeout_secs,
            tool_timeout_secs:    value.tool_timeout_secs,
        }
    }
}

/// Build a definition + its canonical persisted bytes from a replace payload.
///
/// The revision is the SHA-256 of the freshly serialized canonical bytes, so a
/// caller can compare it to the on-disk content hash for optimistic
/// concurrency.
pub(crate) fn definition_from_replace(
    id: McpServerId,
    replace: McpServerReplace,
) -> Result<(McpServerDefinition, Vec<u8>), McpServerStoreError> {
    mcp_store::validate_mcp_server_fields(&replace)?;
    let bytes = canonical_bytes(&replace)?;
    let revision = McpServerRevision::from_bytes(&bytes);
    let definition = assemble(id, revision, replace);
    Ok((definition, bytes))
}

/// Reconstruct a definition from bytes loaded off disk, deriving the revision
/// from the raw file bytes (not a re-serialization).
pub(crate) fn definition_from_persisted_path(
    id: McpServerId,
    bytes: &[u8],
    path: impl Into<PathBuf>,
) -> Result<McpServerDefinition, McpServerStoreError> {
    let path = path.into();
    let revision = McpServerRevision::from_bytes(bytes);
    let persisted = parse_persisted(bytes, path)?;
    let replace = McpServerReplace::from(persisted);
    mcp_store::validate_mcp_server_fields(&replace)?;
    Ok(assemble(id, revision, replace))
}

fn assemble(
    id: McpServerId,
    revision: McpServerRevision,
    replace: McpServerReplace,
) -> McpServerDefinition {
    McpServerDefinition {
        id,
        revision,
        display_name: replace.display_name,
        description: replace.description,
        transport: replace.transport,
        startup_timeout_secs: replace.startup_timeout_secs,
        tool_timeout_secs: replace.tool_timeout_secs,
    }
}

pub(crate) fn canonical_bytes(replace: &McpServerReplace) -> Result<Vec<u8>, McpServerStoreError> {
    let persisted = PersistedMcpServerRef::from(replace);
    let toml = toml::to_string_pretty(&persisted)?;
    Ok(toml.into_bytes())
}

fn parse_persisted(bytes: &[u8], path: PathBuf) -> Result<PersistedMcpServer, McpServerStoreError> {
    let content = std::str::from_utf8(bytes)
        .map_err(|err| McpServerStoreError::invalid_utf8(path.clone(), err))?;
    toml::from_str(content).map_err(|err| McpServerStoreError::parse(path, err))
}
