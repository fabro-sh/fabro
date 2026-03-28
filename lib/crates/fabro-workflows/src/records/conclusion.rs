use std::path::Path;

pub use fabro_types::conclusion::{Conclusion, StageSummary};

pub trait ConclusionExt {
    fn save(&self, path: &Path) -> crate::error::Result<()>;
    fn load(path: &Path) -> crate::error::Result<Self>
    where
        Self: Sized;
}

impl ConclusionExt for Conclusion {
    fn save(&self, path: &Path) -> crate::error::Result<()> {
        crate::save_json(self, path, "conclusion")
    }

    fn load(path: &Path) -> crate::error::Result<Self> {
        crate::load_json(path, "conclusion")
    }
}
