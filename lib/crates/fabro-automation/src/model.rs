use std::collections::HashSet;

use croner::parser::{CronParser, Seconds, Year};
use serde::{Deserialize, Serialize};

use crate::{
    AutomationId, AutomationRevision, AutomationStoreError, AutomationTriggerId,
    AutomationValidationError,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Automation {
    pub id:          AutomationId,
    pub revision:    AutomationRevision,
    pub name:        String,
    pub description: Option<String>,
    pub enabled:     bool,
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
        validate_fields(&draft)?;
        let persisted = PersistedAutomation::from(draft.clone());
        let bytes = canonical_bytes(&persisted)?;
        let revision = AutomationRevision::from_bytes(&bytes);
        let automation = Self::from_validated_replace(id, revision, draft);
        Ok((automation, bytes))
    }

    pub(crate) fn to_persisted(&self) -> PersistedAutomation {
        PersistedAutomation {
            name:        self.name.clone(),
            description: self.description.clone(),
            enabled:     self.enabled,
            target:      self.target.clone(),
            triggers:    self.triggers.clone(),
        }
    }

    pub fn to_toml_string(&self) -> Result<String, AutomationStoreError> {
        toml::to_string_pretty(&self.to_persisted()).map_err(AutomationStoreError::from)
    }

    fn from_persisted(
        id: AutomationId,
        revision: AutomationRevision,
        persisted: PersistedAutomation,
    ) -> Result<Self, AutomationValidationError> {
        let replace = AutomationReplace::from(persisted);
        validate_fields(&replace)?;
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
            enabled: replace.enabled,
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
    pub id:          AutomationId,
    pub name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled:     bool,
    pub target:      AutomationTarget,
    pub triggers:    Vec<AutomationTrigger>,
}

impl From<AutomationDraft> for (AutomationId, AutomationReplace) {
    fn from(value: AutomationDraft) -> Self {
        (value.id, AutomationReplace {
            name:        value.name,
            description: value.description,
            enabled:     value.enabled,
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
    pub enabled:     bool,
    pub target:      AutomationTarget,
    pub triggers:    Vec<AutomationTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedAutomation {
    name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default = "default_true")]
    enabled:     bool,
    target:      AutomationTarget,
    #[serde(default)]
    triggers:    Vec<AutomationTrigger>,
}

impl From<AutomationReplace> for PersistedAutomation {
    fn from(value: AutomationReplace) -> Self {
        Self {
            name:        value.name,
            description: value.description,
            enabled:     value.enabled,
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
            enabled:     value.enabled,
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

fn validate_repository_slug(value: &str) -> Result<(), AutomationValidationError> {
    let Some((owner, repo)) = value.split_once('/') else {
        return Err(AutomationValidationError::InvalidRepositorySlug {
            value: value.to_string(),
        });
    };
    if repo.contains('/') || !valid_github_owner(owner) || !valid_github_repo(repo) {
        return Err(AutomationValidationError::InvalidRepositorySlug {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn valid_github_owner(value: &str) -> bool {
    if value.is_empty() || value.len() > 39 {
        return false;
    }
    let bytes = value.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    (first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

fn valid_github_repo(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_git_ref_selector(value: &str) -> Result<(), AutomationValidationError> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value.trim() == value
        && !value.starts_with(['/', '-', '.'])
        && !value.ends_with(['/', '.'])
        && !has_lock_suffix(value)
        && value != "@"
        && !value.contains("..")
        && !value.contains("//")
        && !value.contains("@{")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        && value
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !has_lock_suffix(part));
    if valid {
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

fn has_lock_suffix(value: &str) -> bool {
    value
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension == "lock")
}

fn validate_triggers(triggers: &[AutomationTrigger]) -> Result<(), AutomationValidationError> {
    let mut seen = HashSet::new();
    let mut has_api_trigger = false;
    let cron_parser = CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build();

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
                cron_parser.parse(&trigger.expression).map_err(|source| {
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

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use crate::{
        ApiTrigger, Automation, AutomationId, AutomationReplace, AutomationTarget,
        AutomationTrigger, AutomationTriggerId, ScheduleTrigger,
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
        AutomationTrigger::Schedule(ScheduleTrigger {
            id:         AutomationTriggerId::new(id).unwrap(),
            enabled:    true,
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
        assert!(automation.enabled);
        assert!(automation.triggers.iter().all(AutomationTrigger::enabled));

        let toml = automation.to_toml_string().unwrap();
        assert!(!top_level_lines(&toml).any(|line| line.starts_with("id = ")));
        assert!(!top_level_lines(&toml).any(|line| line.starts_with("revision = ")));
        assert!(toml.contains("enabled = true"));
        assert!(toml.contains("type = \"api\""));
    }

    #[test]
    fn validation_rejects_invalid_inputs() {
        let cases = [
            AutomationReplace {
                name:        " ".to_string(),
                description: None,
                enabled:     true,
                target:      target(),
                triggers:    vec![api_trigger("manual")],
            },
            AutomationReplace {
                name:        "Bad repo".to_string(),
                description: None,
                enabled:     true,
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
                enabled:     true,
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
                enabled:     true,
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
                enabled:     true,
                target:      target(),
                triggers:    vec![
                    api_trigger("manual"),
                    schedule_trigger("manual", "0 0 * * *"),
                ],
            },
            AutomationReplace {
                name:        "Two API triggers".to_string(),
                description: None,
                enabled:     true,
                target:      target(),
                triggers:    vec![api_trigger("one"), api_trigger("two")],
            },
            AutomationReplace {
                name:        "Six field cron".to_string(),
                description: None,
                enabled:     true,
                target:      target(),
                triggers:    vec![schedule_trigger("nightly", "0 0 0 * * *")],
            },
            AutomationReplace {
                name:        "Bad cron".to_string(),
                description: None,
                enabled:     true,
                target:      target(),
                triggers:    vec![schedule_trigger("nightly", "99 0 * * *")],
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
