use std::time::Duration;

pub use fabro_types::outcome::{
    FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageOutcome, StageState,
    StageStatus,
};

use crate::error::Error;

pub trait NodeResultExt<M: OutcomeMeta = ()> {
    fn from_error(error: &Error, duration: Duration, attempts: u32, max_attempts: u32) -> Self;
}

impl<M: OutcomeMeta> NodeResultExt<M> for NodeResult<M> {
    fn from_error(error: &Error, duration: Duration, attempts: u32, max_attempts: u32) -> Self {
        Self {
            outcome: error.to_fail_outcome(),
            duration,
            attempts,
            max_attempts,
        }
    }
}
