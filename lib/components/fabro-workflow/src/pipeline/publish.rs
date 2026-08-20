use std::sync::Arc;

use super::pull_request::{AutoMergeOptions, OpenPullRequestRequest, open_pull_request};
use super::types::{Concluded, PublishOptions, PublishOutcome, Published};
use crate::error::Error;
use crate::event::Event;
use crate::lifecycle::git::push_run_branch;

/// PUBLISH phase: push the final run commit and, when configured, open a pull
/// request.
///
/// Publish is always present in the pipeline. It becomes a no-op when the run
/// did not succeed, is a dry run, or has no remote branch configured.
pub async fn publish(concluded: Concluded, options: &PublishOptions) -> Published {
    let mut publish_outcome = PublishOutcome::default();
    let publish_error = concluded.publish(options, &mut publish_outcome).await.err();

    let Concluded {
        outcome,
        conclusion,
        artifact_count,
        graph: _,
        run_options,
        services,
    } = concluded;

    Published {
        execution_outcome: outcome,
        publish_outcome,
        publish_error,
        conclusion,
        artifact_count,
        run_options,
        services,
    }
}

impl Concluded {
    /// Run the publish steps, recording each one into `outcome` as it lands.
    ///
    /// `outcome` accumulates what actually happened, so a branch that reached
    /// the remote is still reported when pull request creation later fails.
    async fn publish(
        &self,
        options: &PublishOptions,
        outcome: &mut PublishOutcome,
    ) -> Result<(), Error> {
        // A run that did not succeed, or that never intended to touch the
        // remote, has nothing to publish — even when a pull request was asked
        // for. Only a run that got far enough to publish can fail publishing.
        if !self
            .outcome
            .as_ref()
            .is_ok_and(|o| o.status.is_successful())
            || self.run_options.dry_run_enabled()
        {
            return Ok(());
        }

        let pull_request_requested = options.pr_config.is_some();
        let (origin_url, run_branch) = match self.publish_target(options) {
            Ok(target) => target,
            Err(_) if !pull_request_requested => return Ok(()),
            Err(reason) => return Err(self.pull_request_error(reason)),
        };

        self.push_final_commit(run_branch).await?;
        outcome.pushed_branch = Some(run_branch.to_string());

        let Some(pr_config) = options.pr_config.as_ref() else {
            return Ok(());
        };
        let diff = self.conclusion.diff.patch.as_deref().unwrap_or_default();
        if diff.trim().is_empty() {
            return Ok(());
        }

        // Only pull request creation needs the SHA, to check that the remote
        // branch really carries this run's work. Pushing does not: the refspec
        // sends whatever the branch points at.
        let final_sha = self
            .conclusion
            .final_git_commit_sha
            .as_deref()
            .ok_or_else(|| {
                self.pull_request_error("pull request creation requires the run's final commit SHA")
            })?;

        let base_branch = self.run_options.base_branch.as_deref().ok_or_else(|| {
            self.pull_request_error("pull request creation requires a base branch")
        })?;
        let credentials = options.github_app.as_ref().ok_or_else(|| {
            self.pull_request_error("pull request creation requires GitHub credentials")
        })?;
        let github_base_url = fabro_github::github_api_base_url();

        let created = open_pull_request(OpenPullRequestRequest {
            github: fabro_github::GitHubContext::new(credentials, &github_base_url),
            origin_url,
            base_branch,
            head_branch: run_branch,
            expected_head_sha: final_sha,
            goal: self.graph.goal(),
            diff,
            model: &options.model,
            draft: pr_config.draft,
            auto_merge: pr_config.auto_merge.then_some(AutoMergeOptions {
                merge_strategy: pr_config.merge_strategy,
            }),
            run_store: &self.services.run_store,
            llm_source: self.services.llm_source.as_ref(),
            catalog: Arc::clone(&self.services.catalog),
            conclusion: Some(&self.conclusion),
            run_state: None,
        })
        .await
        .map_err(|error| {
            self.services.emitter.emit(&Event::PullRequestFailed {
                creation_id: None,
                error:       error.clone(),
            });
            Error::publish_with_source("failed to create pull request", anyhow::anyhow!(error))
        })?;

        self.services.emitter.emit(&Event::pull_request_created(
            &created.link,
            &created.base_branch,
            &created.head_branch,
            final_sha,
            &created.title,
            pr_config.draft,
        ));
        outcome.pr_url = Some(created.link.html_url());

        Ok(())
    }

    /// The origin and run branch to publish to.
    ///
    /// `Err` carries why there is no target. That is only a failure when a
    /// pull request was requested; otherwise publish just has nothing to do.
    fn publish_target<'a>(
        &'a self,
        options: &'a PublishOptions,
    ) -> Result<(&'a str, &'a str), &'static str> {
        let origin_url = options
            .origin_url
            .as_deref()
            .filter(|origin| !origin.trim().is_empty())
            .ok_or("pull request creation requires a GitHub origin URL")?;
        let run_branch = self
            .run_options
            .run_branch()
            .ok_or("pull request creation requires a run branch")?;
        if !self.run_options.settings.run.run_branch.push {
            return Err("pull request creation requires run branch pushing");
        }
        Ok((origin_url, run_branch))
    }

    async fn push_final_commit(&self, run_branch: &str) -> Result<(), Error> {
        match push_run_branch(self.services.sandbox.as_ref(), run_branch).await {
            Ok(()) => {
                self.services.emitter.emit(&Event::GitPush {
                    branch:           run_branch.to_string(),
                    success:          true,
                    exec_output_tail: None,
                });
                Ok(())
            }
            Err(error) => {
                let exec_output_tail = fabro_sandbox::default_redacted_output_tail(&error);
                self.services.emitter.emit(&Event::GitPush {
                    branch:           run_branch.to_string(),
                    success:          false,
                    exec_output_tail: exec_output_tail.clone(),
                });
                Err(Error::publish_with_source_and_exec_output_tail(
                    format!("failed to push run branch '{run_branch}'"),
                    error,
                    exec_output_tail,
                ))
            }
        }
    }

    fn pull_request_error(&self, message: &str) -> Error {
        self.services.emitter.emit(&Event::PullRequestFailed {
            creation_id: None,
            error:       message.to_string(),
        });
        Error::publish(message)
    }
}
