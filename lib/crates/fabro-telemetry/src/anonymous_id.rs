use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use uuid::Uuid;

fn dot_id_path() -> std::path::PathBuf {
    fabro_util::Home::from_env().root().join(".id")
}

fn read_existing_id(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let id = contents.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Server: UUID persisted at ~/.fabro/.id
pub fn load_or_create_server_id() -> Result<String> {
    let path = dot_id_path();

    if let Some(id) = read_existing_id(&path) {
        return Ok(id);
    }

    let id = Uuid::new_v4().to_string();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    fs::write(&path, &id)
        .with_context(|| format!("failed to write anonymous id to {}", path.display()))?;

    Ok(id)
}

/// CLI: prefer existing ~/.fabro/.id, else MD5 of MAC address
pub fn compute_cli_id() -> Result<String> {
    if let Some(id) = read_existing_id(&dot_id_path()) {
        return Ok(id);
    }

    let mac = mac_address::get_mac_address()
        .context("failed to get MAC address")?
        .context("no MAC address found")?;

    let digest = md5::compute(mac.bytes());
    Ok(format!("{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_cli_id_returns_non_empty_string() {
        let id = compute_cli_id().unwrap();
        assert!(!id.is_empty());
    }

    #[test]
    fn load_or_create_server_id_returns_stable_uuid() {
        let id1 = load_or_create_server_id().unwrap();
        let id2 = load_or_create_server_id().unwrap();
        assert_eq!(id1, id2);
        Uuid::parse_str(&id1).unwrap();
    }
}
