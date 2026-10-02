//! Live Docker coverage through the same engine assembly as a run worker.

mod support;

use std::env;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunRequest, RunStatus};
use fabro_petri::providers::{self, DaytonaCredentials, SandboxProviderConfig};
use fabro_petri::prune::{self, PruneRequest};
use fabro_petri::runtime::RuntimeSpec;
use fabro_types::settings::run::{EnvironmentNetworkMode, EnvironmentNetworkSettings};
use fabro_types::{RunId, SandboxProviderKind};
use petri_store::MemoryRunStore;
use sandbox_driver::{NetworkPolicy, SandboxFilter, SandboxState, SnapshotFilter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::time::{self, Instant};
use tokio_util::task::AbortOnDropHandle;

#[fabro_macros::e2e_test()]
async fn docker_block_prevents_canary_access_while_allow_all_preserves_it() {
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let _canary = AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            if stream.read(&mut request).await.unwrap() > 0 {
                seen.fetch_add(1, Ordering::SeqCst);
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nnetwork-canary").await.unwrap();
            }
        }
    }));
    for (mode, network_mode, expected_hits) in [
        (EnvironmentNetworkMode::AllowAll, "bridge", 1),
        (EnvironmentNetworkMode::Block, "none", 0),
    ] {
        let root = tempfile::tempdir().unwrap();
        let run_id = RunId::new().to_string();
        let store = Arc::new(MemoryRunStore::new());
        let curl = format!(
            "curl --noproxy '*' --fail --silent --max-time 2 http://host.docker.internal:{port}/canary"
        );
        let request = probe_request(
            &run_id,
            root.path(),
            &store,
            RuntimeSpec::default(),
            SandboxProviderKind::DOCKER,
            mode,
            &format!("test $({curl}) = network-canary"),
            &curl,
        );
        let outcome = engine::run(request).await;
        // Inspect Docker independently of Fabro's settings and driver status.
        let containers = Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("label=petri.run={run_id}"),
            ])
            .output()
            .await
            .unwrap();
        let ids = String::from_utf8(containers.stdout).unwrap();
        let inspected = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.HostConfig.NetworkMode}}",
                ids.trim(),
            ])
            .output()
            .await
            .unwrap();
        // Cleanup before assertions, including when execution or inspection failed.
        let cleanup = prune::prune(PruneRequest {
            sandbox: SandboxProviderConfig::default(),
            run_id,
            run_dir: root.path().to_path_buf(),
            store,
            provider: SandboxProviderKind::DOCKER,
        })
        .await;
        assert!(cleanup.unwrap().is_clean());
        let outcome = outcome.unwrap();
        assert_eq!(outcome.status, RunStatus::Success, "{mode}: {outcome:?}");
        assert!(inspected.status.success(), "{inspected:?}");
        assert_eq!(
            String::from_utf8(inspected.stdout).unwrap().trim(),
            network_mode
        );
        assert_eq!(hits.swap(0, Ordering::SeqCst), expected_hits, "{mode}");
    }
}

/// Runs only when explicitly requested with live credentials. It reuses the
/// runner snapshot and deletes both task-owned sandboxes through Petri.
#[fabro_macros::e2e_test(live("DAYTONA_API_KEY"), live("FABRO_TEST_DAYTONA_RUNNER_SNAPSHOT"))]
async fn daytona_block_prevents_outbound_https_while_allow_all_preserves_it() {
    let credentials = DaytonaCredentials::from_api_key(
        fabro_test::require_env("DAYTONA_API_KEY").expect("guard checked the credential"),
        provider_env,
    );
    let provider = providers::connect_daytona(&credentials).await.unwrap();
    let snapshots = provider.snapshots().unwrap();
    let before = snapshots.list(&SnapshotFilter::default()).await.unwrap();
    // Live validation requires the standard runner snapshot to exist already.
    // Its exact name is supplied by the operator after checking the pinned
    // Petri runner, so the test does not create or delete shared snapshots.
    let runner = fabro_test::require_env("FABRO_TEST_DAYTONA_RUNNER_SNAPSHOT")
        .expect("set the existing pinned Petri runner snapshot name");
    assert!(
        before
            .iter()
            .any(|s| s.name.as_deref() == Some(runner.as_str()))
    );
    let config = SandboxProviderConfig::from_lookup(Some(credentials), provider_env);
    for (mode, expected) in [
        (EnvironmentNetworkMode::AllowAll, NetworkPolicy::AllowAll),
        (EnvironmentNetworkMode::Block, NetworkPolicy::Block),
    ] {
        let root = tempfile::tempdir().unwrap();
        let run_id = RunId::new().to_string();
        let store = Arc::new(MemoryRunStore::new());
        let curl =
            "curl --noproxy '*' --fail --silent --max-time 5 https://example.com/ -o /dev/null";
        let request = probe_request(
            &run_id,
            root.path(),
            &store,
            RuntimeSpec {
                sandbox: config.clone(),
                ..Default::default()
            },
            SandboxProviderKind::DAYTONA,
            mode,
            curl,
            curl,
        );
        let outcome = engine::run(request).await;
        let mut filter = SandboxFilter::default();
        filter
            .labels
            .insert("petri.run".to_string(), run_id.clone());
        let observed = provider.list(&filter).await;
        let cleanup = prune::prune(PruneRequest {
            sandbox: config.clone(),
            run_id,
            run_dir: root.path().to_path_buf(),
            store,
            provider: SandboxProviderKind::DAYTONA,
        })
        .await;
        assert!(cleanup.unwrap().is_clean());
        let observed = observed.unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].snapshot.as_deref(), Some(runner.as_str()));
        assert_eq!(observed[0].network.as_ref(), Some(&expected));
        let outcome = outcome.unwrap();
        assert_eq!(outcome.status, RunStatus::Success, "{mode}: {outcome:?}");
        let deadline = Instant::now() + Duration::from_mins(2);
        loop {
            let remaining = provider.list(&filter).await.unwrap();
            if remaining
                .iter()
                .all(|sandbox| sandbox.state == SandboxState::Deleted)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "test sandbox deletion did not settle: {remaining:?}"
            );
            time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// A one-stage run whose probe script must succeed under `AllowAll`, and
/// whose `reach` command must fail under `Block`.
#[expect(
    clippy::too_many_arguments,
    reason = "each live test varies every input"
)]
fn probe_request(
    run_id: &str,
    root: &Path,
    store: &Arc<MemoryRunStore>,
    runtime: RuntimeSpec,
    provider: SandboxProviderKind,
    mode: EnvironmentNetworkMode,
    allowed_probe: &str,
    reach: &str,
) -> RunRequest {
    let script = if mode == EnvironmentNetworkMode::Block {
        format!("if {reach}; then exit 19; fi")
    } else {
        allowed_probe.to_string()
    };
    let workflow = format!(
        r#"digraph Network {{
            start [shape=Mdiamond]
            probe [shape=parallelogram, script="{script}"]
            exit [shape=Msquare]
            start -> probe -> exit
        }}"#
    );
    let graphs = support::admit(
        &[
            ("workflow.toml", support::SETTINGS),
            ("workflow.fabro", &workflow),
        ],
        Launch::default(),
        &runtime,
    );
    let mut request = support::run_request(
        run_id,
        root,
        graphs,
        store.clone(),
        runtime,
        support::no_questions(Arc::new(support::Silent)),
    );
    request.provider = provider;
    request.network = EnvironmentNetworkSettings {
        mode,
        allow: Vec::new(),
    };
    request
}

#[expect(
    clippy::disallowed_methods,
    reason = "live tests explicitly pass the operator's provider configuration"
)]
fn provider_env(name: &str) -> Option<String> {
    env::var(name).ok()
}
