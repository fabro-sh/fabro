//! Construction and persistence glue for [`McpServerDefinition`].
//!
//! The domain types (`McpServerDefinition`, `McpServerDraft`,
//! `McpServerReplace`, `McpServerId`, `McpServerRevision`,
//! `PersistedMcpServer`) live in `fabro-types` so they stay
//! persistence-independent. This module owns the store-side glue: validating,
//! serializing to canonical TOML bytes, deriving the revision, and
//! reconstructing definitions from persisted bytes.

use std::path::PathBuf;

use fabro_types::{
    McpServerDefinition, McpServerId, McpServerReplace, McpServerRevision, PersistedMcpServer,
    mcp_store,
};

use crate::error::McpServerStoreError;

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
    let persisted = PersistedMcpServer::from(replace.clone());
    let bytes = canonical_bytes(&persisted)?;
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
        name: replace.name,
        description: replace.description,
        transport: replace.transport,
        startup_timeout_secs: replace.startup_timeout_secs,
        tool_timeout_secs: replace.tool_timeout_secs,
    }
}

pub(crate) fn canonical_bytes(
    persisted: &PersistedMcpServer,
) -> Result<Vec<u8>, McpServerStoreError> {
    let toml = toml::to_string_pretty(persisted)?;
    Ok(toml.into_bytes())
}

fn parse_persisted(bytes: &[u8], path: PathBuf) -> Result<PersistedMcpServer, McpServerStoreError> {
    let content = std::str::from_utf8(bytes)
        .map_err(|err| McpServerStoreError::invalid_utf8(path.clone(), err))?;
    toml::from_str(content).map_err(|err| McpServerStoreError::parse(path, err))
}
