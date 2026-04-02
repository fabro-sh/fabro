use std::path::PathBuf;

use anyhow::Result;
use fabro_server::bind::Bind;
use fabro_server::serve;
use fabro_server::serve::ServeArgs;
use fabro_util::terminal::Styles;

use super::record;

pub(crate) async fn execute(
    record_path: PathBuf,
    mut serve_args: ServeArgs,
    bind: Bind,
    storage_dir: Option<PathBuf>,
    styles: &'static Styles,
) -> Result<()> {
    let _ = fabro_proc::title_init();
    fabro_proc::title_set(&format!("fabro: server {bind}"));

    // Ensure serve_args carries the resolved bind so the server binds correctly.
    serve_args.bind = Some(bind.to_string());

    let _record_guard = scopeguard::guard(record_path, |path| {
        record::remove_server_record(&path);
    });

    // If Unix socket, clean up socket file on exit
    let _socket_guard = if let Bind::Unix(ref path) = bind {
        let path = path.clone();
        Some(scopeguard::guard(path, |p| {
            let _ = std::fs::remove_file(p);
        }))
    } else {
        None
    };

    serve::serve_command(serve_args, styles, storage_dir).await
}
