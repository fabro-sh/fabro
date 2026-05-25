//! Temporary compatibility shim for pre-token/oauth vault files.
//!
//! Delete this module after 2026-08-18, once supported installs have had a
//! release window to rewrite `credential` / `environment` entries to the
//! `oauth` / `token` schemas.

#![expect(
    clippy::disallowed_methods,
    reason = "Temporary startup migration uses synchronous vault file I/O before serving requests."
)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use fabro_auth::{OAuthConfig, OAuthCredential, OAuthTokens};
use serde::Deserialize;
use serde_json::{Map, Value};

pub(crate) const REMOVAL_DEADLINE: &str = "2026-08-18";

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyVaultMigrationReport {
    pub(crate) migrated_entries: usize,
    pub(crate) skipped_entries:  usize,
    pub(crate) backup_path:      Option<PathBuf>,
}

impl LegacyVaultMigrationReport {
    pub(crate) fn changed(&self) -> bool {
        self.migrated_entries > 0 || self.skipped_entries > 0
    }
}

#[derive(Debug, Deserialize)]
struct LegacyAuthCredential {
    provider: String,
    #[serde(flatten)]
    details:  LegacyAuthDetails,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LegacyAuthDetails {
    ApiKey {
        key: String,
    },
    CodexOauth {
        tokens:     OAuthTokens,
        config:     OAuthConfig,
        #[serde(default)]
        account_id: Option<String>,
    },
}

pub(crate) fn migrate_legacy_vault_file(path: &Path) -> anyhow::Result<LegacyVaultMigrationReport> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LegacyVaultMigrationReport::default());
        }
        Err(err) => return Err(err).with_context(|| format!("read vault {}", path.display())),
    };
    let entries = parse_vault_entries(&contents)?;
    let (next_entries, migrated_entries, skipped_entries) = rewrite_entries(entries);
    let mut report = LegacyVaultMigrationReport {
        migrated_entries,
        skipped_entries,
        backup_path: None,
    };
    if !report.changed() {
        return Ok(report);
    }

    let backup_path = backup_vault_file(path)?;
    write_vault_entries(path, &next_entries)?;
    report.backup_path = Some(backup_path);
    Ok(report)
}

fn parse_vault_entries(contents: &str) -> anyhow::Result<Map<String, Value>> {
    let value: Value = serde_json::from_str(contents).context("parse vault JSON")?;
    match value {
        Value::Object(entries) => Ok(entries),
        _ => bail!("vault JSON root must be an object"),
    }
}

fn rewrite_entries(entries: Map<String, Value>) -> (Map<String, Value>, usize, usize) {
    let mut next_entries = Map::new();
    let mut occupied = HashSet::new();
    for (name, entry) in &entries {
        if matches!(
            entry.get("type").and_then(Value::as_str),
            Some("token" | "oauth" | "file")
        ) {
            next_entries.insert(name.clone(), entry.clone());
            occupied.insert(name.clone());
        }
    }

    let mut migrated_entries = 0;
    let mut skipped_entries = 0;
    for (name, entry) in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("token" | "oauth" | "file") => {}
            Some("environment") => {
                if insert_rewritten_entry(
                    &mut next_entries,
                    &mut occupied,
                    name,
                    rewrite_entry(entry, "token", None),
                ) {
                    migrated_entries += 1;
                } else {
                    skipped_entries += 1;
                }
            }
            Some("credential") => match legacy_credential_entry(&name, &entry) {
                Some((target_name, rewritten)) => {
                    if insert_rewritten_entry(
                        &mut next_entries,
                        &mut occupied,
                        target_name,
                        rewritten,
                    ) {
                        migrated_entries += 1;
                    } else {
                        skipped_entries += 1;
                    }
                }
                None => skipped_entries += 1,
            },
            _ => skipped_entries += 1,
        }
    }

    (next_entries, migrated_entries, skipped_entries)
}

fn insert_rewritten_entry(
    entries: &mut Map<String, Value>,
    occupied: &mut HashSet<String>,
    name: String,
    entry: Value,
) -> bool {
    if !occupied.insert(name.clone()) {
        return false;
    }
    entries.insert(name, entry);
    true
}

fn legacy_credential_entry(name: &str, entry: &Value) -> Option<(String, Value)> {
    let value = entry.get("value").and_then(Value::as_str)?;
    let credential: LegacyAuthCredential = serde_json::from_str(value).ok()?;
    match credential.details {
        LegacyAuthDetails::ApiKey { key } => {
            let target_name = api_key_secret_name(&credential.provider)?;
            Some((
                target_name,
                rewrite_entry(entry.clone(), "token", Some(key)),
            ))
        }
        LegacyAuthDetails::CodexOauth {
            tokens,
            config,
            account_id,
        } if credential.provider == "openai" && name == "openai_codex" => {
            let credential = OAuthCredential {
                tokens,
                config,
                account_id,
            };
            let value = serde_json::to_string(&credential).ok()?;
            Some((
                "OPENAI_CODEX".to_string(),
                rewrite_entry(entry.clone(), "oauth", Some(value)),
            ))
        }
        LegacyAuthDetails::CodexOauth { .. } => None,
    }
}

fn rewrite_entry(mut entry: Value, secret_type: &str, value: Option<String>) -> Value {
    if let Value::Object(fields) = &mut entry {
        fields.insert("type".to_string(), Value::String(secret_type.to_string()));
        if let Some(value) = value {
            fields.insert("value".to_string(), Value::String(value));
        }
    }
    entry
}

fn api_key_secret_name(provider: &str) -> Option<String> {
    let mut name = String::new();
    for ch in provider.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    while name.ends_with('_') {
        name.pop();
    }
    if name.is_empty() {
        return None;
    }
    if !name.ends_with("_API_KEY") {
        name.push_str("_API_KEY");
    }
    Some(name)
}

fn backup_vault_file(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secrets.json");
    let backup_path = parent.join(format!(
        ".{file_name}.legacy-vault-migration-{}.bak",
        ulid::Ulid::new()
    ));
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "copy vault {} to backup {}",
            path.display(),
            backup_path.display()
        )
    })?;
    set_private_permissions(&backup_path)?;
    Ok(backup_path)
}

fn write_vault_entries(path: &Path, entries: &Map<String, Value>) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create vault directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secrets.json");
    let tmp_path = parent.join(format!(
        ".{file_name}.legacy-vault-migration-tmp-{}",
        ulid::Ulid::new()
    ));
    let json = serde_json::to_vec_pretty(entries).context("serialize migrated vault JSON")?;
    std::fs::write(&tmp_path, json)
        .with_context(|| format!("write migrated vault temp file {}", tmp_path.display()))?;
    set_private_permissions(&tmp_path)?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "rename migrated vault temp file {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set private permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
