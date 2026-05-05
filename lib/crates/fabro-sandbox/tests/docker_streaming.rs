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
