#[cfg(feature = "daytona")]
mod daytona_streaming_live {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result, ensure};
    use fabro_sandbox::daytona::{DaytonaConfig, DaytonaSandbox};
    use fabro_sandbox::{CommandOutputCallback, ExecStreamingResult, Sandbox};
    use fabro_types::{CommandOutputStream, CommandTermination};
    use tokio::sync::Mutex;
    use tokio::time::{Instant, sleep};
    use tokio_util::sync::CancellationToken;

    #[derive(Debug, Clone)]
    struct CapturedChunk {
        stream: CommandOutputStream,
        text:   String,
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires live Daytona credentials and provisions a sandbox"]
    async fn daytona_streaming_live_smoke() -> Result<()> {
        ensure!(
            std::env::var_os("DAYTONA_API_KEY").is_some(),
            "DAYTONA_API_KEY must be set to run this live smoke test"
        );

        let sandbox = Arc::new(
            DaytonaSandbox::new(
                DaytonaConfig {
                    skip_clone: true,
                    ..Default::default()
                },
                None,
                None,
                None,
                None,
                None,
            )
            .await?,
        );

        sandbox.initialize().await?;

        let smoke_result = run_smoke(Arc::clone(&sandbox)).await;
        let cleanup_result = sandbox.cleanup().await.context("clean up Daytona sandbox");

        smoke_result?;
        cleanup_result?;

        Ok(())
    }

    async fn run_smoke(sandbox: Arc<DaytonaSandbox>) -> Result<()> {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let cancel_token = CancellationToken::new();
        let callback = capture_callback(Arc::clone(&chunks));
        let sandbox_for_exec = Arc::clone(&sandbox);
        let cancel_for_exec = cancel_token.clone();

        let live_exec = tokio::spawn(async move {
            sandbox_for_exec
                .exec_command_streaming(
                    "printf 'live-out\\n'; printf 'live-err\\n' >&2; sleep 30",
                    60_000,
                    None,
                    None,
                    Some(cancel_for_exec),
                    callback,
                )
                .await
        });

        let saw_live_stdout_and_stderr =
            wait_for_chunks(&chunks, Duration::from_secs(20), |chunks| {
                contains_chunk(chunks, CommandOutputStream::Stdout, "live-out")
                    && contains_chunk(chunks, CommandOutputStream::Stderr, "live-err")
            })
            .await;

        cancel_token.cancel();

        let live_result = live_exec
            .await
            .context("join live cancel command task")?
            .context("run live cancel command")?;

        ensure!(
            saw_live_stdout_and_stderr,
            "expected live stdout and stderr chunks before cancellation, got {chunks:?}",
            chunks = chunks.lock().await
        );
        ensure!(
            live_result.live_streaming,
            "expected Daytona command logs to stream before completion"
        );
        ensure!(
            live_result.streams_separated,
            "expected Daytona command logs to separate stdout and stderr"
        );
        ensure_eq(
            live_result.result.termination,
            CommandTermination::Cancelled,
            "cancelled command should preserve cancellation termination",
        )?;
        ensure_contains(
            &live_result.result.stdout,
            "live-out",
            "cancelled command stdout should preserve partial logs",
        )?;
        ensure_contains(
            &live_result.result.stderr,
            "live-err",
            "cancelled command stderr should preserve partial logs",
        )?;

        let (nonzero, nonzero_chunks) = run_captured(
            sandbox.as_ref(),
            "printf 'exit-out\\n'; printf 'exit-err\\n' >&2; exit 7",
            30_000,
            None,
        )
        .await?;
        ensure_eq(
            nonzero.result.exit_code,
            Some(7),
            "nonzero command should preserve the Daytona exit code",
        )?;
        ensure_eq(
            nonzero.result.termination,
            CommandTermination::Exited,
            "nonzero command should be represented as a completed process",
        )?;
        ensure_contains(
            &nonzero.result.stdout,
            "exit-out",
            "nonzero command stdout should be captured",
        )?;
        ensure_contains(
            &nonzero.result.stderr,
            "exit-err",
            "nonzero command stderr should be captured",
        )?;
        ensure!(
            contains_chunk(&nonzero_chunks, CommandOutputStream::Stdout, "exit-out"),
            "nonzero command should stream stdout chunks"
        );
        ensure!(
            contains_chunk(&nonzero_chunks, CommandOutputStream::Stderr, "exit-err"),
            "nonzero command should stream stderr chunks"
        );

        let (timed_out, _) = run_captured(
            sandbox.as_ref(),
            "printf 'timeout-out\\n'; printf 'timeout-err\\n' >&2; sleep 30",
            1_500,
            None,
        )
        .await?;
        ensure_eq(
            timed_out.result.termination,
            CommandTermination::TimedOut,
            "timed-out command should preserve timeout termination",
        )?;
        ensure_contains(
            &timed_out.result.stdout,
            "timeout-out",
            "timed-out command stdout should preserve partial logs",
        )?;
        ensure_contains(
            &timed_out.result.stderr,
            "timeout-err",
            "timed-out command stderr should preserve partial logs",
        )?;

        Ok(())
    }

    async fn run_captured(
        sandbox: &DaytonaSandbox,
        command: &str,
        timeout_ms: u64,
        cancel_token: Option<CancellationToken>,
    ) -> Result<(ExecStreamingResult, Vec<CapturedChunk>)> {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let callback = capture_callback(Arc::clone(&chunks));
        let result = sandbox
            .exec_command_streaming(command, timeout_ms, None, None, cancel_token, callback)
            .await?;
        let chunks = chunks.lock().await.clone();

        Ok((result, chunks))
    }

    fn capture_callback(chunks: Arc<Mutex<Vec<CapturedChunk>>>) -> CommandOutputCallback {
        Arc::new(move |stream, bytes| {
            let chunks = Arc::clone(&chunks);
            Box::pin(async move {
                chunks.lock().await.push(CapturedChunk {
                    stream,
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                });
                Ok(())
            })
        })
    }

    async fn wait_for_chunks(
        chunks: &Arc<Mutex<Vec<CapturedChunk>>>,
        timeout_after: Duration,
        predicate: impl Fn(&[CapturedChunk]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout_after;
        loop {
            if predicate(&chunks.lock().await) {
                return true;
            }

            if Instant::now() >= deadline {
                return false;
            }

            sleep(Duration::from_millis(100)).await;
        }
    }

    fn contains_chunk(chunks: &[CapturedChunk], stream: CommandOutputStream, text: &str) -> bool {
        chunks
            .iter()
            .any(|chunk| chunk.stream == stream && chunk.text.contains(text))
    }

    fn ensure_contains(value: &str, needle: &str, message: &str) -> Result<()> {
        ensure!(
            value.contains(needle),
            "{message}: expected to find {needle:?} in {value:?}"
        );
        Ok(())
    }

    fn ensure_eq<T>(actual: T, expected: T, message: &str) -> Result<()>
    where
        T: std::fmt::Debug + PartialEq,
    {
        ensure!(
            actual == expected,
            "{message}: expected {expected:?}, got {actual:?}"
        );
        Ok(())
    }
}
