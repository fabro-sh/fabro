use std::collections::HashSet;
use std::sync::LazyLock;

use croner::Cron;
use croner::errors::CronError;
use croner::parser::{CronParser, Seconds, Year};
use fabro_types::{GitRunTarget, RunTarget};
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
    pub id:              AutomationId,
    pub revision:        AutomationRevision,
    pub name:            String,
    pub description:     Option<String>,
    /// Server-managed environment selected when the automation fires. Legacy
    /// rows may be incomplete until an operator selects one.
    pub environment_id:  Option<String>,
    /// Most recent scheduler failure. Runtime status is not part of the
    /// optimistic-concurrency revision.
    pub last_error:      Option<String>,
    pub target:          RunTarget,
    pub workflow:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<AutomationGitWorkflowSource>,
    pub triggers:        Vec<AutomationTrigger>,
}

impl Automation {
    pub fn from_toml_bytes(id: AutomationId, bytes: &[u8]) -> Result<Self, AutomationStoreError> {
        let revision = AutomationRevision::from_bytes(bytes);
        let persisted = parse_persisted(bytes, None)?;
        Self::from_persisted(id, revision, persisted).map_err(AutomationStoreError::from)
    }

    pub(crate) fn from_replace(
        id: AutomationId,
        draft: AutomationReplace,
    ) -> Result<(Self, Vec<u8>), AutomationStoreError> {
        let draft = normalize_replace(draft, true)?;
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
        let value = normalize_replace(value, false)?;
        Ok(Self::from_validated_replace(id, revision, value))
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

    pub(crate) fn schedule_triggers(&self) -> impl Iterator<Item = &ScheduleTrigger> {
        self.triggers.iter().filter_map(|trigger| match trigger {
            AutomationTrigger::Schedule(trigger) => Some(trigger),
            AutomationTrigger::Api(_) => None,
        })
    }

    pub(crate) fn api_enabled(&self) -> bool {
        self.enabled_api_trigger().is_some()
    }

    /// Returns the validated Git target owned by this automation.
    #[must_use]
    pub fn git_target(&self) -> Option<&GitRunTarget> {
        match &self.target {
            RunTarget::Git(target) => Some(target),
            RunTarget::None {} | RunTarget::Folder { .. } => None,
        }
    }

    fn from_persisted(
        id: AutomationId,
        revision: AutomationRevision,
        persisted: PersistedAutomation,
    ) -> Result<Self, AutomationValidationError> {
        let replace = normalize_replace(AutomationReplace::from(persisted), false)?;
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
            environment_id: replace.environment_id,
            last_error: None,
            target: replace.target,
            workflow: replace.workflow,
            workflow_source: replace.workflow_source,
            triggers: replace.triggers,
        }
    }
}

/// A Git coordinate from which an automation loads workflow files.
///
/// This deliberately reuses the run target's branch/tag/SHA model. The branch
/// is the fallback selector and audit context; it does not constrain an exact
/// SHA to be reachable from that branch.
pub type AutomationGitWorkflowSource = GitRunTarget;

/// Validate and canonicalize a saved workflow source without resolving remote
/// repository state.
///
/// # Errors
///
/// Returns an error when the repository slug, branch, tag, or exact commit
/// does not use the canonical Git-coordinate grammar.
pub fn validate_workflow_source(
    source: AutomationGitWorkflowSource,
) -> Result<AutomationGitWorkflowSource, AutomationValidationError> {
    source
        .canonical()
        .map_err(|source| AutomationValidationError::InvalidWorkflowSource { source })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomationTrigger {
    Api(ApiTrigger),
    Schedule(ScheduleTrigger),
}

impl AutomationTrigger {
    #[must_use]
    pub fn id(&self) -> &AutomationTriggerId {
        match self {
            Self::Api(trigger) => &trigger.id,
            Self::Schedule(trigger) => &trigger.id,
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        match self {
            Self::Api(trigger) => trigger.enabled,
            Self::Schedule(trigger) => trigger.enabled,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationDraft {
    pub id:              AutomationId,
    pub name:            String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description:     Option<String>,
    #[serde(default)]
    pub environment_id:  Option<String>,
    pub target:          RunTarget,
    pub workflow:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<AutomationGitWorkflowSource>,
    pub triggers:        Vec<AutomationTrigger>,
}

impl From<AutomationDraft> for (AutomationId, AutomationReplace) {
    fn from(value: AutomationDraft) -> Self {
        (value.id, AutomationReplace {
            name:            value.name,
            description:     value.description,
            environment_id:  value.environment_id,
            target:          value.target,
            workflow:        value.workflow,
            workflow_source: value.workflow_source,
            triggers:        value.triggers,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationReplace {
    pub name:            String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description:     Option<String>,
    #[serde(default)]
    pub environment_id:  Option<String>,
    pub target:          RunTarget,
    pub workflow:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<AutomationGitWorkflowSource>,
    pub triggers:        Vec<AutomationTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedAutomation {
    name:            String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment_id:  Option<String>,
    target:          RunTarget,
    workflow:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_source: Option<AutomationGitWorkflowSource>,
    #[serde(default)]
    triggers:        Vec<AutomationTrigger>,
}

impl From<AutomationReplace> for PersistedAutomation {
    fn from(value: AutomationReplace) -> Self {
        Self {
            name:            value.name,
            description:     value.description,
            environment_id:  value.environment_id,
            target:          value.target,
            workflow:        value.workflow,
            workflow_source: value.workflow_source,
            triggers:        value.triggers,
        }
    }
}

impl From<PersistedAutomation> for AutomationReplace {
    fn from(value: PersistedAutomation) -> Self {
        Self {
            name:            value.name,
            description:     value.description,
            environment_id:  value.environment_id,
            target:          value.target,
            workflow:        value.workflow,
            workflow_source: value.workflow_source,
            triggers:        value.triggers,
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

fn validate_fields(
    value: &AutomationReplace,
    require_environment: bool,
) -> Result<(), AutomationValidationError> {
    if value.name.trim().is_empty() {
        return Err(AutomationValidationError::EmptyName);
    }
    if require_environment && value.environment_id.is_none() {
        return Err(AutomationValidationError::MissingEnvironment);
    }
    validate_workflow_selector(&value.workflow)?;
    validate_triggers(&value.triggers)
}

fn normalize_replace(
    mut value: AutomationReplace,
    require_environment: bool,
) -> Result<AutomationReplace, AutomationValidationError> {
    value.target = validate_target(value.target)?;
    value.environment_id = value
        .environment_id
        .map(|environment_id| environment_id.trim().to_string())
        .filter(|environment_id| !environment_id.is_empty());
    value.workflow_source = value
        .workflow_source
        .map(validate_workflow_source)
        .transpose()?;
    validate_fields(&value, require_environment)?;

    let api_enabled = value
        .triggers
        .iter()
        .any(|trigger| matches!(trigger, AutomationTrigger::Api(trigger) if trigger.enabled));
    let mut schedules = value
        .triggers
        .into_iter()
        .filter_map(|trigger| match trigger {
            AutomationTrigger::Schedule(trigger) => Some(trigger),
            AutomationTrigger::Api(_) => None,
        })
        .collect::<Vec<_>>();
    schedules.sort_by(|left, right| left.id.cmp(&right.id));

    // Canonicalization renames the enabled API trigger to `manual`, which can
    // collide with a schedule trigger id even when the input ids were unique.
    if api_enabled
        && schedules
            .iter()
            .any(|schedule| schedule.id.as_str() == MANUAL_TRIGGER_ID)
    {
        return Err(AutomationValidationError::DuplicateTriggerId {
            id: MANUAL_TRIGGER_ID.to_string(),
        });
    }

    let mut triggers = Vec::with_capacity(schedules.len() + usize::from(api_enabled));
    if api_enabled {
        triggers.push(AutomationTrigger::Api(ApiTrigger::manual()));
    }
    triggers.extend(schedules.into_iter().map(AutomationTrigger::Schedule));
    value.triggers = triggers;
    Ok(value)
}

fn validate_target(target: RunTarget) -> Result<RunTarget, AutomationValidationError> {
    if !matches!(&target, RunTarget::Git(_)) {
        return Err(AutomationValidationError::UnsupportedTarget {
            kind: target.kind_name().to_string(),
        });
    }
    target
        .canonical()
        .map_err(|source| AutomationValidationError::InvalidTarget { source })
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
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use fabro_types::{
        GitCoordinateValidationError, GitRunTarget, RunTarget, TargetValidationError,
    };

    use crate::{
        ApiTrigger, Automation, AutomationGitWorkflowSource, AutomationId, AutomationReplace,
        AutomationStoreError, AutomationTrigger, AutomationTriggerId, AutomationValidationError,
        ScheduleTrigger,
    };

    fn target() -> RunTarget {
        RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "main".to_string(),
            tag:    None,
            sha:    None,
        })
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

    fn workflow_source(
        branch: &str,
        tag: Option<&str>,
        sha: Option<&str>,
    ) -> AutomationGitWorkflowSource {
        AutomationGitWorkflowSource {
            repo:   "fabro-sh/workflows".to_string(),
            branch: branch.to_string(),
            tag:    tag.map(str::to_string),
            sha:    sha.map(str::to_string),
        }
    }

    fn replace_with_source(
        workflow_source: Option<AutomationGitWorkflowSource>,
    ) -> AutomationReplace {
        AutomationReplace {
            name: "Nightly".to_string(),
            description: None,
            environment_id: Some("default".to_string()),
            target: target(),
            workflow: "release".to_string(),
            workflow_source,
            triggers: vec![api_trigger("manual")],
        }
    }

    #[test]
    fn omitted_workflow_source_preserves_canonical_bytes_and_revision() {
        let expected = concat!(
            "name = \"Nightly\"\n",
            "environment_id = \"default\"\n",
            "workflow = \"release\"\n",
            "\n",
            "[target]\n",
            "kind = \"git\"\n",
            "repo = \"fabro-sh/fabro\"\n",
            "branch = \"main\"\n",
            "\n",
            "[[triggers]]\n",
            "type = \"api\"\n",
            "id = \"manual\"\n",
            "enabled = true\n",
        );

        let (automation, bytes) = Automation::from_replace(
            AutomationId::new("nightly").unwrap(),
            replace_with_source(None),
        )
        .unwrap();

        assert_eq!(automation.workflow_source, None);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(
            automation.revision.as_str(),
            "bc26bacc9ed091f4f171c8fdaf45cd319549a50b4291a5c93205028094abf385"
        );

        let decoded =
            Automation::from_toml_bytes(AutomationId::new("nightly").unwrap(), expected.as_bytes())
                .unwrap();
        assert_eq!(decoded.workflow_source, None);
        assert_eq!(
            serde_json::to_value(decoded)
                .unwrap()
                .get("workflow_source"),
            None
        );
    }

    #[test]
    fn workflow_sources_round_trip_and_shas_are_canonicalized() {
        for (source, expected_sha) in [
            (workflow_source("main", None, None), None),
            (workflow_source("main", Some("release/v1"), None), None),
            (
                workflow_source(
                    "context-only",
                    Some("release/v1"),
                    Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01"),
                ),
                Some("abcdef0123456789abcdef0123456789abcdef01"),
            ),
        ] {
            let (automation, bytes) = Automation::from_replace(
                AutomationId::new("nightly").unwrap(),
                replace_with_source(Some(source)),
            )
            .unwrap();

            let source = automation.workflow_source.as_ref().unwrap();
            assert_eq!(source.sha.as_deref(), expected_sha);
            assert!(
                String::from_utf8(bytes.clone())
                    .unwrap()
                    .contains("[workflow_source]")
            );

            let decoded =
                Automation::from_toml_bytes(AutomationId::new("nightly").unwrap(), &bytes).unwrap();
            assert_eq!(decoded.workflow_source, automation.workflow_source);
        }
    }

    #[test]
    fn explicit_workflow_source_changes_revision_and_is_never_collapsed() {
        let (omitted, _) = Automation::from_replace(
            AutomationId::new("nightly").unwrap(),
            replace_with_source(None),
        )
        .unwrap();
        let mut explicit_source = workflow_source("main", None, None);
        explicit_source.repo = "FABRO-SH/FABRO".to_string();
        let (explicit, _) = Automation::from_replace(
            AutomationId::new("nightly").unwrap(),
            replace_with_source(Some(explicit_source.clone())),
        )
        .unwrap();

        assert_ne!(explicit.revision, omitted.revision);
        assert_eq!(explicit.workflow_source, Some(explicit_source));
    }

    #[test]
    fn workflow_source_validation_reports_the_invalid_coordinate_part() {
        let cases = [
            (
                workflow_source("main", None, None),
                GitCoordinateValidationError::Repository,
            ),
            (
                workflow_source("refs/heads/main", None, None),
                GitCoordinateValidationError::Branch,
            ),
            (
                workflow_source("main", Some("tags/v1"), None),
                GitCoordinateValidationError::Tag,
            ),
            (
                workflow_source("main", None, Some("short")),
                GitCoordinateValidationError::Sha,
            ),
        ];

        for (mut source, expected) in cases {
            if expected == GitCoordinateValidationError::Repository {
                source.repo = "not/a/github/slug".to_string();
            }
            let error = Automation::from_replace(
                AutomationId::new("nightly").unwrap(),
                replace_with_source(Some(source)),
            )
            .unwrap_err();
            let AutomationStoreError::Validation { source } = error else {
                panic!("expected validation error");
            };
            assert!(matches!(
                source,
                AutomationValidationError::InvalidWorkflowSource { source }
                    if source == expected
            ));
        }
    }

    #[test]
    fn persisted_toml_applies_defaults_and_canonicalizes_without_id_or_revision() {
        let bytes = br#"
name = "Nightly"
workflow = "release"

[target]
kind = "git"
repo = "fabro-sh/fabro"
branch = "main"

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

        let persisted = super::parse_persisted(bytes, None).unwrap();
        let toml = String::from_utf8(super::canonical_bytes(&persisted).unwrap()).unwrap();
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
workflow = "release"

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
                name:            "Nightly".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        ".fabro/workflows/test/workflow.toml".to_string(),
                workflow_source: None,
                triggers:        vec![
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
    fn invalid_git_target_preserves_the_shared_validation_error() {
        let error = super::validate_target(RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "main;rm".to_string(),
            tag:    None,
            sha:    None,
        }))
        .unwrap_err();

        assert!(matches!(&error, AutomationValidationError::InvalidTarget {
            source: TargetValidationError::Branch,
        }));
        assert_eq!(error.to_string(), "automation Git target is invalid");
    }

    #[test]
    fn non_git_targets_are_rejected_with_their_kind() {
        let error = super::validate_target(RunTarget::None {}).unwrap_err();

        assert!(matches!(
            error,
            AutomationValidationError::UnsupportedTarget { kind } if kind == "none"
        ));
    }

    #[test]
    fn validation_rejects_invalid_inputs() {
        let cases = [
            AutomationReplace {
                name:            " ".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:            "Bad repo".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          RunTarget::Git(GitRunTarget {
                    repo:   "not/github/slug".to_string(),
                    branch: "main".to_string(),
                    tag:    None,
                    sha:    None,
                }),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:            "Bad ref".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          RunTarget::Git(GitRunTarget {
                    repo:   "fabro-sh/fabro".to_string(),
                    branch: "main;rm".to_string(),
                    tag:    None,
                    sha:    None,
                }),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:            "Bad workflow".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "../release".to_string(),
                workflow_source: None,
                triggers:        vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:            "Duplicate trigger".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![
                    api_trigger("manual"),
                    schedule_trigger("manual", "0 0 * * *"),
                ],
            },
            AutomationReplace {
                name:            "Two API triggers".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![api_trigger("one"), api_trigger("two")],
            },
            AutomationReplace {
                name:            "Six field cron".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![schedule_trigger("nightly", "0 0 0 * * *")],
            },
            AutomationReplace {
                name:            "Bad cron".to_string(),
                description:     None,
                environment_id:  Some("default".to_string()),
                target:          target(),
                workflow:        "release".to_string(),
                workflow_source: None,
                triggers:        vec![schedule_trigger("nightly", "99 0 * * *")],
            },
        ];

        for case in cases {
            assert!(Automation::from_replace(AutomationId::new("test").unwrap(), case).is_err());
        }
    }

    fn top_level_lines(toml: &str) -> impl Iterator<Item = &str> {
        toml.lines().take_while(|line| !line.starts_with('['))
    }
}
