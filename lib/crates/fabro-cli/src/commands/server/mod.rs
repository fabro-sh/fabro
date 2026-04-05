pub(crate) mod foreground;
pub(crate) mod record;
pub(crate) mod start;
pub(crate) mod status;
pub(crate) mod stop;

use std::time::Duration;

use anyhow::Result;
use fabro_server::bind;
use fabro_server::bind::Bind;
use fabro_util::terminal::Styles;

use crate::args::{
    GlobalArgs, ServerCommand, ServerServeArgs, ServerStartArgs, ServerStatusArgs, ServerStopArgs,
};
use crate::user_config;

pub(crate) async fn dispatch(command: ServerCommand, _globals: &GlobalArgs) -> Result<()> {
    match command {
        ServerCommand::Start(ServerStartArgs {
            storage_dir,
            foreground,
            serve_args,
        }) => {
            let settings =
                user_config::load_user_settings_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = settings.storage_dir();
            let bind_addr = match serve_args.bind.as_deref() {
                Some(s) => bind::parse_bind(s)?,
                None => Bind::Unix(storage_dir.join("fabro.sock")),
            };
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            start::execute(bind_addr, foreground, serve_args, storage_dir, styles).await
        }
        ServerCommand::Stop(ServerStopArgs {
            storage_dir,
            timeout,
        }) => {
            let settings =
                user_config::load_user_settings_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = settings.storage_dir();
            stop::execute(&storage_dir, Duration::from_secs(timeout));
            Ok(())
        }
        ServerCommand::Status(ServerStatusArgs { storage_dir, json }) => {
            let settings =
                user_config::load_user_settings_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = settings.storage_dir();
            status::execute(&storage_dir, json)
        }
        ServerCommand::Serve(ServerServeArgs {
            storage_dir,
            record_path,
            serve_args,
        }) => {
            let bind_addr = if let Some(s) = serve_args.bind.as_deref() {
                bind::parse_bind(s)?
            } else {
                // __serve should always receive an explicit --bind from the parent,
                // but fall back to the storage dir default if missing.
                let settings =
                    user_config::load_user_settings_with_storage_dir(storage_dir.as_deref())?;
                Bind::Unix(settings.storage_dir().join("fabro.sock"))
            };
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            foreground::execute(
                record_path,
                serve_args,
                bind_addr,
                storage_dir.clone_path(),
                styles,
            )
            .await
        }
    }
}
