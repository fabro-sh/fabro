use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One choice a question offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InterviewOption {
    pub key:         String,
    pub label:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview:     Option<String>,
}

const REVIEW_TARGET_LABEL_MAX_CHARS: usize = 200;
const REVIEW_TARGET_URL_MAX_CHARS: usize = 2048;

/// The type of resource a human should review. `Display` renders the noun used
/// in review question text ("document").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ReviewTargetKind {
    Document,
}

/// A validated external resource presented as the primary subject of a human
/// review question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewTarget {
    label: String,
    url:   String,
    kind:  ReviewTargetKind,
}

impl ReviewTarget {
    pub fn new(
        label: impl Into<String>,
        url: impl Into<String>,
        kind: ReviewTargetKind,
    ) -> Result<Self, ReviewTargetError> {
        let label = label.into();
        let url = url.into();
        let label = label.trim();
        let url = url.trim();

        validate_review_target_label(label)?;
        validate_review_target_url(url)?;

        Ok(Self {
            label: label.to_string(),
            url: url.to_string(),
            kind,
        })
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub const fn kind(&self) -> ReviewTargetKind {
        self.kind
    }

    /// The question text shown to a human, with the label rendered as plain
    /// text.
    #[must_use]
    pub fn question_text(&self) -> String {
        self.question_text_with_link(&self.label)
    }

    /// The same sentence as [`Self::question_text`], with the label replaced by
    /// a client-specific rendering of the link (Slack `<url|label>` syntax, for
    /// example). This is the single definition of the review question wording.
    #[must_use]
    pub fn question_text_with_link(&self, rendered_link: &str) -> String {
        format!(
            "Review the {rendered_link} {}, then choose the next action.",
            self.kind
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewTargetError {
    #[error("review target label must not be empty")]
    EmptyLabel,
    #[error("review target label must be at most {REVIEW_TARGET_LABEL_MAX_CHARS} characters")]
    LabelTooLong,
    #[error("review target label must not contain control characters")]
    LabelContainsControl,
    #[error("review target URL must not be empty")]
    EmptyUrl,
    #[error("review target URL must be at most {REVIEW_TARGET_URL_MAX_CHARS} characters")]
    UrlTooLong,
    #[error("review target URL must not contain control characters or link delimiters")]
    UrlContainsUnsafeCharacters,
    #[error("review target URL must be a valid absolute URL")]
    InvalidUrl,
    #[error("review target URL must use http or https")]
    UnsupportedUrlScheme,
    #[error("review target URL must include a host")]
    MissingUrlHost,
    #[error("review target URL must not include username or password credentials")]
    UrlContainsCredentials,
}

fn validate_review_target_label(label: &str) -> Result<(), ReviewTargetError> {
    if label.is_empty() {
        return Err(ReviewTargetError::EmptyLabel);
    }
    if label.chars().count() > REVIEW_TARGET_LABEL_MAX_CHARS {
        return Err(ReviewTargetError::LabelTooLong);
    }
    if label.chars().any(char::is_control) {
        return Err(ReviewTargetError::LabelContainsControl);
    }
    Ok(())
}

#[expect(
    clippy::disallowed_types,
    reason = "Review target validation parses an untrusted URL only to enforce safe display schemes and syntax; Fabro never fetches the URL."
)]
fn validate_review_target_url(url: &str) -> Result<(), ReviewTargetError> {
    if url.is_empty() {
        return Err(ReviewTargetError::EmptyUrl);
    }
    if url.chars().count() > REVIEW_TARGET_URL_MAX_CHARS {
        return Err(ReviewTargetError::UrlTooLong);
    }
    if url
        .chars()
        .any(|character| character.is_control() || matches!(character, '<' | '>' | '|'))
    {
        return Err(ReviewTargetError::UrlContainsUnsafeCharacters);
    }

    let parsed = url::Url::parse(url).map_err(|_| ReviewTargetError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ReviewTargetError::UnsupportedUrlScheme);
    }
    if parsed.host_str().is_none() {
        return Err(ReviewTargetError::MissingUrlHost);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ReviewTargetError::UrlContainsCredentials);
    }
    Ok(())
}

impl<'de> Deserialize<'de> for ReviewTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Unknown fields are ignored so the OpenAPI contract (which leaves
        // `additionalProperties` permissive) and this deserializer agree, and
        // so an unknown key in a persisted event cannot fail the whole event.
        #[derive(Deserialize)]
        struct WireReviewTarget {
            label: String,
            url:   String,
            kind:  ReviewTargetKind,
        }

        let wire = WireReviewTarget::deserialize(deserializer)?;
        Self::new(wire.label, wire.url, wire.kind).map_err(D::Error::custom)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Default,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum QuestionType {
    YesNo,
    MultipleChoice,
    MultiSelect,
    #[default]
    Freeform,
    Confirmation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InterviewQuestionRecord {
    #[serde(default)]
    pub id:              String,
    #[serde(default)]
    pub text:            String,
    #[serde(default)]
    pub stage:           String,
    #[serde(default)]
    pub question_type:   QuestionType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options:         Vec<InterviewOption>,
    #[serde(default)]
    pub allow_freeform:  bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_target:   Option<ReviewTarget>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_type_wire_names_roundtrip() {
        let cases = [
            ("yes_no", QuestionType::YesNo),
            ("multiple_choice", QuestionType::MultipleChoice),
            ("multi_select", QuestionType::MultiSelect),
            ("freeform", QuestionType::Freeform),
            ("confirmation", QuestionType::Confirmation),
        ];

        for (wire, question_type) in cases {
            assert_eq!(wire.parse::<QuestionType>().unwrap(), question_type);
            assert_eq!(question_type.to_string(), wire);
        }
    }

    #[test]
    fn review_target_roundtrips_and_builds_question_text() {
        let value = serde_json::json!({
            "label": "Quarry review exercise",
            "url": "https://quarry.lithos.computer/tmp/0123456789abcdef0123456789abcdef",
            "kind": "document",
        });

        let target: ReviewTarget = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(target.label(), "Quarry review exercise");
        assert_eq!(
            target.question_text(),
            "Review the Quarry review exercise document, then choose the next action."
        );
        assert_eq!(serde_json::to_value(target).unwrap(), value);
    }

    #[test]
    fn review_target_rejects_non_http_urls_and_credentials() {
        for url in [
            "javascript:alert(1)",
            "file:///tmp/review.md",
            "https://user:secret@example.com/review",
        ] {
            assert!(
                ReviewTarget::new("Review", url, ReviewTargetKind::Document).is_err(),
                "URL should be rejected: {url}"
            );
        }
    }

    #[test]
    fn review_target_rejects_blank_or_unsafe_display_values() {
        assert_eq!(
            ReviewTarget::new(" ", "https://example.com", ReviewTargetKind::Document),
            Err(ReviewTargetError::EmptyLabel)
        );
        assert_eq!(
            ReviewTarget::new(
                "Review",
                "https://example.com/a|b",
                ReviewTargetKind::Document
            ),
            Err(ReviewTargetError::UrlContainsUnsafeCharacters)
        );
    }
}
