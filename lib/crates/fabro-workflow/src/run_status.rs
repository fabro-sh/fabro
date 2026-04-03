use std::path::Path;

pub use fabro_types::status::{
    InvalidTransition, ParseRunStatusError, RunStatus, RunStatusRecord, StatusReason,
};

pub trait RunStatusRecordExt {
    fn save(&self, path: &Path) -> std::io::Result<()>;
    fn load(path: &Path) -> std::io::Result<Self>
    where
        Self: Sized;
}

impl RunStatusRecordExt for RunStatusRecord {
    fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
