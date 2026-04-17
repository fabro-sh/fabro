use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rand::TryRngCore;
use rand::rngs::OsRng;

pub const DEV_TOKEN_PREFIX: &str = "fabro_dev_";
const DEV_TOKEN_RANDOM_BYTES: usize = 32;
const DEV_TOKEN_HEX_LEN: usize = DEV_TOKEN_RANDOM_BYTES * 2;
const DEV_TOKEN_LEN: usize = DEV_TOKEN_PREFIX.len() + DEV_TOKEN_HEX_LEN;

pub fn generate_dev_token() -> String {
    let mut bytes = [0_u8; DEV_TOKEN_RANDOM_BYTES];
    OsRng.try_fill_bytes(&mut bytes).expect("OS RNG");

    let mut token = String::with_capacity(DEV_TOKEN_LEN);
    token.push_str(DEV_TOKEN_PREFIX);
    for byte in bytes {
        use std::fmt::Write as _;

        let _ = write!(&mut token, "{byte:02x}");
    }
    token
}

pub fn validate_dev_token_format(token: &str) -> bool {
    let Some(hex) = token.strip_prefix(DEV_TOKEN_PREFIX) else {
        return false;
    };

    token.len() == DEV_TOKEN_LEN
        && hex.len() == DEV_TOKEN_HEX_LEN
        && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn read_dev_token_file(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| validate_dev_token_format(token))
}

pub fn load_or_create_dev_token(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim().to_string();
            if validate_dev_token_format(&token) {
                return Ok(token);
            }
            return Err(anyhow!("invalid dev token format in {}", path.display()));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(anyhow::Error::from(err))
                .with_context(|| format!("read dev token {}", path.display()));
        }
    }

    let token = generate_dev_token();
    atomic_write_private(path, &token)?;
    Ok(token)
}

pub fn write_dev_token(path: &Path, token: &str) -> Result<()> {
    if !validate_dev_token_format(token) {
        return Err(anyhow!("invalid dev token format for {}", path.display()));
    }

    atomic_write_private(path, token)
}

fn atomic_write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }

    let temp_path = path.with_file_name(format!(
        ".{}.tmp-{:x}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dev-token"),
        rand::random::<u64>()
    ));
    write_private_file(&temp_path, contents)?;
    fs::rename(&temp_path, path)
        .with_context(|| format!("rename {} to {}", temp_path.display(), path.display()))?;
    Ok(())
}

fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    let mut file = {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open {} for writing", path.display()))?
    };

    #[cfg(not(unix))]
    let mut file =
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;

    file.write_all(contents.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    Ok(())
}
