use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use fabro_config::ServerRuntimeState;
use fabro_server::bind::BindRequest;
use fabro_server::serve;
use fabro_server::serve::ServeArgs;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use super::record;

pub(crate) async fn execute(
    record_path: PathBuf,
    mut serve_args: ServeArgs,
    bind: BindRequest,
    storage_dir: PathBuf,
    styles: &'static Styles,
    printer: Printer,
) -> Result<()> {
    let _ = printer;
    serve_args.bind = Some(bind.to_string());

    let _record_guard = scopeguard::guard(record_path.clone(), |path| {
        record::remove_server_record(&path);
    });

    let _socket_guard = if let BindRequest::Unix(ref path) = bind {
        let path = path.clone();
        Some(scopeguard::guard(path, |p| {
            let _ = std::fs::remove_file(p);
        }))
    } else {
        None
    };

    let log_path = ServerRuntimeState::new(&storage_dir).log_path();
    let dev_token_path = std::env::var_os("FABRO_DEV_TOKEN_PATH").map(PathBuf::from);
    let pid = std::process::id();

    Box::pin(serve::serve_command(
        serve_args,
        styles,
        Some(storage_dir),
        move |resolved_bind| {
            record::write_server_record(&record_path, &record::ServerRecord {
                pid,
                bind: resolved_bind.clone(),
                log_path: log_path.clone(),
                dev_token_path: dev_token_path.clone(),
                started_at: Utc::now(),
            })
        },
    ))
    .await
}
