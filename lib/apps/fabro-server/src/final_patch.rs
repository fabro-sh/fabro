//! A run's final patch as text.
//!
//! Petri keeps the final patch in the blob table, and the projection's
//! `conclusion.diff.patch` carries a `blob://sha256/<hex>` reference to it;
//! older projections carry the patch text inline. Server readers resolve the
//! patch here so none of them mistakes the reference for patch text.

use std::borrow::Cow;

use axum::http::StatusCode;
use fabro_store::RunProjection;
use fabro_types::blob_ref;
use fabro_util::error;

use crate::error::ApiError;
use crate::server::AppState;

/// The run's final patch, or `None` when the run recorded none. A reference
/// whose bytes are missing, fail the blob store's integrity check, or are not
/// UTF-8 is an error, never an empty patch.
pub(crate) async fn load<'a>(
    state: &AppState,
    projection: &'a RunProjection,
) -> Result<Option<Cow<'a, str>>, ApiError> {
    let Some(patch) = projection
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.diff.patch.as_deref())
    else {
        return Ok(None);
    };
    let Some(hash) = blob_ref::parse_blob_ref(patch.trim()) else {
        return Ok(Some(Cow::Borrowed(patch)));
    };

    let run_id = &projection.spec.run_id;
    let bytes = state
        .store_ref()
        .blobs()
        .read(&hash)
        .await
        .map_err(|error| {
            tracing::error!(
                %run_id,
                error = %error::collect_chain(&error).join(": "),
                "Failed to read final patch blob"
            );
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The saved final patch could not be read.",
            )
        })?
        .ok_or_else(|| {
            tracing::error!(%run_id, "Final patch blob is missing");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The saved final patch is missing.",
            )
        })?;
    let text = String::from_utf8(bytes.into()).map_err(|error| {
        tracing::error!(%run_id, %error, "Final patch blob is not UTF-8");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The saved final patch is not valid UTF-8.",
        )
    })?;
    Ok(Some(Cow::Owned(text)))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use fabro_types::blob_ref;

    use super::load;
    use crate::test_support::{test_app_state, test_concluded_run_projection};

    #[tokio::test]
    async fn inline_and_absent_patches_pass_through() {
        let state = test_app_state();
        let projection = test_concluded_run_projection(Some("diff --git a/x b/x\n"));
        let patch = load(&state, &projection).await.unwrap();
        assert_eq!(patch.as_deref(), Some("diff --git a/x b/x\n"));

        let projection = test_concluded_run_projection(None);
        assert!(load(&state, &projection).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn blob_reference_resolves_to_its_bytes() {
        let state = test_app_state();
        let text = "diff --git a/x b/x\n+saved\n";
        let hash = state
            .store_ref()
            .blobs()
            .write(text.as_bytes())
            .await
            .unwrap();
        let projection = test_concluded_run_projection(Some(&blob_ref::format_blob_ref(&hash)));
        let patch = load(&state, &projection).await.unwrap();
        assert_eq!(patch.as_deref(), Some(text));
    }

    #[tokio::test]
    async fn missing_and_non_utf8_blobs_are_errors() {
        let state = test_app_state();
        let absent = fabro_types::BlobHash::new(b"never written");
        let projection = test_concluded_run_projection(Some(&blob_ref::format_blob_ref(&absent)));
        let error = load(&state, &projection).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.detail(), "The saved final patch is missing.");

        let hash = state.store_ref().blobs().write(&[0xff]).await.unwrap();
        let projection = test_concluded_run_projection(Some(&blob_ref::format_blob_ref(&hash)));
        let error = load(&state, &projection).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.detail(), "The saved final patch is not valid UTF-8.");
    }
}
