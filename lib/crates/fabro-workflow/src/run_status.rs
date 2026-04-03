use std::path::Path;

pub use fabro_types::status::{
    InvalidTransition, ParseRunStatusError, RunStatus, RunStatusRecord, StatusReason,
};

pub trait RunStatusRecordExt {
    fn load(path: &Path) -> std::io::Result<Self>
    where
        Self: Sized;
}

impl RunStatusRecordExt for RunStatusRecord {
    fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
