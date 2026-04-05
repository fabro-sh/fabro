mod list;
mod rm;
mod set;

use anyhow::{Result, anyhow};

use crate::args::{GlobalArgs, SecretCommand, SecretNamespace};
use crate::server_client;

fn map_api_error<E>(err: progenitor_client::Error<E>) -> anyhow::Error
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            let status = response.status();
            if let Ok(value) = serde_json::to_value(response.into_inner()) {
                if let Some(detail) = value
                    .get("errors")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|errors| errors.first())
                    .and_then(|entry| entry.get("detail"))
                    .and_then(serde_json::Value::as_str)
                {
                    return anyhow!("{detail}");
                }
            }
            anyhow!("request failed with status {status}")
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            anyhow!("request failed with status {}", response.status())
        }
        other => anyhow!("{other}"),
    }
}

pub(crate) async fn dispatch(ns: SecretNamespace, globals: &GlobalArgs) -> Result<()> {
    let client = server_client::connect_server_backed_api_client(&ns.target).await?;
    match ns.command {
        SecretCommand::List(args) => list::list_command(&client, &args, globals).await,
        SecretCommand::Rm(args) => rm::rm_command(&client, &args, globals).await,
        SecretCommand::Set(args) => set::set_command(&client, &args, globals).await,
    }
}
