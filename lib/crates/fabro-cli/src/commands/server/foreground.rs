use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use fabro_config::Storage;
use fabro_server::bind::BindRequest;
use fabro_server::serve;
use fabro_server::serve::ServeArgs;
use fabro_util::terminal::Styles;

use super::record;

pub(crate) async fn execute(
    record_path: PathBuf,
    mut serve_args: ServeArgs,
    bind: BindRequest,
    storage_dir: Option<PathBuf>,
    styles: &'static Styles,
) -> Result<()> {
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

    let log_path = storage_dir
        .as_ref()
        .map(|dir| Storage::new(dir).server_state().log_path())
        .unwrap_or_else(|| {
            record_path
                .parent()
                .map(|parent| parent.join("server.log"))
                .unwrap_or_else(|| PathBuf::from("server.log"))
        });
    let pid = std::process::id();

    serve::serve_command(serve_args, styles, storage_dir, move |resolved_bind| {
        record::write_server_record(
            &record_path,
            &record::ServerRecord {
                pid,
                bind: resolved_bind.clone(),
                log_path: log_path.clone(),
                started_at: Utc::now(),
            },
        )
    })
    .await
}
