//! Run domain.
//!
//! `[run]` is the shared execution domain. It may appear in all three config
//! files and layer normally. Subdomains cover model selection, git author,
//! prepare steps, execution posture, checkpoint policy, environment selection,
//! notifications, interviews, agent knobs, hooks, SCM targeting, pull-request
//! behavior, and artifact collection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use serde::de::{self, Deserializer};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

use super::duration::Duration;
use super::interp::{InterpString, Namespace, ResolveError};
use super::model_ref::ModelRef;
use super::size::Size;

/// A structurally resolved `[run]` view for consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNamespace {
    pub goal:          Option<RunGoal>,
    pub working_dir:   Option<String>,
    pub metadata:      HashMap<String, String>,
    pub inputs:        HashMap<String, toml::Value>,
    pub model:         RunModelSettings,
    pub git:           RunGitSettings,
    pub prepare:       RunPrepareSettings,
    pub execution:     RunExecutionSettings,
    pub checkpoint:    RunCheckpointSettings,
    pub clone:         RunCloneSettings,
    pub run_branch:    RunBranchSettings,
    pub meta_branch:   RunMetaBranchSettings,
    pub environment:   RunEnvironmentSettings,
    pub notifications: HashMap<String, NotificationRouteSettings>,
    pub interviews:    RunInterviewsSettings,
    pub agent:         RunAgentSettings,
    pub hooks:         Vec<HookDefinition>,
    pub scm:           RunScmSettings,
    pub pull_request:  Option<PullRequestSettings>,
    pub artifacts:     ArtifactsSettings,
    pub integrations:  RunIntegrationsSettings,
}

#[expect(
    clippy::derivable_impls,
    reason = "run defaults are product policy; keep the true-valued branch defaults visible here"
)]
impl Default for RunNamespace {
    fn default() -> Self {
        Self {
            goal:          None,
            working_dir:   None,
            metadata:      HashMap::new(),
            inputs:        HashMap::new(),
            model:         RunModelSettings::default(),
            git:           RunGitSettings::default(),
            prepare:       RunPrepareSettings::default(),
            execution:     RunExecutionSettings::default(),
            checkpoint:    RunCheckpointSettings::default(),
            clone:         RunCloneSettings::default(),
            run_branch:    RunBranchSettings::default(),
            meta_branch:   RunMetaBranchSettings::default(),
            environment:   RunEnvironmentSettings::default(),
            notifications: HashMap::new(),
            interviews:    RunInterviewsSettings::default(),
            agent:         RunAgentSettings::default(),
            hooks:         Vec::new(),
            scm:           RunScmSettings::default(),
            pull_request:  None,
            artifacts:     ArtifactsSettings::default(),
            integrations:  RunIntegrationsSettings::default(),
        }
    }
}

impl RunNamespace {
    pub fn substitute_variables<F>(&mut self, mut lookup: F) -> Result<(), ResolveError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        substitute_goal(&mut self.goal, &mut lookup)?;
        substitute_string_map(&mut self.metadata, &mut lookup)?;
        // run.working_dir, run.model.provider/name, and run.git.author.* were
        // demoted to plain `String` and removed from this pass (D2/D11):
        // values stay literal.
        substitute_option_string(&mut self.model.controls.reasoning_effort, &mut lookup)?;
        substitute_option_string(&mut self.model.controls.speed, &mut lookup)?;
        substitute_string_vec(&mut self.checkpoint.exclude_globs, &mut lookup)?;
        substitute_environment(&mut self.environment, &mut lookup)?;
        substitute_map(&mut self.environment.env, &mut lookup)?;
        for route in self.notifications.values_mut() {
            substitute_option_string(&mut route.provider, &mut lookup)?;
            substitute_string_vec(&mut route.events, &mut lookup)?;
            if let Some(slack) = &mut route.slack {
                substitute_option(&mut slack.channel, &mut lookup)?;
            }
        }
        substitute_option_string(&mut self.interviews.provider, &mut lookup)?;
        if let Some(slack) = &mut self.interviews.slack {
            substitute_option(&mut slack.channel, &mut lookup)?;
        }
        substitute_map(&mut self.integrations.github.permissions, &mut lookup)?;
        // run.scm.owner/repository were demoted and removed from this pass
        // (D2): values stay literal.
        substitute_string_vec(&mut self.prepare.commands, &mut lookup)?;
        // Only resolved inline servers carry substitutable templates; an
        // unresolved reference holds just an id + enabled flag.
        for entry in self.agent.mcps.values_mut() {
            if let ResolvedMcpEntry::Resolved(mcp) = entry {
                substitute_string(&mut mcp.name, &mut lookup)?;
                substitute_mcp_transport(&mut mcp.transport, &mut lookup)?;
            }
        }
        for hook in &mut self.hooks {
            substitute_option_string(&mut hook.name, &mut lookup)?;
            substitute_option_string(&mut hook.command, &mut lookup)?;
            substitute_option_string(&mut hook.matcher, &mut lookup)?;
            if let Some(hook_type) = &mut hook.hook_type {
                substitute_hook_type(hook_type, &mut lookup)?;
            }
        }
        substitute_option_string(&mut self.scm.provider, &mut lookup)?;
        substitute_string_vec(&mut self.artifacts.include, &mut lookup)?;
        Ok(())
    }
}

fn substitute_goal<F>(goal: &mut Option<RunGoal>, lookup: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    match goal {
        Some(RunGoal::Inline(value) | RunGoal::File(value)) => substitute(value, lookup),
        None => Ok(()),
    }
}

fn substitute_option<F>(
    value: &mut Option<InterpString>,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    match value {
        Some(value) => substitute(value, lookup),
        None => Ok(()),
    }
}

fn substitute_map<F>(
    values: &mut HashMap<String, InterpString>,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    for value in values.values_mut() {
        substitute(value, lookup)?;
    }
    Ok(())
}

fn substitute<F>(value: &mut InterpString, lookup: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    if !value.references(Namespace::Vars) {
        return Ok(());
    }
    *value = value.substitute_variables(lookup)?;
    Ok(())
}

fn substitute_string<F>(value: &mut String, lookup: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    if !may_reference_variable(value) {
        return Ok(());
    }
    if let std::borrow::Cow::Owned(substituted) =
        InterpString::substitute_variables_in_str_cow(value, lookup)?
    {
        *value = substituted;
    }
    Ok(())
}

fn may_reference_variable(value: &str) -> bool {
    value.contains("{{") && value.contains("vars.")
}

fn substitute_option_string<F>(
    value: &mut Option<String>,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    match value {
        Some(value) => substitute_string(value, lookup),
        None => Ok(()),
    }
}

fn substitute_string_vec<F>(values: &mut [String], lookup: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    for value in values {
        substitute_string(value, lookup)?;
    }
    Ok(())
}

fn substitute_string_map<F>(
    values: &mut HashMap<String, String>,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    for value in values.values_mut() {
        substitute_string(value, lookup)?;
    }
    Ok(())
}

fn substitute_mcp_transport<F>(
    transport: &mut McpTransport,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    visit_mcp_transport_strings(transport, &mut |value| {
        substitute_string(value, &mut *lookup)
    })
}

fn visit_mcp_transport_strings<F>(
    transport: &mut McpTransport,
    visitor: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&mut String) -> Result<(), ResolveError>,
{
    match transport {
        McpTransport::Stdio { command, env } | McpTransport::Sandbox { command, env, .. } => {
            visit_string_vec(command, visitor)?;
            visit_string_map(env, visitor)
        }
        McpTransport::Http { url, headers, .. } => {
            visitor(url)?;
            visit_string_map(headers, visitor)
        }
    }
}

fn visit_string_vec<F>(values: &mut [String], visitor: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&mut String) -> Result<(), ResolveError>,
{
    for value in values {
        visitor(value)?;
    }
    Ok(())
}

fn visit_string_map<F>(
    values: &mut HashMap<String, String>,
    visitor: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&mut String) -> Result<(), ResolveError>,
{
    for value in values.values_mut() {
        visitor(value)?;
    }
    Ok(())
}

fn substitute_environment<F>(
    environment: &mut RunEnvironmentSettings,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    substitute_string(&mut environment.id, lookup)?;
    substitute_option_string(&mut environment.image.docker, lookup)?;
    substitute_dockerfile_source(&mut environment.image.dockerfile, lookup)?;
    substitute_string_vec(&mut environment.network.allow, lookup)?;
    substitute_string_map(&mut environment.labels, lookup)?;
    Ok(())
}

fn substitute_dockerfile_source<F>(
    source: &mut Option<DockerfileSource>,
    lookup: &mut F,
) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    match source {
        Some(DockerfileSource::Inline(value) | DockerfileSource::Path { path: value }) => {
            substitute_string(value, lookup)
        }
        None => Ok(()),
    }
}

fn substitute_hook_type<F>(hook_type: &mut HookType, lookup: &mut F) -> Result<(), ResolveError>
where
    F: FnMut(&str) -> Option<String>,
{
    match hook_type {
        HookType::Command { command } => substitute_string(command, lookup),
        HookType::Http { url, headers, .. } => {
            substitute_string(url, lookup)?;
            if let Some(headers) = headers {
                substitute_string_map(headers, lookup)?;
            }
            Ok(())
        }
        HookType::Prompt { prompt, model } | HookType::Agent { prompt, model, .. } => {
            substitute_string(prompt, lookup)?;
            substitute_option_string(model, lookup)
        }
    }
}

#[cfg(test)]
mod run_namespace_variable_substitution_tests {
    use std::collections::HashMap;

    use super::{
        ArtifactsSettings, DockerfileSource, EnvironmentImageSettings, EnvironmentNetworkMode,
        EnvironmentNetworkSettings, HookDefinition, HookEvent, HookType, InterpString,
        McpHttpProtocol, McpServerSettings, McpTransport, RunCheckpointSettings,
        RunEnvironmentSettings, RunGoal, RunNamespace, RunPrepareSettings,
    };

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[test]
    fn substitutes_variables_in_interp_and_late_bound_run_strings() {
        let mut run = RunNamespace {
            goal: Some(RunGoal::Inline(InterpString::parse(
                "deploy {{ vars.ENV }} in {{ env.REGION }}",
            ))),
            prepare: RunPrepareSettings {
                commands:   vec!["echo {{ vars.ENV }} {{ env.REGION }}".to_string()],
                timeout_ms: 1_000,
            },
            agent: super::RunAgentSettings {
                mcps: HashMap::from([(
                    "http".to_string(),
                    super::ResolvedMcpEntry::Resolved(McpServerSettings {
                        name:                 "http".to_string(),
                        transport:            McpTransport::Http {
                            protocol: McpHttpProtocol::default(),
                            url:      "https://{{ vars.HOST }}/mcp".to_string(),
                            headers:  HashMap::from([(
                                "X-Env".to_string(),
                                "{{ vars.ENV }}".to_string(),
                            )]),
                        },
                        current_dir:          None,
                        clear_env:            false,
                        startup_timeout_secs: 10,
                        tool_timeout_secs:    60,
                    }),
                )]),
                ..super::RunAgentSettings::default()
            },
            hooks: vec![HookDefinition {
                name:       Some("notify".to_string()),
                event:      HookEvent::RunComplete,
                command:    None,
                hook_type:  Some(HookType::Http {
                    url:              "https://hooks.example/{{ vars.ENV }}".to_string(),
                    headers:          Some(HashMap::from([(
                        "X-Env".to_string(),
                        "{{ vars.ENV }}".to_string(),
                    )])),
                    allowed_env_vars: Vec::new(),
                    tls:              super::TlsMode::Verify,
                }),
                matcher:    None,
                blocking:   None,
                timeout_ms: None,
                sandbox:    None,
            }],
            ..RunNamespace::default()
        };

        run.substitute_variables(|name| match name {
            "ENV" => Some("prod".to_string()),
            "HOST" => Some("mcp.example".to_string()),
            _ => None,
        })
        .unwrap();

        let goal_source = match run.goal.as_ref() {
            Some(RunGoal::Inline(value) | RunGoal::File(value)) => Some(value.as_source()),
            None => None,
        };
        assert_eq!(
            goal_source,
            Some("deploy prod in {{ env.REGION }}".to_string())
        );
        assert_eq!(run.prepare.commands, vec![
            "echo prod {{ env.REGION }}".to_string()
        ]);
        let mcp = run.agent.mcps["http"]
            .as_resolved()
            .expect("expected resolved inline mcp entry");
        match &mcp.transport {
            McpTransport::Http { url, headers, .. } => {
                assert_eq!(url, "https://mcp.example/mcp");
                assert_eq!(headers.get("X-Env").map(String::as_str), Some("prod"));
            }
            other => panic!("expected http mcp transport, got {other:?}"),
        }
        match run.hooks[0].hook_type.as_ref().unwrap() {
            HookType::Http { url, headers, .. } => {
                assert_eq!(url, "https://hooks.example/prod");
                assert_eq!(
                    headers
                        .as_ref()
                        .and_then(|headers| headers.get("X-Env"))
                        .map(String::as_str),
                    Some("prod")
                );
            }
            other => panic!("expected http hook type, got {other:?}"),
        }
    }

    #[test]
    fn demoted_fields_do_not_interpolate() {
        // Demoted fields (run.working_dir, run.model.*, run.git.author.*,
        // run.scm.owner/repository) were removed from the vars pass (D2/D11):
        // `{{ vars.* }}` and `{{ env.* }}` stay literal even when a value is
        // available.
        let mut run = RunNamespace {
            working_dir: Some("/workspace/{{ vars.ENV }}".to_string()),
            model: super::RunModelSettings {
                provider: Some("{{ vars.PROVIDER }}".to_string()),
                name: Some("{{ vars.MODEL }}".to_string()),
                ..super::RunModelSettings::default()
            },
            git: super::RunGitSettings {
                author: Some(super::GitAuthorSettings {
                    name:  Some("{{ vars.AUTHOR }}".to_string()),
                    email: Some("{{ env.EMAIL }}".to_string()),
                }),
            },
            scm: super::RunScmSettings {
                owner: Some("{{ vars.OWNER }}".to_string()),
                repository: Some("{{ env.REPO }}".to_string()),
                ..super::RunScmSettings::default()
            },
            ..RunNamespace::default()
        };

        // Even with every variable available, demoted fields stay literal.
        run.substitute_variables(|_| Some("SUBSTITUTED".to_string()))
            .unwrap();

        assert_eq!(
            run.working_dir.as_deref(),
            Some("/workspace/{{ vars.ENV }}")
        );
        assert_eq!(run.model.provider.as_deref(), Some("{{ vars.PROVIDER }}"));
        assert_eq!(run.model.name.as_deref(), Some("{{ vars.MODEL }}"));
        let author = run.git.author.as_ref().unwrap();
        assert_eq!(author.name.as_deref(), Some("{{ vars.AUTHOR }}"));
        assert_eq!(author.email.as_deref(), Some("{{ env.EMAIL }}"));
        assert_eq!(run.scm.owner.as_deref(), Some("{{ vars.OWNER }}"));
        assert_eq!(run.scm.repository.as_deref(), Some("{{ env.REPO }}"));
    }

    #[test]
    fn substitutes_variables_in_string_backed_settings_families() {
        let mut run = RunNamespace {
            checkpoint: RunCheckpointSettings {
                exclude_globs:  vec!["tmp/{{ vars.ENV }}/**".to_string()],
                skip_git_hooks: false,
            },
            environment: RunEnvironmentSettings {
                image: EnvironmentImageSettings {
                    docker:     Some("registry.example/{{ vars.ENV }}:latest".to_string()),
                    dockerfile: Some(DockerfileSource::Inline(
                        "FROM registry.example/base:{{ vars.ENV }}".to_string(),
                    )),
                },
                network: EnvironmentNetworkSettings {
                    mode:  EnvironmentNetworkMode::CidrAllowList,
                    allow: vec!["{{ vars.CIDR }}".to_string()],
                },
                labels: HashMap::from([("deploy-env".to_string(), "{{ vars.ENV }}".to_string())]),
                ..RunEnvironmentSettings::default()
            },
            artifacts: ArtifactsSettings {
                include: vec!["reports/{{ vars.ENV }}/**".to_string()],
            },
            ..RunNamespace::default()
        };

        run.substitute_variables(|name| match name {
            "CIDR" => Some("10.0.0.0/8".to_string()),
            "ENV" => Some("prod".to_string()),
            _ => None,
        })
        .unwrap();

        assert_eq!(run.checkpoint.exclude_globs, vec!["tmp/prod/**"]);
        assert_eq!(
            run.environment.image.docker.as_deref(),
            Some("registry.example/prod:latest")
        );
        assert_eq!(
            run.environment.image.dockerfile,
            Some(DockerfileSource::Inline(
                "FROM registry.example/base:prod".to_string()
            ))
        );
        assert_eq!(run.environment.network.allow, vec!["10.0.0.0/8"]);
        assert_eq!(
            run.environment.labels.get("deploy-env").map(String::as_str),
            Some("prod")
        );
        assert_eq!(run.artifacts.include, vec!["reports/prod/**"]);
    }
}

/// `[run.integrations]` — run-level integration knobs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunIntegrationsSettings {
    pub github: RunIntegrationsGithubSettings,
}

/// `[run.integrations.github]` — runtime GitHub token shape.
///
/// `permissions` is empty when no token should be requested. The
/// presence-vs-clear distinction is only meaningful at the layer-merge
/// stage; the resolved form collapses both `None` and `Some({})` into an
/// empty map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunIntegrationsGithubSettings {
    pub permissions: HashMap<String, InterpString>,
}

impl RunIntegrationsGithubSettings {
    /// Whether the run config asks Fabro to mint a `GITHUB_TOKEN` for the
    /// sandbox. Empty `permissions` means "no token requested" — the only
    /// resolved-form sentinel for this state.
    pub fn is_token_requested(&self) -> bool {
        !self.permissions.is_empty()
    }

    /// Resolve every `permissions` value's `{{ env.* }}` tokens via
    /// `lookup`, falling back to the raw template source when resolution
    /// fails so callers see a recognizable diagnostic instead of a
    /// silently dropped key. The `lookup` seam keeps tests free of
    /// process-env coupling; production callers pass a thin wrapper over
    /// `std::env::var`.
    pub fn resolve_permissions<F>(&self, mut lookup: F) -> HashMap<String, String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        self.permissions
            .iter()
            .map(|(name, value)| (name.clone(), value.resolve_or_source(&mut lookup)))
            .collect()
    }
}

#[cfg(test)]
mod run_integrations_github_tests {
    use super::{HashMap, InterpString, RunIntegrationsGithubSettings};

    fn settings(permissions: &[(&str, &str)]) -> RunIntegrationsGithubSettings {
        RunIntegrationsGithubSettings {
            permissions: permissions
                .iter()
                .map(|(k, v)| ((*k).to_string(), InterpString::parse(v)))
                .collect(),
        }
    }

    #[test]
    fn is_token_requested_reflects_permissions_presence() {
        assert!(!settings(&[]).is_token_requested());
        assert!(settings(&[("issues", "read")]).is_token_requested());
    }

    #[test]
    fn resolve_permissions_substitutes_env_tokens_via_lookup() {
        let s = settings(&[("issues", "{{ env.GH_PERM_LEVEL }}"), ("contents", "read")]);
        let resolved = s.resolve_permissions(|name| match name {
            "GH_PERM_LEVEL" => Some("write".to_string()),
            _ => None,
        });
        assert_eq!(resolved.get("issues"), Some(&"write".to_string()));
        assert_eq!(resolved.get("contents"), Some(&"read".to_string()));
    }

    #[test]
    fn resolve_permissions_falls_back_to_source_when_lookup_fails() {
        let s = settings(&[("issues", "{{ env.GH_PERM_MISSING }}")]);
        let resolved = s.resolve_permissions(|_| None);
        assert_eq!(
            resolved.get("issues"),
            Some(&"{{ env.GH_PERM_MISSING }}".to_string())
        );
    }

    #[test]
    fn resolve_permissions_is_empty_for_empty_settings() {
        let s: HashMap<String, String> = settings(&[]).resolve_permissions(|_| None);
        assert!(s.is_empty());
    }
}

/// The resolved source of a run goal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RunGoal {
    Inline(InterpString),
    File(InterpString),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunModelSettings {
    pub provider:  Option<String>,
    pub name:      Option<String>,
    pub fallbacks: Vec<ModelRef>,
    /// Run-level default values for typed model controls
    /// (`reasoning_effort`, `speed`). Node and style attributes still win
    /// over these defaults.
    #[serde(default)]
    pub controls:  RunModelControls,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunModelControls {
    pub reasoning_effort: Option<String>,
    pub speed:            Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunGitSettings {
    pub author: Option<GitAuthorSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GitAuthorSettings {
    pub name:  Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPrepareSettings {
    pub commands:   Vec<String>,
    pub timeout_ms: u64,
}

impl Default for RunPrepareSettings {
    fn default() -> Self {
        Self {
            commands:   Vec::new(),
            timeout_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunExecutionSettings {
    pub mode:     RunMode,
    pub approval: ApprovalMode,
}

impl Default for RunExecutionSettings {
    fn default() -> Self {
        Self {
            mode:     RunMode::Normal,
            approval: ApprovalMode::Prompt,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunCheckpointSettings {
    pub exclude_globs:  Vec<String>,
    /// When `true`, Fabro-managed run-branch checkpoint commits bypass
    /// local Git commit hooks (e.g. `pre-commit`, `commit-msg`). This does
    /// not affect Fabro workflow `[[run.hooks]]` or metadata-branch
    /// snapshots, which already bypass repository hooks.
    #[serde(default)]
    pub skip_git_hooks: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCloneSettings {
    pub enabled: bool,
}

impl Default for RunCloneSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunBranchSettings {
    pub enabled: bool,
    pub push:    bool,
}

impl Default for RunBranchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            push:    true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMetaBranchSettings {
    pub enabled: bool,
    pub push:    bool,
}

impl Default for RunMetaBranchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            push:    true,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum EnvironmentProvider {
    #[default]
    Local,
    Docker,
    Daytona,
}

impl EnvironmentProvider {
    #[must_use]
    pub fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }

    #[must_use]
    pub fn is_clone_based(self) -> bool {
        matches!(self, Self::Docker | Self::Daytona)
    }
}

impl From<EnvironmentProvider> for crate::SandboxProviderKind {
    fn from(value: EnvironmentProvider) -> Self {
        match value {
            EnvironmentProvider::Local => Self::Local,
            EnvironmentProvider::Docker => Self::Docker,
            EnvironmentProvider::Daytona => Self::Daytona,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum EnvironmentNetworkMode {
    #[default]
    AllowAll,
    Block,
    CidrAllowList,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentImageSettings {
    pub docker:     Option<String>,
    pub dockerfile: Option<DockerfileSource>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentResourcesSettings {
    pub cpu:    Option<i32>,
    pub memory: Option<Size>,
    pub disk:   Option<Size>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentNetworkSettings {
    pub mode:  EnvironmentNetworkMode,
    pub allow: Vec<String>,
}

impl Default for EnvironmentNetworkSettings {
    fn default() -> Self {
        Self {
            mode:  EnvironmentNetworkMode::AllowAll,
            allow: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentLifecycleSettings {
    pub preserve:         bool,
    #[serde(default = "default_stop_on_terminal")]
    pub stop_on_terminal: bool,
    pub auto_stop:        Option<Duration>,
}

fn default_stop_on_terminal() -> bool {
    true
}

impl Default for EnvironmentLifecycleSettings {
    fn default() -> Self {
        Self {
            preserve:         false,
            stop_on_terminal: true,
            auto_stop:        None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSettings {
    pub provider:  EnvironmentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd:       Option<String>,
    pub image:     EnvironmentImageSettings,
    pub resources: EnvironmentResourcesSettings,
    pub network:   EnvironmentNetworkSettings,
    pub lifecycle: EnvironmentLifecycleSettings,
    pub labels:    HashMap<String, String>,
    pub env:       HashMap<String, InterpString>,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self {
            provider:  EnvironmentProvider::Local,
            cwd:       None,
            image:     EnvironmentImageSettings::default(),
            resources: EnvironmentResourcesSettings::default(),
            network:   EnvironmentNetworkSettings::default(),
            lifecycle: EnvironmentLifecycleSettings::default(),
            labels:    HashMap::new(),
            env:       HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEnvironmentSettings {
    pub id:        String,
    pub provider:  EnvironmentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd:       Option<String>,
    pub image:     EnvironmentImageSettings,
    pub resources: EnvironmentResourcesSettings,
    pub network:   EnvironmentNetworkSettings,
    pub lifecycle: EnvironmentLifecycleSettings,
    pub labels:    HashMap<String, String>,
    pub env:       HashMap<String, InterpString>,
}

impl RunEnvironmentSettings {
    #[must_use]
    pub fn from_environment(id: String, environment: EnvironmentSettings) -> Self {
        Self {
            id,
            provider: environment.provider,
            cwd: environment.cwd,
            image: environment.image,
            resources: environment.resources,
            network: environment.network,
            lifecycle: environment.lifecycle,
            labels: environment.labels,
            env: environment.env,
        }
    }

    /// Resolve every environment value's `{{ env.* }}` tokens via `lookup`,
    /// falling back to the original source string when resolution fails.
    #[must_use]
    pub fn resolve_env<F>(&self, mut lookup: F) -> HashMap<String, String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        self.env
            .iter()
            .map(|(name, value)| (name.clone(), value.resolve_or_source(&mut lookup)))
            .collect()
    }
}

impl Default for RunEnvironmentSettings {
    fn default() -> Self {
        Self::from_environment("default".to_string(), EnvironmentSettings::default())
    }
}

#[cfg(test)]
mod run_environment_settings_tests {
    use super::{HashMap, InterpString, RunEnvironmentSettings};

    fn settings(env: &[(&str, &str)]) -> RunEnvironmentSettings {
        RunEnvironmentSettings {
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), InterpString::parse(v)))
                .collect(),
            ..RunEnvironmentSettings::default()
        }
    }

    #[test]
    fn resolve_env_substitutes_env_tokens_via_lookup() {
        let s = settings(&[("NODE_ENV", "{{ env.NODE_ENV }}"), ("STATIC", "value")]);
        let resolved = s.resolve_env(|name| match name {
            "NODE_ENV" => Some("test".to_string()),
            _ => None,
        });

        assert_eq!(resolved.get("NODE_ENV"), Some(&"test".to_string()));
        assert_eq!(resolved.get("STATIC"), Some(&"value".to_string()));
    }

    #[test]
    fn resolve_env_falls_back_to_source_when_lookup_fails() {
        let s = settings(&[("NODE_ENV", "{{ env.MISSING_NODE_ENV }}")]);
        let resolved = s.resolve_env(|_| None);

        assert_eq!(
            resolved.get("NODE_ENV"),
            Some(&"{{ env.MISSING_NODE_ENV }}".to_string())
        );
    }

    #[test]
    fn resolve_env_is_empty_for_empty_settings() {
        let s: HashMap<String, String> = settings(&[]).resolve_env(|_| None);
        assert!(s.is_empty());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerfileSource {
    Inline(String),
    Path { path: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DockerfileSourceRepr {
    Inline { value: String },
    Path { path: String },
}

impl Serialize for DockerfileSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DockerfileSource", 2)?;
        match self {
            Self::Inline(value) => {
                state.serialize_field("type", "inline")?;
                state.serialize_field("value", value)?;
            }
            Self::Path { path } => {
                state.serialize_field("type", "path")?;
                state.serialize_field("path", path)?;
            }
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for DockerfileSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match DockerfileSourceRepr::deserialize(deserializer)? {
            DockerfileSourceRepr::Inline { value } => Ok(Self::Inline(value)),
            DockerfileSourceRepr::Path { path } => Ok(Self::Path { path }),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationRouteSettings {
    pub enabled:  bool,
    pub provider: Option<String>,
    pub events:   Vec<String>,
    pub slack:    Option<NotificationProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInterviewsSettings {
    pub provider: Option<String>,
    pub slack:    Option<InterviewProviderSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterviewProviderSettings {
    pub channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunAgentSettings {
    #[serde(default)]
    pub fabro_tools: bool,
    pub permissions: Option<AgentPermissions>,
    pub mcps:        HashMap<String, ResolvedMcpEntry>,
}

/// An MCP entry in a run's agent settings: either an inline resolved server or
/// an unresolved reference to a server-side catalog definition.
///
/// `Resolved` (de)serializes as a **bare** [`McpServerSettings`] - with no enum
/// tag - for backward compatibility with run specs persisted before this enum
/// existed. Serialization stays `#[serde(untagged)]`, and custom
/// deserialization preserves the same wire shapes while rejecting entries that
/// mix catalog-reference fields (`id`, `enabled`) with inline server fields.
///
/// Invariant: no `Reference` survives run creation. References are resolved to
/// `Resolved` on the server's run-preparation path before the run spec is
/// persisted; any `Reference` reaching a post-persistence consumer is a bug.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResolvedMcpEntry {
    Resolved(McpServerSettings),
    Reference(McpServerRef),
}

impl<'de> Deserialize<'de> for ResolvedMcpEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let serde_json::Value::Object(map) = &value else {
            return Err(de::Error::custom("MCP entry must be a table"));
        };

        let has_reference_fields = map.contains_key("id") || map.contains_key("enabled");
        let has_inline_server_fields = map.contains_key("name")
            || map.contains_key("transport")
            || map.contains_key("current_dir")
            || map.contains_key("clear_env")
            || map.contains_key("startup_timeout_secs")
            || map.contains_key("tool_timeout_secs");

        if has_reference_fields && has_inline_server_fields {
            return Err(de::Error::custom(
                "MCP entry cannot mix catalog reference fields (`id`, `enabled`) with inline \
                 server fields",
            ));
        }

        if has_reference_fields {
            serde_json::from_value(value)
                .map(Self::Reference)
                .map_err(de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Resolved)
                .map_err(de::Error::custom)
        }
    }
}

impl ResolvedMcpEntry {
    /// The inline resolved server, or `None` for an unresolved [`Reference`].
    ///
    /// [`Reference`]: ResolvedMcpEntry::Reference
    #[must_use]
    pub fn as_resolved(&self) -> Option<&McpServerSettings> {
        match self {
            Self::Resolved(server) => Some(server),
            Self::Reference(_) => None,
        }
    }
}

/// An unresolved reference to a server-defined MCP catalog entry.
///
/// `id` is kept as a plain `String` to keep `fabro-types` decoupled from the
/// `fabro-mcp-store` crate; the resolver maps it to a store id at resolve time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerRef {
    pub id:      String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod run_agent_settings_tests {
    use super::{
        McpServerRef, McpServerSettings, McpTransport, ResolvedMcpEntry, RunAgentSettings,
    };

    #[test]
    fn deserializes_missing_fabro_tools_as_false() {
        let settings: RunAgentSettings = serde_json::from_value(serde_json::json!({
            "permissions": null,
            "mcps": {}
        }))
        .expect("legacy run agent settings should deserialize");

        assert!(!settings.fabro_tools);
    }

    /// Critical back-compat guarantee: a run spec persisted before
    /// `ResolvedMcpEntry` existed stores `mcps` as a map of bare
    /// `McpServerSettings` (no enum tag). It must still deserialize, with every
    /// entry landing as `ResolvedMcpEntry::Resolved`.
    #[test]
    fn deserializes_old_format_bare_mcps_as_resolved_json() {
        let settings: RunAgentSettings = serde_json::from_value(serde_json::json!({
            "fabro_tools": true,
            "permissions": null,
            "mcps": {
                "filesystem": {
                    "name": "filesystem",
                    "transport": {
                        "type": "stdio",
                        "command": ["npx", "server-filesystem"],
                        "env": {}
                    },
                    "startup_timeout_secs": 10,
                    "tool_timeout_secs": 60
                }
            }
        }))
        .expect("old-format run agent settings should deserialize");

        let entry = settings
            .mcps
            .get("filesystem")
            .expect("filesystem entry should be present");
        match entry {
            ResolvedMcpEntry::Resolved(server) => {
                assert_eq!(server.name, "filesystem");
                assert!(matches!(server.transport, McpTransport::Stdio { .. }));
            }
            ResolvedMcpEntry::Reference(_) => {
                panic!("bare McpServerSettings must deserialize as Resolved, not Reference")
            }
        }
    }

    /// The same guarantee via TOML, since run specs are also persisted/read as
    /// TOML config.
    #[test]
    fn deserializes_old_format_bare_mcps_as_resolved_toml() {
        let toml = r#"
fabro_tools = true

[mcps.http_server]
name = "http_server"
startup_timeout_secs = 10
tool_timeout_secs = 60

[mcps.http_server.transport]
type = "http"
url = "https://example.com/mcp"

[mcps.http_server.transport.headers]
"#;
        let settings: RunAgentSettings =
            toml::from_str(toml).expect("old-format TOML run agent settings should deserialize");

        match settings.mcps.get("http_server") {
            Some(ResolvedMcpEntry::Resolved(server)) => {
                assert_eq!(server.name, "http_server");
                assert!(matches!(server.transport, McpTransport::Http { .. }));
            }
            other => panic!("expected Resolved http_server entry, got {other:?}"),
        }
    }

    /// A `{ id, enabled }`-shaped value deserializes as `Reference` (it has no
    /// `name`/`transport`, and `deny_unknown_fields` keeps it from matching a
    /// server config), while a full server config deserializes as `Resolved`.
    #[test]
    fn distinguishes_reference_from_resolved() {
        let settings: RunAgentSettings = serde_json::from_value(serde_json::json!({
            "mcps": {
                "sentry": { "id": "sentry", "enabled": true },
                "linear": { "id": "linear" },
                "inline": {
                    "name": "inline",
                    "transport": {
                        "type": "stdio",
                        "command": ["my-server"],
                        "env": {}
                    },
                    "startup_timeout_secs": 5,
                    "tool_timeout_secs": 30
                }
            }
        }))
        .expect("mixed reference/resolved settings should deserialize");

        assert_eq!(
            settings.mcps.get("sentry"),
            Some(&ResolvedMcpEntry::Reference(McpServerRef {
                id:      "sentry".to_string(),
                enabled: Some(true),
            }))
        );
        assert_eq!(
            settings.mcps.get("linear"),
            Some(&ResolvedMcpEntry::Reference(McpServerRef {
                id:      "linear".to_string(),
                enabled: None,
            }))
        );
        match settings.mcps.get("inline") {
            Some(ResolvedMcpEntry::Resolved(server)) => assert_eq!(server.name, "inline"),
            other => panic!("expected Resolved inline entry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_entry_mixing_reference_and_inline_server_fields() {
        let err = serde_json::from_value::<RunAgentSettings>(serde_json::json!({
            "mcps": {
                "mixed": {
                    "id": "catalog-server",
                    "name": "inline",
                    "transport": {
                        "type": "stdio",
                        "command": ["my-server"],
                        "env": {}
                    },
                    "startup_timeout_secs": 5,
                    "tool_timeout_secs": 30
                }
            }
        }))
        .expect_err("mixed reference/server entry should be rejected");

        assert!(
            err.to_string()
                .contains("cannot mix catalog reference fields"),
            "unexpected error: {err}"
        );
    }

    /// `Resolved` serializes back out as a bare `McpServerSettings` (no enum
    /// tag) so newly written run specs stay readable by any old reader and the
    /// round-trip is stable.
    #[test]
    fn resolved_serializes_as_bare_server_settings() {
        let server = McpServerSettings {
            name: "demo".to_string(),
            transport: McpTransport::Stdio {
                command: vec!["demo-server".to_string()],
                env:     std::collections::HashMap::new(),
            },
            ..McpServerSettings::default()
        };
        let entry = ResolvedMcpEntry::Resolved(server.clone());

        let value = serde_json::to_value(&entry).expect("entry should serialize");
        // No "Resolved" tag — it serializes as the bare server settings.
        assert_eq!(
            value,
            serde_json::to_value(&server).expect("server serializes")
        );

        let round_tripped: ResolvedMcpEntry =
            serde_json::from_value(value).expect("entry should round-trip");
        assert_eq!(round_tripped, entry);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerSettings {
    pub name:                 String,
    pub transport:            McpTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_dir:          Option<PathBuf>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_env:            bool,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs:    u64,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            name:                 String::new(),
            transport:            McpTransport::Stdio {
                command: Vec::new(),
                env:     HashMap::new(),
            },
            current_dir:          None,
            clear_env:            false,
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        }
    }
}

impl McpServerSettings {
    #[must_use]
    pub fn startup_timeout(&self) -> StdDuration {
        StdDuration::from_secs(self.startup_timeout_secs)
    }

    #[must_use]
    pub fn tool_timeout(&self) -> StdDuration {
        StdDuration::from_secs(self.tool_timeout_secs)
    }

    /// Resolve `{{ env.* }}` tokens in this server's transport strings
    /// (`command`/`args`/`url`/`env`/`headers`) against `env_lookup`,
    /// returning a copy with the tokens replaced and every other field
    /// preserved.
    ///
    /// This is the late, use-time half of MCP interpolation, the counterpart
    /// to [`substitute_mcp_transport`]: `{{ vars.* }}` are substituted
    /// earlier, server-side, while `{{ env.* }}` resolve here — in whichever
    /// process actually launches the server (the run worker for `fabro run`,
    /// the CLI process for `fabro exec`). Carrying the source form out of the
    /// config resolve layer keeps `fabro validate` portable (it never requires
    /// env to be set).
    ///
    /// A referenced env var that is unset is a hard error — no fallback to the
    /// unresolved source. Reserved `secrets`/`inputs` tokens have no lookup
    /// here and surface as a loud [`ResolveErrorKind::Unavailable`] error
    /// rather than passing through as literal text.
    pub fn resolve_transport_env(
        &self,
        mut env_lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ResolveError> {
        let mut resolved = self.clone();
        visit_mcp_transport_strings(&mut resolved.transport, &mut |value| {
            resolve_env_string(value, &mut env_lookup)
        })?;
        Ok(resolved)
    }
}

/// Resolve `{{ env.* }}` tokens in one MCP transport string. A literal value
/// (no tokens) round-trips unchanged.
fn resolve_env_string(
    value: &mut String,
    env_lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<(), ResolveError> {
    if !value.contains("{{") {
        return Ok(());
    }
    *value = InterpString::parse(value).resolve(&mut *env_lookup)?.value;
    Ok(())
}

#[cfg(test)]
mod resolve_transport_env_tests {
    use std::collections::HashMap;

    use super::super::interp::ResolveErrorKind;
    use super::{McpHttpProtocol, McpServerSettings, McpTransport, Namespace};

    fn env_lookup(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<String> + Copy {
        move |name| {
            pairs
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
        }
    }

    #[test]
    fn literal_transport_passes_through() {
        let settings = McpServerSettings {
            name: "baseline".to_string(),
            transport: McpTransport::Stdio {
                command: vec!["python".to_string(), "server.py".to_string()],
                env:     HashMap::from([("TOKEN".to_string(), "literal-value".to_string())]),
            },
            ..McpServerSettings::default()
        };

        let resolved = settings.resolve_transport_env(env_lookup(&[])).unwrap();

        let McpTransport::Stdio { command, env } = resolved.transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(command, vec!["python".to_string(), "server.py".to_string()]);
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("literal-value"));
    }

    #[test]
    fn stdio_command_and_env_resolve() {
        let settings = McpServerSettings {
            name: "gemini".to_string(),
            transport: McpTransport::Stdio {
                command: vec!["python".to_string(), "{{ env.SERVER_PATH }}".to_string()],
                env:     HashMap::from([(
                    "GEMINI_API_KEY".to_string(),
                    "{{ env.GEMINI_API_KEY }}".to_string(),
                )]),
            },
            ..McpServerSettings::default()
        };

        let resolved = settings
            .resolve_transport_env(env_lookup(&[
                ("SERVER_PATH", "/srv/mcp.py"),
                ("GEMINI_API_KEY", "real-key"),
            ]))
            .unwrap();

        let McpTransport::Stdio { command, env } = resolved.transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(command, vec![
            "python".to_string(),
            "/srv/mcp.py".to_string()
        ]);
        assert_eq!(
            env.get("GEMINI_API_KEY").map(String::as_str),
            Some("real-key")
        );
    }

    #[test]
    fn http_url_and_headers_resolve() {
        let settings = McpServerSettings {
            name: "remote".to_string(),
            transport: McpTransport::Http {
                protocol: McpHttpProtocol::default(),
                url:      "https://{{ env.MCP_HOST }}/mcp".to_string(),
                headers:  HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer {{ env.MCP_TOKEN }}".to_string(),
                )]),
            },
            ..McpServerSettings::default()
        };

        let resolved = settings
            .resolve_transport_env(env_lookup(&[
                ("MCP_HOST", "mcp.example"),
                ("MCP_TOKEN", "abc123"),
            ]))
            .unwrap();

        let McpTransport::Http { url, headers, .. } = resolved.transport else {
            panic!("expected http transport");
        };
        assert_eq!(url, "https://mcp.example/mcp");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer abc123")
        );
    }

    #[test]
    fn missing_env_is_hard_error() {
        let settings = McpServerSettings {
            name: "gemini".to_string(),
            transport: McpTransport::Stdio {
                command: vec!["python".to_string()],
                env:     HashMap::from([(
                    "GEMINI_API_KEY".to_string(),
                    "{{ env.GEMINI_API_KEY }}".to_string(),
                )]),
            },
            ..McpServerSettings::default()
        };

        let err = settings.resolve_transport_env(env_lookup(&[])).unwrap_err();

        assert_eq!(err.namespace, Namespace::Env);
        assert_eq!(err.name, "GEMINI_API_KEY");
        assert_eq!(err.kind, ResolveErrorKind::Missing);
    }

    #[test]
    fn reserved_secret_token_is_unavailable_not_leaked() {
        let settings = McpServerSettings {
            name: "vaulted".to_string(),
            transport: McpTransport::Stdio {
                command: vec!["python".to_string()],
                env:     HashMap::from([(
                    "API_KEY".to_string(),
                    "{{ secrets.API_KEY }}".to_string(),
                )]),
            },
            ..McpServerSettings::default()
        };

        let err = settings.resolve_transport_env(env_lookup(&[])).unwrap_err();

        assert_eq!(err.namespace, Namespace::Secrets);
        assert_eq!(err.kind, ResolveErrorKind::Unavailable);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: Vec<String>,
        env:     HashMap<String, String>,
    },
    Http {
        #[serde(default)]
        protocol: McpHttpProtocol,
        url:      String,
        headers:  HashMap<String, String>,
    },
    Sandbox {
        #[serde(default)]
        protocol: McpHttpProtocol,
        command:  Vec<String>,
        port:     u16,
        env:      HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpHttpProtocol {
    #[default]
    StreamableHttp,
    Sse,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if helpers receive borrowed field values"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    Verify,
    NoVerify,
    Off,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookType {
    Command {
        command: String,
    },
    Http {
        url:              String,
        headers:          Option<HashMap<String, String>>,
        #[serde(default)]
        allowed_env_vars: Vec<String>,
        #[serde(default)]
        tls:              TlsMode,
    },
    Prompt {
        prompt: String,
        model:  Option<String>,
    },
    Agent {
        prompt:          String,
        model:           Option<String>,
        max_tool_rounds: Option<u32>,
    },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct HookDefinition {
    pub name:       Option<String>,
    pub event:      HookEvent,
    #[serde(default)]
    pub command:    Option<String>,
    #[serde(flatten)]
    pub hook_type:  Option<HookType>,
    pub matcher:    Option<String>,
    pub blocking:   Option<bool>,
    pub timeout_ms: Option<u64>,
    pub sandbox:    Option<bool>,
}

impl HookDefinition {
    pub fn resolved_hook_type(&self) -> Option<std::borrow::Cow<'_, HookType>> {
        if let Some(ref hook_type) = self.hook_type {
            return Some(std::borrow::Cow::Borrowed(hook_type));
        }
        self.command.as_ref().map(|command| {
            std::borrow::Cow::Owned(HookType::Command {
                command: command.clone(),
            })
        })
    }

    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.blocking.unwrap_or({
            matches!(
                self.event,
                HookEvent::RunStart
                    | HookEvent::StageStart
                    | HookEvent::EdgeSelected
                    | HookEvent::PreToolUse
                    | HookEvent::SandboxReady
            )
        })
    }

    #[must_use]
    pub fn timeout(&self) -> StdDuration {
        if let Some(ms) = self.timeout_ms {
            return StdDuration::from_millis(ms);
        }
        let default_ms = match self.resolved_hook_type().as_deref() {
            Some(HookType::Prompt { .. }) => 30_000,
            _ => 60_000,
        };
        StdDuration::from_millis(default_ms)
    }

    #[must_use]
    pub fn runs_in_sandbox(&self) -> bool {
        self.sandbox.unwrap_or(true)
    }

    #[must_use]
    pub fn effective_name(&self) -> String {
        if let Some(ref name) = self.name {
            return name.clone();
        }
        let event = format!("{:?}", self.event).to_lowercase();
        match self.resolved_hook_type().as_deref() {
            Some(HookType::Command { command }) => {
                let short = &command[..command.floor_char_boundary(20)];
                format!("{event}:{short}")
            }
            Some(HookType::Http { url, .. }) => format!("{event}:{url}"),
            Some(HookType::Prompt { prompt, .. } | HookType::Agent { prompt, .. }) => {
                let short = &prompt[..prompt.floor_char_boundary(20)];
                format!("{event}:{short}")
            }
            None => event,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunScmSettings {
    pub provider:   Option<String>,
    pub owner:      Option<String>,
    pub repository: Option<String>,
    pub github:     Option<ScmGitHubSettings>,
}

#[expect(
    clippy::empty_structs_with_brackets,
    reason = "resolved empty table must stay object-shaped on the wire"
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScmGitHubSettings {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestSettings {
    pub enabled:        bool,
    pub draft:          bool,
    pub auto_merge:     bool,
    pub merge_strategy: MergeStrategy,
}

impl Default for PullRequestSettings {
    fn default() -> Self {
        Self {
            enabled:        false,
            draft:          true,
            auto_merge:     false,
            merge_strategy: MergeStrategy::Squash,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtifactsSettings {
    pub include: Vec<String>,
}
/// Outcome of resolving a [`RunGoal`] to its final goal text.
///
/// Carries provenance alongside the text so downstream consumers (e.g. the
/// run manifest builder) can distinguish inline goals from file-sourced goals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRunGoal {
    pub text:   String,
    pub source: ResolvedGoalSource,
}

/// Provenance of a [`ResolvedRunGoal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedGoalSource {
    /// Goal text came from a literal `run.goal = "..."` value.
    Inline,
    /// Goal text was read from a file on disk. The absolute path of that
    /// file is carried for provenance / error reporting.
    File { path: std::path::PathBuf },
}

/// A single prepare step. Exactly one of `script` or `command` must be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script:  Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<InterpString>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env:     HashMap<String, InterpString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Normal,
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Prompt,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPermissions {
    ReadOnly,
    ReadWrite,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    RunStart,
    RunComplete,
    RunFailed,
    StageStart,
    StageComplete,
    StageFailed,
    StageRetrying,
    EdgeSelected,
    ParallelStart,
    ParallelComplete,
    SandboxReady,
    SandboxCleanup,
    CheckpointSaved,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}
