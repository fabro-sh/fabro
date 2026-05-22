use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::common;
use super::common::{FabroToolBackend, RunSummaryResult, ToolError, ToolResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunGetParams {
    pub run_id: String,
}

#[derive(Debug)]
pub struct ValidatedRunGet {
    pub run_id: String,
}

impl TryFrom<FabroRunGetParams> for ValidatedRunGet {
    type Error = ToolError;

    fn try_from(params: FabroRunGetParams) -> Result<Self, Self::Error> {
        let run_id = params.run_id.trim();
        if run_id.is_empty() {
            return Err(ToolError::message("run_id is required"));
        }
        Ok(Self {
            run_id: run_id.to_string(),
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunGetResult {
    pub run_id:     String,
    pub summary:    RunSummaryResult,
    pub projection: Value,
    pub questions:  Value,
}

pub async fn run_get(
    backend: Arc<dyn FabroToolBackend>,
    params: ValidatedRunGet,
) -> ToolResult<RunGetResult> {
    let run_id = backend
        .resolve_run(&params.run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;
    let summary = common::retrieve_run(backend.as_ref(), &run_id).await?;
    let projection = backend
        .get_run_state(&run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?;
    let questions = backend
        .list_run_questions(&run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?;
    Ok(RunGetResult {
        run_id:     run_id.to_string(),
        summary:    common::run_summary_result(&summary),
        projection: json!(projection),
        questions:  json!(questions),
    })
}

pub fn run_get_text(result: &RunGetResult) -> String {
    format!("returned Fabro run {}", result.run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_run_id() {
        let err = ValidatedRunGet::try_from(FabroRunGetParams {
            run_id: "   ".to_string(),
        })
        .unwrap_err();

        assert!(err.as_str().contains("run_id"));
    }

    #[test]
    fn validate_trims_run_id() {
        let params = ValidatedRunGet::try_from(FabroRunGetParams {
            run_id: "  nightly  ".to_string(),
        })
        .unwrap();

        assert_eq!(params.run_id, "nightly");
    }
}
