use std::path::Path;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};
use fabro_types::PullRequestLink;

use super::{EngineServices, Handler};
use crate::context::{Context, WorkflowContext, keys};
use crate::error::Error;
use crate::event::{Event, StageScope};
use crate::outcome::Outcome;
use crate::pull_request::{
    AutoMergeOptions, CommittedPullRequestSnapshotError, OpenPullRequestRequest,
    PullRequestDisposition, maybe_open_pull_request, prepare_committed_pull_request_snapshot,
};

pub struct PullRequestHandler;

fn outcome_for_pull_request(
    link: &PullRequestLink,
    base_branch: &str,
    head_branch: &str,
) -> Outcome {
    let mut outcome = Outcome::success();
    outcome.context_updates.insert(
        keys::PULL_REQUEST_URL.to_string(),
        serde_json::json!(link.html_url()),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_NUMBER.to_string(),
        serde_json::json!(link.number),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_OWNER.to_string(),
        serde_json::json!(link.owner),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_REPO.to_string(),
        serde_json::json!(link.repo),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_BASE_BRANCH.to_string(),
        serde_json::json!(base_branch),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_HEAD_BRANCH.to_string(),
        serde_json::json!(head_branch),
    );
    outcome.context_updates.insert(
        keys::PULL_REQUEST_DRAFT.to_string(),
        serde_json::json!(true),
    );
    outcome
}

fn required<'a>(value: Option<&'a str>, message: &str) -> Result<&'a str, Error> {
    value.ok_or_else(|| Error::Precondition(message.to_string()))
}

fn model_for_node<'a>(node: &'a Node, run_model: &'a str) -> &'a str {
    node.model().unwrap_or(run_model)
}

fn validate_github_origin(origin_url: &str) -> Result<(), Error> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    fabro_github::parse_github_owner_repo(&https_url)
        .map(|_| ())
        .map_err(|_| {
            Error::Precondition(
                "pull_request nodes require a valid github.com repo origin".to_string(),
            )
        })
}

#[async_trait]
impl Handler for PullRequestHandler {
    async fn simulate(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        Ok(outcome_for_pull_request(
            &PullRequestLink {
                owner:  "fabro".to_string(),
                repo:   "dry-run".to_string(),
                number: 1,
            },
            "main",
            "fabro/run/dry-run",
        ))
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        _run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let scope = StageScope::for_handler(context, &node.id);
        let result = async {
            if context.parallel_branch_id().is_some() {
                return Err(Error::Precondition(
                    "pull_request nodes cannot execute inside a parallel branch".to_string(),
                ));
            }

            let run_state = services
                .run
                .run_store
                .state()
                .await
                .map_err(|err| Error::handler_with_source("Failed to load run state", err))?;
            if let Some(link) = run_state.pull_request.as_ref() {
                let runtime = services.run.pull_request.as_ref();
                let base_branch = runtime
                    .and_then(|runtime| runtime.base_branch.as_deref())
                    .unwrap_or("main");
                let head_branch = runtime
                    .and_then(|runtime| runtime.head_branch.as_deref())
                    .unwrap_or("");
                return Ok(outcome_for_pull_request(link, base_branch, head_branch));
            }

            let runtime = services.run.pull_request.as_ref().ok_or_else(|| {
                Error::Precondition("pull request runtime is unavailable".to_string())
            })?;
            if !runtime.push_enabled {
                return Err(Error::Precondition(
                    "pull_request nodes require run.run_branch.push = true".to_string(),
                ));
            }
            let credentials = runtime.github.as_ref().ok_or_else(|| {
                Error::Precondition(
                    "GitHub credentials are required for pull_request nodes".to_string(),
                )
            })?;
            let origin_url = required(
                runtime.origin_url.as_deref(),
                "pull_request nodes require a GitHub repo origin",
            )?;
            let base_branch = required(
                runtime.base_branch.as_deref(),
                "pull_request nodes require a base branch",
            )?;
            let head_branch = required(
                runtime.head_branch.as_deref(),
                "pull_request nodes require an enabled run branch",
            )?;
            let base_sha = required(
                runtime.base_sha.as_deref(),
                "pull_request nodes require a committed base SHA",
            )?;
            validate_github_origin(origin_url)?;

            let snapshot = prepare_committed_pull_request_snapshot(
                services.run.sandbox.as_ref(),
                base_sha,
                head_branch,
            )
            .await
            .map_err(|err| match err {
                CommittedPullRequestSnapshotError::InvalidBase => Error::Precondition(
                    "pull_request nodes require a valid committed base SHA".to_string(),
                ),
                err => Error::handler_with_source("Failed to prepare pull request snapshot", err),
            })?;
            if snapshot.diff.trim().is_empty() {
                return Err(Error::Precondition(
                    "pull_request node found no committed changes to open".to_string(),
                ));
            }

            let model = model_for_node(node, &services.run.model);
            let opened = maybe_open_pull_request(OpenPullRequestRequest {
                github: fabro_github::GitHubContext::new(credentials, &runtime.github_base_url),
                origin_url,
                base_branch,
                head_branch,
                goal: graph.goal(),
                diff: &snapshot.diff,
                model,
                draft: true,
                auto_merge: None::<AutoMergeOptions>,
                run_store: &services.run.run_store,
                llm_source: services.run.llm_source.as_ref(),
                catalog: services.run.catalog.clone(),
                conclusion: run_state.conclusion.as_ref(),
                run_state: Some(&run_state),
            })
            .await
            .map_err(|err| Error::handler_with_anyhow("Pull request creation failed", err))?
            .ok_or_else(|| {
                Error::Precondition(
                    "pull_request node found no committed changes to open".to_string(),
                )
            })?;

            match opened.disposition {
                PullRequestDisposition::Created => {
                    services.run.emitter.emit_scoped(
                        &Event::pull_request_created(
                            &opened.link,
                            &opened.base_branch,
                            &opened.head_branch,
                            &opened.title,
                            true,
                        ),
                        &scope,
                    );
                }
                PullRequestDisposition::Linked => {
                    services.run.emitter.emit_scoped(
                        &Event::PullRequestLinked {
                            pull_request: opened.link.clone(),
                        },
                        &scope,
                    );
                }
            }

            Ok(outcome_for_pull_request(
                &opened.link,
                &opened.base_branch,
                &opened.head_branch,
            ))
        }
        .await;

        if let Err(error) = &result {
            services.run.emitter.emit_scoped(
                &Event::PullRequestFailed {
                    error: error.to_string(),
                },
                &scope,
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use fabro_graphviz::graph::{AttrValue, Graph, Node};
    use fabro_store::Database;
    use fabro_types::{WorkflowSettings, fixtures, test_support};
    use httpmock::MockServer;
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::{Emitter, append_event};
    use crate::runtime_store::RunStoreHandle;
    use crate::services::PullRequestRuntime;

    #[expect(
        clippy::disallowed_methods,
        reason = "Temporary git fixture setup is intentionally synchronous."
    )]
    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git should execute");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_string()
    }

    async fn test_run_store(graph: &Graph) -> RunStoreHandle {
        let store = Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ));
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            title:            None,
            settings:         serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:            serde_json::to_value(graph).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           std::collections::BTreeMap::new(),
            run_dir:          "/tmp/test".to_string(),
            source_directory: None,
            workflow_slug:    Some("test".to_string()),
            automation:       None,
            db_prefix:        None,
            provenance:       test_support::test_run_provenance(),
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            retried_from:     None,
            parent_id:        None,
            web_url:          None,
        })
        .await
        .unwrap();
        run_store.into()
    }

    #[tokio::test]
    async fn dry_run_returns_deterministic_pull_request_context() {
        let outcome = PullRequestHandler
            .simulate(
                &Node::new("create_pr"),
                &Context::new(),
                &Graph::new("test"),
                Path::new("."),
                &EngineServices::test_default(),
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_URL],
            serde_json::json!("https://github.com/fabro/dry-run/pull/1")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_DRAFT],
            serde_json::json!(true)
        );
    }

    #[tokio::test]
    async fn pull_request_node_rejects_parallel_branch_context() {
        let context = Context::new();
        context.set(
            keys::INTERNAL_PARALLEL_BRANCH_ID,
            serde_json::json!("fanout@1:0"),
        );

        let error = PullRequestHandler
            .execute(
                &Node::new("create_pr"),
                &context,
                &Graph::new("test"),
                Path::new("."),
                &EngineServices::test_default(),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Precondition(_)));
        assert!(error.to_string().contains("parallel branch"));
    }

    #[test]
    fn node_model_overrides_run_model() {
        let mut node = Node::new("create_pr");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("node-model".to_string()),
        );

        assert_eq!(model_for_node(&node, "run-model"), "node-model");
        assert_eq!(
            model_for_node(&Node::new("create_pr"), "run-model"),
            "run-model"
        );
    }

    #[test]
    fn github_origin_validation_accepts_https_and_scp_syntax() {
        assert!(validate_github_origin("https://github.com/acme/widgets.git").is_ok());
        assert!(validate_github_origin("git@github.com:acme/widgets.git").is_ok());
        assert!(validate_github_origin("https://gitlab.com/acme/widgets.git").is_err());
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "Temporary git fixture setup is intentionally synchronous."
    )]
    async fn creates_context_and_links_matching_pull_request_from_committed_snapshot() {
        let repo_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        git(repo_dir.path(), &["init", "-b", "main"]);
        git(repo_dir.path(), &[
            "config",
            "user.email",
            "fabro@example.test",
        ]);
        git(repo_dir.path(), &["config", "user.name", "Fabro Test"]);
        std::fs::write(repo_dir.path().join("tracked.txt"), "base\n").unwrap();
        git(repo_dir.path(), &["add", "tracked.txt"]);
        git(repo_dir.path(), &["commit", "-m", "base"]);
        let base_sha = git(repo_dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(repo_dir.path().join("tracked.txt"), "base\ncommitted\n").unwrap();
        git(repo_dir.path(), &["add", "tracked.txt"]);
        git(repo_dir.path(), &["commit", "-m", "committed"]);
        let head_sha = git(repo_dir.path(), &["rev-parse", "HEAD"]);
        git(remote_dir.path(), &["init", "--bare"]);
        git(repo_dir.path(), &[
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ]);

        let github = MockServer::start();
        let find_mock = github.mock(|when, then| {
            when.method("GET")
                .path("/repos/acme/widgets/pulls")
                .query_param("state", "open")
                .query_param("head", "acme:fabro/run/test")
                .query_param("base", "main")
                .query_param("per_page", "2")
                .header("authorization", "Bearer ghu_test");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    serde_json::json!([{
                        "html_url": "https://github.com/acme/widgets/pull/42",
                        "number": 42
                    }])
                    .to_string(),
                );
        });

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Open a pull request".to_string()),
        );
        let run_store = test_run_store(&graph).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_for_listener = Arc::clone(&received);
        emitter.on_event(move |event| {
            received_for_listener.lock().unwrap().push(event.clone());
        });
        let mut services = EngineServices::test_default();
        services.run = services
            .run
            .with_run_store(run_store)
            .with_sandbox(Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )))
            .with_emitter(emitter)
            .with_pull_request(PullRequestRuntime {
                github:          Some(fabro_github::GitHubCredentials::Pat("ghu_test".to_string())),
                github_base_url: github.base_url(),
                origin_url:      Some("https://github.com/acme/widgets.git".to_string()),
                base_branch:     Some("main".to_string()),
                head_branch:     Some("fabro/run/test".to_string()),
                base_sha:        Some(base_sha),
                push_enabled:    true,
            });

        let outcome = PullRequestHandler
            .execute(
                &Node::new("create_pr"),
                &Context::new(),
                &graph,
                repo_dir.path(),
                &services,
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_URL],
            serde_json::json!("https://github.com/acme/widgets/pull/42")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_NUMBER],
            serde_json::json!(42)
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_OWNER],
            serde_json::json!("acme")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_REPO],
            serde_json::json!("widgets")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_BASE_BRANCH],
            serde_json::json!("main")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_HEAD_BRANCH],
            serde_json::json!("fabro/run/test")
        );
        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_DRAFT],
            serde_json::json!(true)
        );
        assert_eq!(
            git(remote_dir.path(), &[
                "rev-parse",
                "refs/heads/fabro/run/test"
            ]),
            head_sha
        );
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_name(), "pull_request.linked");
        find_mock.assert();
    }

    #[tokio::test]
    async fn succeeds_from_stored_pull_request_without_runtime_capability() {
        let graph = Graph::new("test");
        let run_store = test_run_store(&graph).await;
        let link = PullRequestLink {
            owner:  "acme".to_string(),
            repo:   "widgets".to_string(),
            number: 42,
        };
        run_store
            .append_run_event(&crate::event::to_run_event(
                &fixtures::RUN_1,
                &Event::PullRequestLinked {
                    pull_request: link.clone(),
                },
            ))
            .await
            .unwrap();
        let mut services = EngineServices::test_default();
        services.run = services.run.with_run_store(run_store);

        let outcome = PullRequestHandler
            .execute(
                &Node::new("create_pr"),
                &Context::new(),
                &graph,
                Path::new("."),
                &services,
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates[keys::PULL_REQUEST_URL],
            serde_json::json!(link.html_url())
        );
    }
}
