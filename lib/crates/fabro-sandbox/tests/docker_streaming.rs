#![cfg(feature = "docker")]

use std::sync::Arc;

use bollard::Docker;
use fabro_sandbox::{CommandOutputCallback, DockerSandbox, DockerSandboxOptions, Sandbox};
use tokio::sync::Mutex;

#[tokio::test]
#[ignore = "requires real Docker container lifecycle; run explicitly when changing Docker exec integration"]
async fn streaming_timeout_terminates_docker_exec_before_returning() {
    let image = "buildpack-deps:noble";
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        return;
    };
    if docker.inspect_image(image).await.is_err() {
        return;
    }

    let sandbox = DockerSandbox::new(
        DockerSandboxOptions {
            image: image.to_string(),
            auto_pull: false,
            skip_clone: true,
            ..DockerSandboxOptions::default()
        },
        None,
        None,
        None,
        None,
    )
    .expect("docker sandbox should construct");
    sandbox
        .initialize()
        .await
        .expect("docker sandbox should initialize");

    let chunks = Arc::new(Mutex::new(Vec::new()));
    let callback_chunks = Arc::clone(&chunks);
    let callback: CommandOutputCallback = Arc::new(move |_stream, bytes| {
        let callback_chunks = Arc::clone(&callback_chunks);
        Box::pin(async move {
            callback_chunks.lock().await.extend(bytes);
            Ok(())
        })
    });

    let marker = "fabro_streaming_timeout_sentinel";
    let result = sandbox
        .exec_command_streaming(
            &format!("trap '' HUP TERM; echo start; sleep 5 # {marker}"),
            Some(200),
            None,
            None,
            None,
            callback,
        )
        .await
        .expect("streaming command should return a timeout result");

    assert!(result.result.is_timed_out());
    assert!(
        String::from_utf8_lossy(&chunks.lock().await).contains("start"),
        "stream should include output emitted before timeout"
    );

    let probe = sandbox
        .exec_command(
            "marker='fabro_streaming_timeout_''sentinel'; \
             ps -eo pid,args | awk -v marker=\"$marker\" \
             'index($0, marker) && $0 !~ /awk/ && $0 !~ /ps -eo/ { print }'",
            1_000,
            None,
            None,
            None,
        )
        .await
        .expect("process probe should run");
    sandbox
        .cleanup()
        .await
        .expect("docker cleanup should succeed");

    assert!(
        !probe.stdout.contains(marker),
        "timed-out docker exec should be terminated before returning, found: {}",
        probe.stdout
    );
}

#[tokio::test]
#[ignore = "requires real Docker container lifecycle, image, network, and a public GitHub clone"]
async fn cloned_docker_sandbox_uses_repos_checkout_and_workspace_symlink() {
    let image = "buildpack-deps:noble";
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        return;
    };
    if docker.inspect_image(image).await.is_err() {
        return;
    }

    let sandbox = DockerSandbox::new(
        DockerSandboxOptions {
            image: image.to_string(),
            auto_pull: false,
            skip_clone: false,
            ..DockerSandboxOptions::default()
        },
        None,
        None,
        Some("https://github.com/brynary/rack-test".to_string()),
        None,
    )
    .expect("docker sandbox should construct");
    sandbox
        .initialize()
        .await
        .expect("docker sandbox should initialize");

    assert_eq!(sandbox.working_directory(), "/workspace/rack-test");

    let result = sandbox
        .exec_command(
            "test -d /repos/brynary/rack-test/.git && \
             test -L /workspace/rack-test && \
             test \"$(readlink /workspace/rack-test)\" = /repos/brynary/rack-test && \
             test \"$(git -C /repos/brynary/rack-test rev-parse HEAD)\" = \
                  \"$(git -C /workspace/rack-test rev-parse HEAD)\" && \
             git rev-parse --is-inside-work-tree",
            10_000,
            None,
            None,
            None,
        )
        .await
        .expect("layout verification command should run");
    sandbox
        .cleanup()
        .await
        .expect("docker cleanup should succeed");

    assert!(
        result.is_success(),
        "layout verification failed: stdout={} stderr={}",
        result.stdout,
        result.stderr
    );
    assert!(result.stdout.contains("true"));
}

// Regression test for glob patterns that contain a path separator. Before the
// glob fix, the remote providers ran `find <base> -name <pattern>`, and
// `find -name` matches only the basename and rejects patterns containing `/`.
// So `*/SKILL.md` and `**/SKILL.md` silently returned an empty list inside a
// real container even though the files existed. Both `glob` calls below fail
// against that old implementation and pass once traversal and matching are
// split (find files, then match host-side).
#[tokio::test]
#[ignore = "requires real Docker container lifecycle; run explicitly when changing Sandbox::glob"]
async fn docker_glob_matches_patterns_containing_a_path_separator() {
    let image = "buildpack-deps:noble";
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        return;
    };
    if docker.inspect_image(image).await.is_err() {
        return;
    }

    let sandbox = DockerSandbox::new(
        DockerSandboxOptions {
            image: image.to_string(),
            auto_pull: false,
            skip_clone: true,
            ..DockerSandboxOptions::default()
        },
        None,
        None,
        None,
        None,
    )
    .expect("docker sandbox should construct");
    sandbox
        .initialize()
        .await
        .expect("docker sandbox should initialize");

    // Build a skills tree with a SKILL.md at the search root, one level below
    // it, and two levels below it.
    let seed = sandbox
        .exec_command(
            "mkdir -p skills/patch skills/nested/deeper && \
             touch skills/SKILL.md skills/patch/SKILL.md skills/nested/deeper/SKILL.md",
            10_000,
            None,
            None,
            None,
        )
        .await
        .expect("seed command should run");

    // `*/SKILL.md` matches exactly one path segment: only the file one level
    // below the search directory, not the root file or the deeper one.
    let one_level = sandbox.glob("*/SKILL.md", Some("skills")).await;
    // `**/SKILL.md` matches at any depth, including several levels down.
    let recursive = sandbox.glob("**/SKILL.md", Some("skills")).await;

    sandbox
        .cleanup()
        .await
        .expect("docker cleanup should succeed");

    assert!(
        seed.is_success(),
        "seeding the skills tree failed: stdout={} stderr={}",
        seed.stdout,
        seed.stderr
    );

    let one_level = one_level.expect("glob should run");
    assert_eq!(
        one_level.len(),
        1,
        "`*/SKILL.md` should match exactly one level below the search dir, got: {one_level:?}"
    );
    assert!(
        one_level[0].ends_with("skills/patch/SKILL.md"),
        "`*/SKILL.md` should match the one-level-deep file, got: {one_level:?}"
    );

    let recursive = recursive.expect("recursive glob should run");
    assert!(
        recursive
            .iter()
            .any(|path| path.ends_with("skills/nested/deeper/SKILL.md")),
        "`**/SKILL.md` should match files nested several levels deep, got: {recursive:?}"
    );
}
