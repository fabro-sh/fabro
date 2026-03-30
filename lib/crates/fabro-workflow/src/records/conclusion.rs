use std::path::Path;

pub use fabro_types::conclusion::{Conclusion, StageSummary};

use crate::error::Result as CrateResult;

pub trait ConclusionExt {
    fn save(&self, path: &Path) -> CrateResult<()>;
    fn load(path: &Path) -> CrateResult<Self>
    where
        Self: Sized;
}

impl ConclusionExt for Conclusion {
    fn save(&self, path: &Path) -> CrateResult<()> {
        crate::save_json(self, path, "conclusion")
    }

    fn load(path: &Path) -> CrateResult<Self> {
        crate::load_json(path, "conclusion")
    }
}
