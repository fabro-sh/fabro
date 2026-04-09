//! Convenience accessors on [`SettingsFile`].
//!
//! These methods provide ergonomic, flat-shaped views into the v2 parse
//! tree. They exist so that consumers don't have to chain `.as_ref()`
//! through every Option layer when reading common fields. Each accessor
//! walks the real v2 structure — there is no transitional state here.

use std::collections::HashMap;
use std::path::PathBuf;

use super::cli::{CliExecLayer, CliLayer, CliOutputLayer};
use super::interp::InterpString;
use super::project::ProjectLayer;
use super::run::{
    ApprovalMode, GitAuthorLayer, HookEntry, McpEntryLayer, RunAgentLayer, RunArtifactsLayer,
    RunCheckpointLayer, RunExecutionLayer, RunLayer, RunMode, RunModelLayer, RunPrepareLayer,
    RunPullRequestLayer, RunSandboxLayer,
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

    #[must_use]
    pub fn run_goal(&self) -> Option<&InterpString> {
        self.run.as_ref().and_then(|r| r.goal.as_ref())
    }

    #[must_use]
    pub fn run_goal_str(&self) -> Option<String> {
        self.run_goal().map(InterpString::as_source)
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

    // ---------- cli-scope ----------

    #[must_use]
    pub fn cli_layer(&self) -> Option<&CliLayer> {
        self.cli.as_ref()
    }

    #[must_use]
    pub fn cli_exec(&self) -> Option<&CliExecLayer> {
        self.cli.as_ref().and_then(|c| c.exec.as_ref())
    }

    #[must_use]
    pub fn cli_output(&self) -> Option<&CliOutputLayer> {
        self.cli.as_ref().and_then(|c| c.output.as_ref())
    }

    #[must_use]
    pub fn verbose_enabled(&self) -> bool {
        use super::cli::OutputVerbosity;
        matches!(
            self.cli_output().and_then(|o| o.verbosity),
            Some(OutputVerbosity::Verbose)
        )
    }

    #[must_use]
    pub fn prevent_idle_sleep_enabled(&self) -> bool {
        self.cli_exec()
            .and_then(|e| e.prevent_idle_sleep)
            .unwrap_or(false)
    }

    /// Upgrade check defaults to `true` when unset.
    #[must_use]
    pub fn upgrade_check_enabled(&self) -> bool {
        self.cli
            .as_ref()
            .and_then(|c| c.updates.as_ref())
            .and_then(|u| u.check)
            .unwrap_or(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::v2::run::{RunLayer, RunModelLayer};

    #[test]
    fn run_goal_str_returns_source_value() {
        let file = SettingsFile {
            run: Some(RunLayer {
                goal: Some(InterpString::parse("Implement OAuth")),
                ..RunLayer::default()
            }),
            ..SettingsFile::default()
        };
        assert_eq!(file.run_goal_str().as_deref(), Some("Implement OAuth"));
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
    fn upgrade_check_defaults_to_true_when_unset() {
        let file = SettingsFile::default();
        assert!(file.upgrade_check_enabled());
    }

    #[test]
    fn all_labels_merges_project_workflow_run() {
        use crate::settings::v2::project::ProjectLayer;
        use crate::settings::v2::workflow::WorkflowLayer;

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
