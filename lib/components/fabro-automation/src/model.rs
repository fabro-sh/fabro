use std::collections::HashSet;
use std::sync::LazyLock;

use croner::Cron;
use croner::errors::CronError;
use croner::parser::{CronParser, Seconds, Year};
use fabro_types::{ExternalAgentHarness, GitHubRepositorySlug, repository};
use serde::{Deserialize, Serialize};

use crate::{
    AutomationId, AutomationRevision, AutomationStoreError, AutomationTriggerId,
    AutomationValidationError,
};

/// Shared cron parser used to validate and evaluate automation schedule trigger
/// expressions. Schedule triggers use the same five-field UTC cron grammar as
/// validation, so both sites must share configuration.
static SCHEDULE_CRON_PARSER: LazyLock<CronParser> = LazyLock::new(|| {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
});

pub(crate) const MANUAL_TRIGGER_ID: &str = "manual";

/// Parse an automation schedule trigger expression with the canonical
/// configuration (no seconds, no year). Returned `Cron` instances can be cached
/// and used to find next occurrences.
pub fn parse_schedule_expression(expression: &str) -> Result<Cron, CronError> {
    SCHEDULE_CRON_PARSER.parse(expression)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Automation {
    pub id:          AutomationId,
    pub revision:    AutomationRevision,
    pub name:        String,
    pub description: Option<String>,
    pub target:      AutomationTarget,
    pub triggers:    Vec<AutomationTrigger>,
}

impl Automation {
    pub fn from_toml_bytes(id: AutomationId, bytes: &[u8]) -> Result<Self, AutomationStoreError> {
        let revision = AutomationRevision::from_bytes(bytes);
        let persisted = parse_persisted(bytes, None)?;
        Self::from_persisted(id, revision, persisted).map_err(AutomationStoreError::from)
    }

    pub(crate) fn from_persisted_path(
        id: AutomationId,
        bytes: &[u8],
        path: impl Into<std::path::PathBuf>,
    ) -> Result<Self, AutomationStoreError> {
        let path = path.into();
        let revision = AutomationRevision::from_bytes(bytes);
        let persisted = parse_persisted(bytes, Some(path))?;
        Self::from_persisted(id, revision, persisted).map_err(AutomationStoreError::from)
    }

    pub(crate) fn from_replace(
        id: AutomationId,
        draft: AutomationReplace,
    ) -> Result<(Self, Vec<u8>), AutomationStoreError> {
        let draft = normalize_replace(draft)?;
        let persisted = PersistedAutomation::from(draft.clone());
        let bytes = canonical_bytes(&persisted)?;
        let revision = AutomationRevision::from_bytes(&bytes);
        let automation = Self::from_validated_replace(id, revision, draft);
        Ok((automation, bytes))
    }

    pub(crate) fn from_stored(
        id: AutomationId,
        revision: AutomationRevision,
        value: AutomationReplace,
    ) -> Result<Self, AutomationValidationError> {
        let value = normalize_replace(value)?;
        Ok(Self::from_validated_replace(id, revision, value))
    }

    pub(crate) fn to_persisted(&self) -> PersistedAutomation {
        PersistedAutomation {
            name:        self.name.clone(),
            description: self.description.clone(),
            target:      self.target.clone(),
            triggers:    self.triggers.clone(),
        }
    }

    pub fn to_toml_string(&self) -> Result<String, AutomationStoreError> {
        toml::to_string_pretty(&self.to_persisted()).map_err(AutomationStoreError::from)
    }

    /// Returns the enabled API trigger if the automation has one.
    /// Returns `None` when the automation has no enabled API trigger.
    #[must_use]
    pub fn enabled_api_trigger(&self) -> Option<&ApiTrigger> {
        self.triggers.iter().find_map(|trigger| match trigger {
            AutomationTrigger::Api(trigger) if trigger.enabled => Some(trigger),
            _ => None,
        })
    }

    /// Iterate the enabled schedule triggers.
    pub fn enabled_schedule_triggers(&self) -> impl Iterator<Item = &ScheduleTrigger> {
        self.triggers
            .iter()
            .filter_map(move |trigger| match trigger {
                AutomationTrigger::Schedule(trigger) if trigger.enabled => Some(trigger),
                _ => None,
            })
    }

    /// Iterate the enabled plane triggers.
    pub fn enabled_plane_triggers(&self) -> impl Iterator<Item = &PlaneTrigger> {
        self.triggers
            .iter()
            .filter_map(move |trigger| match trigger {
                AutomationTrigger::Plane(trigger) if trigger.enabled => Some(trigger),
                _ => None,
            })
    }

    pub(crate) fn schedule_triggers(&self) -> impl Iterator<Item = &ScheduleTrigger> {
        self.triggers.iter().filter_map(|trigger| match trigger {
            AutomationTrigger::Schedule(trigger) => Some(trigger),
            _ => None,
        })
    }

    pub(crate) fn plane_triggers(&self) -> impl Iterator<Item = &PlaneTrigger> {
        self.triggers.iter().filter_map(|trigger| match trigger {
            AutomationTrigger::Plane(trigger) => Some(trigger),
            _ => None,
        })
    }

    pub(crate) fn api_enabled(&self) -> bool {
        self.enabled_api_trigger().is_some()
    }

    fn from_persisted(
        id: AutomationId,
        revision: AutomationRevision,
        persisted: PersistedAutomation,
    ) -> Result<Self, AutomationValidationError> {
        let replace = normalize_replace(AutomationReplace::from(persisted))?;
        Ok(Self::from_validated_replace(id, revision, replace))
    }

    fn from_validated_replace(
        id: AutomationId,
        revision: AutomationRevision,
        replace: AutomationReplace,
    ) -> Self {
        Self {
            id,
            revision,
            name: replace.name,
            description: replace.description,
            target: replace.target,
            triggers: replace.triggers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationTarget {
    pub repository:   String,
    #[serde(rename = "ref")]
    pub ref_selector: String,
    pub workflow:     String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomationTrigger {
    Api(ApiTrigger),
    Schedule(ScheduleTrigger),
    Plane(PlaneTrigger),
}

impl AutomationTrigger {
    #[must_use]
    pub fn id(&self) -> &AutomationTriggerId {
        match self {
            Self::Api(trigger) => &trigger.id,
            Self::Schedule(trigger) => &trigger.id,
            Self::Plane(trigger) => &trigger.id,
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        match self {
            Self::Api(trigger) => trigger.enabled,
            Self::Schedule(trigger) => trigger.enabled,
            Self::Plane(trigger) => trigger.enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiTrigger {
    pub id:      AutomationTriggerId,
    pub enabled: bool,
}

impl ApiTrigger {
    /// The canonical enabled API trigger. Automations store API enablement as a
    /// flag and re-materialize it as this trigger with the fixed `manual` id.
    pub(crate) fn manual() -> Self {
        Self {
            id:      AutomationTriggerId::new(MANUAL_TRIGGER_ID)
                .expect("manual automation trigger id is valid"),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleTrigger {
    pub id:         AutomationTriggerId,
    pub enabled:    bool,
    pub expression: String,
}

pub(crate) const DEFAULT_PLANE_POLL_INTERVAL_SECONDS: u64 = 60;
pub(crate) const MIN_PLANE_POLL_INTERVAL_SECONDS: u64 = 15;
pub(crate) const MAX_PLANE_POLL_INTERVAL_SECONDS: u64 = 3600;

pub(crate) const DEFAULT_PLANE_MAX_CONCURRENCY: usize = 3;
pub(crate) const MIN_PLANE_MAX_CONCURRENCY: usize = 1;
pub(crate) const MAX_PLANE_MAX_CONCURRENCY: usize = 10;

pub(crate) const DEFAULT_PLANE_MAX_RETRIES: usize = 1;

fn default_plane_poll_interval_seconds() -> u64 {
    DEFAULT_PLANE_POLL_INTERVAL_SECONDS
}

fn default_plane_max_concurrency() -> usize {
    DEFAULT_PLANE_MAX_CONCURRENCY
}

fn default_plane_max_retries() -> usize {
    DEFAULT_PLANE_MAX_RETRIES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaneTrigger {
    pub id:                    AutomationTriggerId,
    pub enabled:               bool,
    pub project_id:            String,
    pub ready_state_id:        String,
    pub in_progress_state_id:  String,
    pub done_state_id:         String,
    pub cancelled_state_id:    String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_label_id:      Option<String>,
    pub default_harness:       ExternalAgentHarness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_label_id:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omp_label_id:          Option<String>,
    #[serde(default = "default_plane_poll_interval_seconds")]
    pub poll_interval_seconds: u64,
    #[serde(default = "default_plane_max_concurrency")]
    pub max_concurrency:       usize,
    #[serde(default = "default_plane_max_retries")]
    pub max_retries:           usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationDraft {
    pub id:          AutomationId,
    pub name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub target:      AutomationTarget,
    pub triggers:    Vec<AutomationTrigger>,
}

impl From<AutomationDraft> for (AutomationId, AutomationReplace) {
    fn from(value: AutomationDraft) -> Self {
        (value.id, AutomationReplace {
            name:        value.name,
            description: value.description,
            target:      value.target,
            triggers:    value.triggers,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationReplace {
    pub name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub target:      AutomationTarget,
    pub triggers:    Vec<AutomationTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedAutomation {
    name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    target:      AutomationTarget,
    #[serde(default)]
    triggers:    Vec<AutomationTrigger>,
}

impl From<AutomationReplace> for PersistedAutomation {
    fn from(value: AutomationReplace) -> Self {
        Self {
            name:        value.name,
            description: value.description,
            target:      value.target,
            triggers:    value.triggers,
        }
    }
}

impl From<PersistedAutomation> for AutomationReplace {
    fn from(value: PersistedAutomation) -> Self {
        Self {
            name:        value.name,
            description: value.description,
            target:      value.target,
            triggers:    value.triggers,
        }
    }
}

pub(crate) fn canonical_bytes(
    persisted: &PersistedAutomation,
) -> Result<Vec<u8>, AutomationStoreError> {
    let toml = toml::to_string_pretty(persisted)?;
    Ok(toml.into_bytes())
}

fn parse_persisted(
    bytes: &[u8],
    path: Option<std::path::PathBuf>,
) -> Result<PersistedAutomation, AutomationStoreError> {
    let content = std::str::from_utf8(bytes).map_err(|err| match &path {
        Some(path) => AutomationStoreError::invalid_utf8(path.clone(), err),
        None => AutomationStoreError::invalid_utf8("<memory>", err),
    })?;
    toml::from_str(content).map_err(|err| match path {
        Some(path) => AutomationStoreError::parse(path, err),
        None => AutomationStoreError::parse("<memory>", err),
    })
}

fn validate_fields(value: &AutomationReplace) -> Result<(), AutomationValidationError> {
    if value.name.trim().is_empty() {
        return Err(AutomationValidationError::EmptyName);
    }
    validate_repository_slug(&value.target.repository)?;
    validate_git_ref_selector(&value.target.ref_selector)?;
    validate_workflow_selector(&value.target.workflow)?;
    validate_triggers(&value.triggers)
}

fn normalize_replace(
    mut value: AutomationReplace,
) -> Result<AutomationReplace, AutomationValidationError> {
    validate_fields(&value)?;

    let api_enabled = value
        .triggers
        .iter()
        .any(|trigger| matches!(trigger, AutomationTrigger::Api(trigger) if trigger.enabled));
    let mut schedules = Vec::new();
    let mut planes = Vec::new();

    for trigger in value.triggers {
        match trigger {
            AutomationTrigger::Api(_) => {}
            AutomationTrigger::Schedule(trigger) => schedules.push(trigger),
            AutomationTrigger::Plane(trigger) => planes.push(trigger),
        }
    }

    schedules.sort_by(|left, right| left.id.cmp(&right.id));
    planes.sort_by(|left, right| left.id.cmp(&right.id));

    // Canonicalization renames the enabled API trigger to `manual`, which can
    // collide with a schedule/plane trigger id even when the input ids were unique.
    if api_enabled
        && (schedules
            .iter()
            .any(|schedule| schedule.id.as_str() == MANUAL_TRIGGER_ID)
            || planes
                .iter()
                .any(|plane| plane.id.as_str() == MANUAL_TRIGGER_ID))
    {
        return Err(AutomationValidationError::DuplicateTriggerId {
            id: MANUAL_TRIGGER_ID.to_string(),
        });
    }

    let mut triggers =
        Vec::with_capacity(schedules.len() + planes.len() + usize::from(api_enabled));
    if api_enabled {
        triggers.push(AutomationTrigger::Api(ApiTrigger::manual()));
    }
    triggers.extend(schedules.into_iter().map(AutomationTrigger::Schedule));
    triggers.extend(planes.into_iter().map(AutomationTrigger::Plane));

    value.triggers = triggers;
    Ok(value)
}
pub fn parse_github_repository_slug(
    value: &str,
) -> Result<GitHubRepositorySlug, AutomationValidationError> {
    GitHubRepositorySlug::try_new(value).ok_or_else(|| {
        AutomationValidationError::InvalidRepositorySlug {
            value: value.to_string(),
        }
    })
}

fn validate_repository_slug(value: &str) -> Result<(), AutomationValidationError> {
    parse_github_repository_slug(value).map(|_| ())
}

fn validate_git_ref_selector(value: &str) -> Result<(), AutomationValidationError> {
    if repository::is_valid_github_ref_selector(value) {
        Ok(())
    } else {
        Err(AutomationValidationError::InvalidGitRefSelector {
            value: value.to_string(),
        })
    }
}

fn validate_workflow_selector(value: &str) -> Result<(), AutomationValidationError> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value.trim() == value
        && !value.starts_with(['/', '~'])
        && !value.ends_with('/')
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if valid {
        Ok(())
    } else {
        Err(AutomationValidationError::InvalidWorkflowSelector {
            value: value.to_string(),
        })
    }
}

fn validate_triggers(triggers: &[AutomationTrigger]) -> Result<(), AutomationValidationError> {
    let mut seen = HashSet::new();
    let mut has_api_trigger = false;

    for trigger in triggers {
        let id = trigger.id().as_str();
        if !seen.insert(id) {
            return Err(AutomationValidationError::DuplicateTriggerId { id: id.to_string() });
        }
        match trigger {
            AutomationTrigger::Api(_) => {
                if has_api_trigger {
                    return Err(AutomationValidationError::MultipleApiTriggers);
                }
                has_api_trigger = true;
            }
            AutomationTrigger::Schedule(trigger) => {
                if trigger.expression.split_whitespace().count() != 5 {
                    return Err(AutomationValidationError::InvalidCronFieldCount {
                        trigger_id: id.to_string(),
                        expression: trigger.expression.clone(),
                    });
                }
                parse_schedule_expression(&trigger.expression).map_err(|source| {
                    AutomationValidationError::InvalidCronExpression {
                        trigger_id: id.to_string(),
                        expression: trigger.expression.clone(),
                        source,
                    }
                })?;
            }
            AutomationTrigger::Plane(trigger) => {
                if trigger.project_id.trim().is_empty() {
                    return Err(AutomationValidationError::EmptyPlaneProjectId {
                        trigger_id: id.to_string(),
                    });
                }
                if !(MIN_PLANE_POLL_INTERVAL_SECONDS..=MAX_PLANE_POLL_INTERVAL_SECONDS)
                    .contains(&trigger.poll_interval_seconds)
                {
                    return Err(AutomationValidationError::InvalidPlanePollInterval {
                        trigger_id: id.to_string(),
                        seconds:    trigger.poll_interval_seconds,
                    });
                }
                if !(MIN_PLANE_MAX_CONCURRENCY..=MAX_PLANE_MAX_CONCURRENCY)
                    .contains(&trigger.max_concurrency)
                {
                    return Err(AutomationValidationError::InvalidPlaneConcurrency {
                        trigger_id:  id.to_string(),
                        concurrency: trigger.max_concurrency,
                    });
                }

                let mut state_ids = HashSet::new();
                for state_id in [
                    &trigger.ready_state_id,
                    &trigger.in_progress_state_id,
                    &trigger.done_state_id,
                    &trigger.cancelled_state_id,
                ] {
                    if state_id.trim().is_empty() || !state_ids.insert(state_id) {
                        return Err(AutomationValidationError::DuplicatePlaneStateId {
                            trigger_id: id.to_string(),
                            state_id:   (*state_id).clone(),
                        });
                    }
                }

                if let (Some(c), Some(o)) = (&trigger.codex_label_id, &trigger.omp_label_id) {
                    if !c.is_empty() && c == o {
                        return Err(AutomationValidationError::ConflictingPlaneHarnessLabels {
                            trigger_id: id.to_string(),
                            label_id:   c.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use fabro_types::ExternalAgentHarness;

    use super::validate_triggers;
    use crate::{
        ApiTrigger, Automation, AutomationId, AutomationReplace, AutomationTarget,
        AutomationTrigger, AutomationTriggerId, AutomationValidationError, PlaneTrigger,
        ScheduleTrigger,
    };

    fn target() -> AutomationTarget {
        AutomationTarget {
            repository:   "fabro-sh/fabro".to_string(),
            ref_selector: "main".to_string(),
            workflow:     ".fabro/workflows/test/workflow.toml".to_string(),
        }
    }

    fn api_trigger(id: &str) -> AutomationTrigger {
        AutomationTrigger::Api(ApiTrigger {
            id:      AutomationTriggerId::new(id).unwrap(),
            enabled: true,
        })
    }

    fn schedule_trigger(id: &str, cron: &str) -> AutomationTrigger {
        schedule_trigger_with_enabled(id, cron, true)
    }

    fn schedule_trigger_with_enabled(id: &str, cron: &str, enabled: bool) -> AutomationTrigger {
        AutomationTrigger::Schedule(ScheduleTrigger {
            id: AutomationTriggerId::new(id).unwrap(),
            enabled,
            expression: cron.to_string(),
        })
    }

    #[test]
    fn persisted_toml_applies_defaults_and_canonicalizes_without_id_or_revision() {
        let bytes = br#"
name = "Nightly"

[target]
repository = "fabro-sh/fabro"
ref = "main"
workflow = "release"

[[triggers]]
type = "api"
id = "manual"
enabled = true

[[triggers]]
type = "schedule"
id = "nightly"
enabled = true
expression = "0 0 * * *"
"#;

        let automation =
            Automation::from_toml_bytes(AutomationId::new("nightly").unwrap(), bytes).unwrap();

        assert_eq!(automation.description, None);
        assert!(automation.triggers.iter().all(AutomationTrigger::enabled));

        let toml = automation.to_toml_string().unwrap();
        assert!(!top_level_lines(&toml).any(|line| line.starts_with("id = ")));
        assert!(!top_level_lines(&toml).any(|line| line.starts_with("revision = ")));
        assert!(!top_level_lines(&toml).any(|line| line.starts_with("enabled = ")));
        assert!(toml.contains("type = \"api\""));
    }

    #[test]
    fn persisted_toml_rejects_legacy_top_level_enabled() {
        let bytes = br#"
name = "Legacy"
enabled = false

[target]
repository = "fabro-sh/fabro"
ref = "main"
workflow = "release"

[[triggers]]
type = "api"
id = "manual"
enabled = true
"#;

        let result = Automation::from_toml_bytes(AutomationId::new("legacy").unwrap(), bytes);

        assert!(result.is_err());
    }

    #[test]
    fn enabled_schedule_triggers_returns_only_enabled_schedule_triggers() {
        let (automation, _) =
            Automation::from_replace(AutomationId::new("nightly").unwrap(), AutomationReplace {
                name:        "Nightly".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![
                    api_trigger("manual"),
                    schedule_trigger_with_enabled("nightly", "0 0 * * *", true),
                    schedule_trigger_with_enabled("disabled", "0 1 * * *", false),
                ],
            })
            .unwrap();

        let trigger_ids = automation
            .enabled_schedule_triggers()
            .map(|trigger| trigger.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(trigger_ids, vec!["nightly"]);
    }

    #[test]
    fn repository_slug_parser_returns_the_shared_type() {
        let slug: fabro_types::GitHubRepositorySlug =
            crate::parse_github_repository_slug("owner/.github").unwrap();

        assert_eq!(slug.owner(), "owner");
        assert_eq!(slug.repo(), ".github");
    }

    #[test]
    fn invalid_repository_slug_preserves_the_automation_error() {
        let error = crate::parse_github_repository_slug("not/github/slug").unwrap_err();

        assert!(matches!(
            &error,
            AutomationValidationError::InvalidRepositorySlug { value }
                if value == "not/github/slug"
        ));
        assert_eq!(
            error.to_string(),
            "repository slug \"not/github/slug\" must be a GitHub owner/repo slug"
        );
    }

    #[test]
    fn invalid_git_ref_selector_preserves_the_automation_error() {
        let error = super::validate_git_ref_selector("main;rm").unwrap_err();

        assert!(matches!(
            &error,
            AutomationValidationError::InvalidGitRefSelector { value } if value == "main;rm"
        ));
        assert_eq!(
            error.to_string(),
            "git ref selector \"main;rm\" is not safe"
        );
    }

    #[test]
    fn validation_rejects_invalid_inputs() {
        let cases = [
            AutomationReplace {
                name:        " ".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:        "Bad repo".to_string(),
                description: None,
                target:      AutomationTarget {
                    repository:   "not/github/slug".to_string(),
                    ref_selector: "main".to_string(),
                    workflow:     "release".to_string(),
                },
                triggers:    vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:        "Bad ref".to_string(),
                description: None,
                target:      AutomationTarget {
                    repository:   "fabro-sh/fabro".to_string(),
                    ref_selector: "main;rm".to_string(),
                    workflow:     "release".to_string(),
                },
                triggers:    vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:        "Bad workflow".to_string(),
                description: None,
                target:      AutomationTarget {
                    repository:   "fabro-sh/fabro".to_string(),
                    ref_selector: "main".to_string(),
                    workflow:     "../release".to_string(),
                },
                triggers:    vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:        "Duplicate trigger".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![
                    api_trigger("manual"),
                    schedule_trigger("manual", "0 0 * * *"),
                ],
            },
            AutomationReplace {
                name:        "Two API triggers".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![api_trigger("one"), api_trigger("two")],
            },
            AutomationReplace {
                name:        "Six field cron".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![schedule_trigger("nightly", "0 0 0 * * *")],
            },
            AutomationReplace {
                name:        "Bad cron".to_string(),
                description: None,
                target:      target(),
                triggers:    vec![schedule_trigger("nightly", "99 0 * * *")],
            },
        ];

        for case in cases {
            assert!(Automation::from_replace(AutomationId::new("test").unwrap(), case).is_err());
        }
    }

    fn sample_plane_trigger(id: &str) -> AutomationTrigger {
        AutomationTrigger::Plane(PlaneTrigger {
            id:                    AutomationTriggerId::new(id).unwrap(),
            enabled:               true,
            project_id:            "0194e43b-252a-7ad2-a50e-7d6f5fb47db3".to_string(),
            ready_state_id:        "state-ready".to_string(),
            in_progress_state_id:  "state-in-progress".to_string(),
            done_state_id:         "state-done".to_string(),
            cancelled_state_id:    "state-cancelled".to_string(),
            failure_label_id:      Some("label-failed".to_string()),
            default_harness:       ExternalAgentHarness::Codex,
            codex_label_id:        Some("label-codex".to_string()),
            omp_label_id:          Some("label-omp".to_string()),
            poll_interval_seconds: 60,
            max_concurrency:       3,
            max_retries:           1,
        })
    }

    #[test]
    fn plane_trigger_toml_round_trip() {
        let automation = AutomationReplace {
            name:        "Plane Loop".to_string(),
            description: Some("Polls Plane for ready tickets".to_string()),
            target:      target(),
            triggers:    vec![api_trigger("manual"), sample_plane_trigger("plane-tickets")],
        };

        let (auto, persisted) =
            Automation::from_replace(AutomationId::new("plane-auto").unwrap(), automation.clone())
                .unwrap();
        let toml_str = std::str::from_utf8(&persisted).unwrap();
        assert!(toml_str.contains("type = \"plane\""));
        assert!(toml_str.contains("project_id = \"0194e43b-252a-7ad2-a50e-7d6f5fb47db3\""));
        assert!(toml_str.contains("default_harness = \"codex\""));

        let parsed =
            Automation::from_toml_bytes(AutomationId::new("plane-auto").unwrap(), &persisted)
                .unwrap();
        assert_eq!(auto.name, parsed.name);
        assert_eq!(auto.triggers.len(), 2);
        assert_eq!(auto.triggers[1], parsed.triggers[1]);
    }

    #[test]
    fn plane_trigger_validation_rejects_invalid_fields() {
        let AutomationTrigger::Plane(mut trigger) = sample_plane_trigger("plane-test") else {
            unreachable!();
        };

        // Empty project id
        trigger.project_id = "   ".to_string();
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::EmptyPlaneProjectId { .. })
        ));
        trigger.project_id = "proj-1".to_string();

        // Poll interval too low
        trigger.poll_interval_seconds = 10;
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::InvalidPlanePollInterval { .. })
        ));
        trigger.poll_interval_seconds = 60;

        // Poll interval too high
        trigger.poll_interval_seconds = 4000;
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::InvalidPlanePollInterval { .. })
        ));
        trigger.poll_interval_seconds = 60;

        // Concurrency 0
        trigger.max_concurrency = 0;
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::InvalidPlaneConcurrency { .. })
        ));
        trigger.max_concurrency = 3;

        // Concurrency 11
        trigger.max_concurrency = 11;
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::InvalidPlaneConcurrency { .. })
        ));
        trigger.max_concurrency = 3;

        // Duplicate state IDs
        trigger.ready_state_id = "same-state".to_string();
        trigger.done_state_id = "same-state".to_string();
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::DuplicatePlaneStateId { .. })
        ));
        trigger.ready_state_id = "state-ready".to_string();
        trigger.done_state_id = "state-done".to_string();

        // Conflicting harness override labels
        trigger.codex_label_id = Some("same-label".to_string());
        trigger.omp_label_id = Some("same-label".to_string());
        let res = validate_triggers(&[AutomationTrigger::Plane(trigger.clone())]);
        assert!(matches!(
            res,
            Err(AutomationValidationError::ConflictingPlaneHarnessLabels { .. })
        ));
    }

    fn top_level_lines(toml: &str) -> impl Iterator<Item = &str> {
        toml.lines().take_while(|line| !line.starts_with('['))
    }
}
