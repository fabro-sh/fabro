//! Convenience accessors on [`SettingsFile`].
//!
//! These methods provide ergonomic, flat-shaped views into the v2 parse
//! tree. They exist so that consumers don't have to chain `.as_ref()`
//! through every Option layer when reading common fields. Each accessor
//! walks the real v2 structure — there is no transitional state here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::interp::InterpString;
use super::project::ProjectLayer;
use super::run::{
    ApprovalMode, GitAuthorLayer, HookEntry, McpEntryLayer, ResolvedGoalSource, ResolvedRunGoal,
    RunAgentLayer, RunArtifactsLayer, RunCheckpointLayer, RunExecutionLayer, RunGoalLayer,
    RunLayer, RunMode, RunModelLayer, RunPrepareLayer, RunPullRequestLayer, RunSandboxLayer,
};
use super::server::{
    GithubIntegrationLayer, ServerApiLayer, ServerArtifactsLayer, ServerIntegrationsLayer,
    ServerLayer, ServerLoggingLayer, ServerSchedulerLayer, ServerStorageLayer, ServerWebLayer,
    SlackIntegrationLayer,
};
use super::tree::SettingsFile;

impl SettingsFile {
    // ---------- project-scope ----------

    #[must_use]
    pub fn project_layer(&self) -> Option<&ProjectLayer> {
        self.project.as_ref()
    }

    #[must_use]
    pub fn project_directory(&self) -> Option<&str> {
        self.project.as_ref().and_then(|p| p.directory.as_deref())
    }

    // ---------- run-scope ----------

    #[must_use]
    pub fn run_layer(&self) -> Option<&RunLayer> {
        self.run.as_ref()
    }

    /// Raw access to the `run.goal` variant (inline or file).
    #[must_use]
    pub fn run_goal_layer(&self) -> Option<&RunGoalLayer> {
        self.run.as_ref().and_then(|r| r.goal.as_ref())
    }

    /// Inline goal text only. Returns `None` when `run.goal` is unset **or**
    /// when it's a file-sourced goal — callers that need the file contents
    /// should use [`SettingsFile::resolve_run_goal`].
    #[must_use]
    pub fn run_goal_inline_str(&self) -> Option<String> {
        match self.run_goal_layer()? {
            RunGoalLayer::Inline(s) => Some(s.as_source()),
            RunGoalLayer::File { .. } => None,
        }
    }

    /// Resolve the `run.goal` layer to its final text, reading a file from
    /// disk if necessary.
    ///
    /// Path resolution:
    ///
    /// - Absolute paths in the `file` variant are used as-is.
    /// - Literal relative paths should already have been rewritten to
    ///   absolute at config-load time by
    ///   `fabro_config::resolve_goal_file_paths`. If one reaches this point
    ///   it will be resolved against `base_dir` as a fallback.
    /// - `${env.NAME}` interpolation is resolved via `std::env::var` at
    ///   call time. Relative paths that survive interpolation are also
    ///   resolved against `base_dir`.
    ///
    /// Returns `Ok(None)` when `run.goal` is unset. Returns `Err` when the
    /// file variant points at a path that can't be read or has an
    /// unresolved env token.
    pub fn resolve_run_goal(
        &self,
        base_dir: &Path,
    ) -> Result<Option<ResolvedRunGoal>, ResolveGoalError> {
        let Some(layer) = self.run_goal_layer() else {
            return Ok(None);
        };
        match layer {
            RunGoalLayer::Inline(s) => Ok(Some(ResolvedRunGoal {
                text: s.as_source(),
                source: ResolvedGoalSource::Inline,
            })),
            RunGoalLayer::File { file } => {
                let resolved = file
                    .resolve(|name| std::env::var(name).ok())
                    .map_err(|err| ResolveGoalError::EnvLookup { var: err.name })?;
                let path = resolve_goal_file_path(&resolved.value, base_dir);
                let text = std::fs::read_to_string(&path).map_err(|err| ResolveGoalError::Io {
                    path: path.clone(),
                    source: err,
                })?;
                Ok(Some(ResolvedRunGoal {
                    text,
                    source: ResolvedGoalSource::File { path },
                }))
            }
        }
    }

    #[must_use]
    pub fn run_working_dir(&self) -> Option<&InterpString> {
        self.run.as_ref().and_then(|r| r.working_dir.as_ref())
    }

    #[must_use]
    pub fn run_working_dir_str(&self) -> Option<String> {
        self.run_working_dir().map(InterpString::as_source)
    }

    #[must_use]
    pub fn run_model(&self) -> Option<&RunModelLayer> {
        self.run.as_ref().and_then(|r| r.model.as_ref())
    }

    #[must_use]
    pub fn run_model_name_str(&self) -> Option<String> {
        self.run_model()
            .and_then(|m| m.name.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn run_model_provider_str(&self) -> Option<String> {
        self.run_model()
            .and_then(|m| m.provider.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn run_sandbox(&self) -> Option<&RunSandboxLayer> {
        self.run.as_ref().and_then(|r| r.sandbox.as_ref())
    }

    #[must_use]
    pub fn run_prepare(&self) -> Option<&RunPrepareLayer> {
        self.run.as_ref().and_then(|r| r.prepare.as_ref())
    }

    /// Flattened prepare-step commands: each `script` is kept as-is, and
    /// `command` argv is joined with spaces. Env-interpolation tokens are
    /// emitted verbatim via [`InterpString::as_source`].
    #[must_use]
    pub fn run_prepare_commands(&self) -> Vec<String> {
        let Some(prepare) = self.run_prepare() else {
            return Vec::new();
        };
        prepare
            .steps
            .iter()
            .filter_map(|step| {
                if let Some(script) = &step.script {
                    Some(script.as_source())
                } else {
                    step.command.as_ref().map(|argv| {
                        argv.iter()
                            .map(InterpString::as_source)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                }
            })
            .collect()
    }

    /// Prepare-step timeout in milliseconds.
    #[must_use]
    pub fn run_prepare_timeout_ms(&self) -> Option<u64> {
        self.run_prepare()
            .and_then(|p| p.timeout)
            .map(|d| u64::try_from(d.as_std().as_millis()).unwrap_or(u64::MAX))
    }

    #[must_use]
    pub fn run_checkpoint(&self) -> Option<&RunCheckpointLayer> {
        self.run.as_ref().and_then(|r| r.checkpoint.as_ref())
    }

    #[must_use]
    pub fn run_hooks(&self) -> &[HookEntry] {
        self.run.as_ref().map_or(&[], |r| r.hooks.as_slice())
    }

    #[must_use]
    pub fn run_pull_request(&self) -> Option<&RunPullRequestLayer> {
        self.run.as_ref().and_then(|r| r.pull_request.as_ref())
    }

    #[must_use]
    pub fn run_artifacts(&self) -> Option<&RunArtifactsLayer> {
        self.run.as_ref().and_then(|r| r.artifacts.as_ref())
    }

    #[must_use]
    pub fn run_execution(&self) -> Option<&RunExecutionLayer> {
        self.run.as_ref().and_then(|r| r.execution.as_ref())
    }

    #[must_use]
    pub fn run_agent(&self) -> Option<&RunAgentLayer> {
        self.run.as_ref().and_then(|r| r.agent.as_ref())
    }

    #[must_use]
    pub fn run_agent_mcps(&self) -> Option<&HashMap<String, McpEntryLayer>> {
        self.run_agent().map(|a| &a.mcps)
    }

    #[must_use]
    pub fn run_inputs(&self) -> Option<&HashMap<String, toml::Value>> {
        self.run.as_ref().and_then(|r| r.inputs.as_ref())
    }

    /// Stringified view of `run.inputs`: non-string TOML values are rendered
    /// via their canonical TOML representation (integers, booleans, and
    /// arrays are flattened through `Display`). Returns `None` when no
    /// inputs are set.
    #[must_use]
    pub fn run_inputs_as_strings(&self) -> Option<HashMap<String, String>> {
        self.run_inputs().map(|inputs| {
            inputs
                .iter()
                .map(|(k, v)| {
                    let stringified = match v {
                        toml::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), stringified)
                })
                .collect()
        })
    }

    #[must_use]
    pub fn run_metadata(&self) -> Option<&HashMap<String, String>> {
        self.run.as_ref().map(|r| &r.metadata)
    }

    #[must_use]
    pub fn run_git_author(&self) -> Option<&GitAuthorLayer> {
        self.run
            .as_ref()
            .and_then(|r| r.git.as_ref())
            .and_then(|g| g.author.as_ref())
    }

    // ---------- execution-posture booleans ----------

    #[must_use]
    pub fn dry_run_enabled(&self) -> bool {
        matches!(
            self.run_execution().and_then(|e| e.mode),
            Some(RunMode::DryRun)
        )
    }

    #[must_use]
    pub fn auto_approve_enabled(&self) -> bool {
        matches!(
            self.run_execution().and_then(|e| e.approval),
            Some(ApprovalMode::Auto)
        )
    }

    /// Returns `true` when retros are explicitly disabled. Defaults to
    /// `false` (retros enabled) when not set.
    #[must_use]
    pub fn no_retro_enabled(&self) -> bool {
        matches!(self.run_execution().and_then(|e| e.retros), Some(false))
    }

    #[must_use]
    pub fn preserve_sandbox_enabled(&self) -> bool {
        self.run_sandbox()
            .and_then(|sb| sb.preserve)
            .unwrap_or(false)
    }

    // ---------- server-scope ----------

    #[must_use]
    pub fn server_layer(&self) -> Option<&ServerLayer> {
        self.server.as_ref()
    }

    #[must_use]
    pub fn server_api(&self) -> Option<&ServerApiLayer> {
        self.server.as_ref().and_then(|s| s.api.as_ref())
    }

    #[must_use]
    pub fn server_web(&self) -> Option<&ServerWebLayer> {
        self.server.as_ref().and_then(|s| s.web.as_ref())
    }

    #[must_use]
    pub fn server_storage(&self) -> Option<&ServerStorageLayer> {
        self.server.as_ref().and_then(|s| s.storage.as_ref())
    }

    #[must_use]
    pub fn server_storage_root_str(&self) -> Option<String> {
        self.server_storage()
            .and_then(|s| s.root.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn server_artifacts(&self) -> Option<&ServerArtifactsLayer> {
        self.server.as_ref().and_then(|s| s.artifacts.as_ref())
    }

    #[must_use]
    pub fn server_scheduler(&self) -> Option<&ServerSchedulerLayer> {
        self.server.as_ref().and_then(|s| s.scheduler.as_ref())
    }

    #[must_use]
    pub fn max_concurrent_runs(&self) -> Option<usize> {
        self.server_scheduler().and_then(|s| s.max_concurrent_runs)
    }

    #[must_use]
    pub fn server_logging(&self) -> Option<&ServerLoggingLayer> {
        self.server.as_ref().and_then(|s| s.logging.as_ref())
    }

    #[must_use]
    pub fn server_integrations(&self) -> Option<&ServerIntegrationsLayer> {
        self.server.as_ref().and_then(|s| s.integrations.as_ref())
    }

    #[must_use]
    pub fn server_integrations_github(&self) -> Option<&GithubIntegrationLayer> {
        self.server_integrations().and_then(|i| i.github.as_ref())
    }

    #[must_use]
    pub fn server_integrations_slack(&self) -> Option<&SlackIntegrationLayer> {
        self.server_integrations().and_then(|i| i.slack.as_ref())
    }

    #[must_use]
    pub fn github_app_id_str(&self) -> Option<String> {
        self.server_integrations_github()
            .and_then(|g| g.app_id.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn github_client_id_str(&self) -> Option<String> {
        self.server_integrations_github()
            .and_then(|g| g.client_id.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn github_slug_str(&self) -> Option<String> {
        self.server_integrations_github()
            .and_then(|g| g.slug.as_ref())
            .map(InterpString::as_source)
    }

    #[must_use]
    pub fn github_permissions(&self) -> Option<&HashMap<String, InterpString>> {
        self.server_integrations_github()
            .map(|g| &g.permissions)
            .filter(|m| !m.is_empty())
    }

    // ---------- storage path with home-dir default ----------

    /// Returns the configured server storage root, or the home-dir default
    /// when unset. Env interpolation is resolved at read time against the
    /// process environment.
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.server_storage()
            .and_then(|s| s.root.as_ref())
            .and_then(|interp| {
                interp
                    .resolve(|name| std::env::var(name).ok())
                    .ok()
                    .map(|resolved| resolved.value)
            })
            .map_or_else(|| fabro_util::Home::from_env().storage_dir(), PathBuf::from)
    }

    // ---------- labels / metadata aggregation ----------

    /// Combined metadata labels from project, workflow, and run layers.
    /// Later layers overwrite earlier ones (project < workflow < run).
    #[must_use]
    pub fn all_labels(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        if let Some(project) = &self.project {
            for (k, v) in &project.metadata {
                out.insert(k.clone(), v.clone());
            }
        }
        if let Some(workflow) = &self.workflow {
            for (k, v) in &workflow.metadata {
                out.insert(k.clone(), v.clone());
            }
        }
        if let Some(run) = &self.run {
            for (k, v) in &run.metadata {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}

/// Resolve a goal-file path string against `base_dir`. Absolute paths are
/// used as-is; relative paths are joined onto `base_dir`.
#[must_use]
pub fn resolve_goal_file_path(path_str: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// Error returned by [`SettingsFile::resolve_run_goal`].
#[derive(Debug)]
pub enum ResolveGoalError {
    /// The `run.goal.file` InterpString referenced an env var that wasn't
    /// set at consume time.
    EnvLookup { var: String },
    /// The goal file exists in config but could not be read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ResolveGoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvLookup { var } => write!(
                f,
                "failed to resolve run.goal.file: env var {var:?} referenced by ${{env.{var}}} is not set"
            ),
            Self::Io { path, .. } => {
                write!(f, "failed to read run.goal.file at {}", path.display())
            }
        }
    }
}

impl std::error::Error for ResolveGoalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EnvLookup { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::run::{RunLayer, RunModelLayer};

    #[test]
    fn run_goal_inline_str_returns_source_value() {
        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(RunGoalLayer::Inline(InterpString::parse("Implement OAuth"))),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        assert_eq!(
            file.run_goal_inline_str().as_deref(),
            Some("Implement OAuth")
        );
    }

    #[test]
    fn run_goal_inline_str_is_none_for_file_variant() {
        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(RunGoalLayer::File {
                    file: InterpString::parse("/abs/goal.md"),
                }),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        assert_eq!(file.run_goal_inline_str(), None);
        assert!(matches!(
            file.run_goal_layer(),
            Some(RunGoalLayer::File { .. })
        ));
    }

    #[test]
    fn resolve_run_goal_reads_file_variant_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let goal_path = tmp.path().join("goal.md");
        std::fs::write(&goal_path, "ship the thing").unwrap();

        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(RunGoalLayer::File {
                    file: InterpString::parse(goal_path.to_str().unwrap()),
                }),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };

        let resolved = file
            .resolve_run_goal(tmp.path())
            .expect("goal file should resolve")
            .expect("goal should be set");
        assert_eq!(resolved.text, "ship the thing");
        assert!(matches!(
            resolved.source,
            ResolvedGoalSource::File { ref path } if path == &goal_path
        ));
    }

    #[test]
    fn resolve_run_goal_inline_passes_text_through() {
        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(RunGoalLayer::Inline(InterpString::parse("literal goal"))),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        let resolved = file
            .resolve_run_goal(std::path::Path::new("/"))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.text, "literal goal");
        assert_eq!(resolved.source, ResolvedGoalSource::Inline);
    }

    #[test]
    fn run_model_name_str_walks_tree() {
        let file = SettingsFile {
            run: Some(RunLayer {
                model: Some(RunModelLayer {
                    name: Some(InterpString::parse("claude-sonnet-4-6")),
                    ..RunModelLayer::default()
                }),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        assert_eq!(
            file.run_model_name_str().as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn all_labels_merges_project_workflow_run() {
        use crate::settings::project::ProjectLayer;
        use crate::settings::workflow::WorkflowLayer;

        let mut project_metadata = HashMap::new();
        project_metadata.insert("env".into(), "project".into());
        project_metadata.insert("team".into(), "core".into());

        let mut workflow_metadata = HashMap::new();
        workflow_metadata.insert("env".into(), "workflow".into());

        let mut run_metadata = HashMap::new();
        run_metadata.insert("priority".into(), "high".into());

        let file = SettingsFile {
            project: Some(ProjectLayer {
                metadata: project_metadata,
                ..ProjectLayer::default()
            }),
            workflow: Some(WorkflowLayer {
                metadata: workflow_metadata,
                ..WorkflowLayer::default()
            }),
            run: Some(RunLayer {
                metadata: run_metadata,
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };

        let labels = file.all_labels();
        assert_eq!(labels.get("env").map(String::as_str), Some("workflow"));
        assert_eq!(labels.get("team").map(String::as_str), Some("core"));
        assert_eq!(labels.get("priority").map(String::as_str), Some("high"));
    }
}
