use std::path::Path;

pub use fabro_types::sandbox_record::SandboxRecord;

pub trait SandboxRecordExt {
    fn save(&self, path: &Path) -> anyhow::Result<()>;
    fn load(path: &Path) -> anyhow::Result<Self>
    where
        Self: Sized;
}

impl SandboxRecordExt for SandboxRecord {
    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("sandbox_record serialize failed: {e}"))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    fn load(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&data)
            .map_err(|e| anyhow::anyhow!("sandbox_record deserialize failed: {e}"))
    }
}
