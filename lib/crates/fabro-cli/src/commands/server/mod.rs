pub(crate) mod foreground;
pub(crate) mod record;
pub(crate) mod start;
pub(crate) mod status;
pub(crate) mod stop;

use std::time::Duration;

use anyhow::Result;
use fabro_server::serve::{self, ServeArgs};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{
    GlobalArgs, ServerCommand, ServerServeArgs, ServerStartArgs, ServerStatusArgs, ServerStopArgs,
};
use crate::user_config;

pub(crate) async fn dispatch(
    command: ServerCommand,
    _globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match command {
        ServerCommand::Start(ServerStartArgs {
            storage_dir,
            foreground,
            serve_args,
        }) => {
            let settings = user_config::load_settings_with_config_and_storage_dir(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
            )?;
            let storage_dir = user_config::storage_dir(&settings)?;
            let bind_addr =
                serve::resolve_bind_request_from_settings(&settings, serve_args.bind.as_deref())?;
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            Box::pin(start::execute(
                bind_addr,
                foreground,
                serve_args,
                storage_dir,
                styles,
                printer,
            ))
            .await
        }
        ServerCommand::Stop(ServerStopArgs {
            storage_dir,
            timeout,
        }) => {
            let settings = user_config::load_settings_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = user_config::storage_dir(&settings)?;
            stop::execute(&storage_dir, Duration::from_secs(timeout), printer).await;
            Ok(())
        }
        ServerCommand::Status(ServerStatusArgs { storage_dir, json }) => {
            let settings = user_config::load_settings_with_storage_dir(storage_dir.as_deref())?;
            let storage_dir = user_config::storage_dir(&settings)?;
            status::execute(&storage_dir, json, printer)
        }
        ServerCommand::Serve(ServerServeArgs {
            storage_dir,
            record_path,
            serve_args,
        }) => {
            let settings = user_config::load_settings_with_config_and_storage_dir(
                serve_args.config.as_deref(),
                storage_dir.as_deref(),
            )?;
            let active_config_path = Some(
                serve_args
                    .config
                    .clone()
                    .unwrap_or_else(|| user_config::active_settings_path(None)),
            );
            let bind_addr =
                serve::resolve_bind_request_from_settings(&settings, serve_args.bind.as_deref())?;
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            Box::pin(foreground::execute(
                record_path,
                ServeArgs {
                    config: active_config_path,
                    ..serve_args
                },
                bind_addr,
                storage_dir.clone_path(),
                styles,
                printer,
            ))
            .await
        }
    }
}
