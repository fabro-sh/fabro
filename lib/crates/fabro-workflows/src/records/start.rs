use std::path::Path;

pub use fabro_types::start::StartRecord;

use crate::error::Result as CrateResult;

const FILE_NAME: &str = "start.json";

pub trait StartRecordExt {
    fn file_name() -> &'static str
    where
        Self: Sized;
    fn save(&self, run_dir: &Path) -> CrateResult<()>;
    fn load(run_dir: &Path) -> CrateResult<Self>
    where
        Self: Sized;
}

impl StartRecordExt for StartRecord {
    fn file_name() -> &'static str {
        FILE_NAME
    }

    fn save(&self, run_dir: &Path) -> CrateResult<()> {
        crate::save_json(self, &run_dir.join(FILE_NAME), "start record")
    }

    fn load(run_dir: &Path) -> CrateResult<Self> {
        crate::load_json(&run_dir.join(FILE_NAME), "start record")
    }
}
