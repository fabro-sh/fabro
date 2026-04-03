use std::path::Path;

pub use fabro_types::conclusion::{Conclusion, StageSummary};

use crate::error::{FabroError, Result as CrateResult};

pub trait ConclusionExt {
    fn save(&self, path: &Path) -> CrateResult<()>;
}

impl ConclusionExt for Conclusion {
    fn save(&self, path: &Path) -> CrateResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| FabroError::Checkpoint(format!("conclusion serialize failed: {e}")))?;
        std::fs::write(path, json)?;
        Ok(())
    }
}
