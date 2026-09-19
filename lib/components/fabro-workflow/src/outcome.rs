pub use fabro_types::ModelUsage;
pub use fabro_types::outcome::{
    FailureCategory, FailureDetail, OutcomeMeta, StageOutcome, StageState,
};

/// A stage outcome carrying the model usage the stage reported.
pub type Outcome = fabro_types::Outcome<Option<ModelUsage>>;

/// Format a USD cost for display, to the cent.
#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}
