use fabro_store::Error as StoreError;
use fabro_types::RunId;

use crate::error::Error;

pub(super) fn map_open_run_error(run_id: &RunId, err: StoreError) -> Error {
    match err {
        StoreError::RunNotFound(id) => Error::RunNotFound(id),
        other => Error::engine(format!("failed to open run {run_id}: {other}")),
    }
}
