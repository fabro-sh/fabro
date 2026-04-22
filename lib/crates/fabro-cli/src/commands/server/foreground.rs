use std::path::PathBuf;

use anyhow::Result;
use fabro_config::RuntimeDirectory;
use fabro_config::bind::BindRequest;
use fabro_config::daemon::ServerDaemon;
use fabro_server::serve;
use fabro_server::serve::ServeArgs;
use fabro_util::terminal::Styles;

/// Run `serve::serve_command` with scopeguards that write/remove the server
/// daemon record and clean up a Unix socket on exit. Used by both
/// `fabro server serve` and `fabro server start --foreground`.
pub(crate) async fn serve_with_daemon_record(
    mut serve_args: ServeArgs,
    bind: BindRequest,
    storage_dir: PathBuf,
    styles: &'static Styles,
) -> Result<()> {
    serve_args.bind = Some(bind.to_string());

    let runtime_directory = RuntimeDirectory::new(&storage_dir);
    let _record_guard = scopeguard::guard(runtime_directory.clone(), |dir| {
        ServerDaemon::remove(&dir);
    });

    let _socket_guard = if let BindRequest::Unix(ref path) = bind {
        let path = path.clone();
        Some(scopeguard::guard(path, |p| {
            let _ = std::fs::remove_file(p);
        }))
    } else {
        None
    };

    let log_path = runtime_directory.log_path();
    let pid = std::process::id();
    let daemon_dir = runtime_directory;

    Box::pin(serve::serve_command(
        serve_args,
        styles,
        Some(storage_dir),
        move |resolved_bind| {
            ServerDaemon::new(pid, resolved_bind.clone(), log_path.clone()).write(&daemon_dir)
        },
    ))
    .await
}
