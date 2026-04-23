#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `secret set` command: reads secret from stdin via blocking std::io::Read"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `secret set` command: reads secret from std::io::stdin"
)]

use std::io::{IsTerminal, Read as _};

use anyhow::{Context as _, Result, bail};
use fabro_api::types;
use fabro_util::printer::Printer;
use tokio::task::spawn_blocking;

use crate::args::{SecretSetArgs, SecretTypeArg};
use crate::server_client::Client;
use crate::shared::print_json_pretty;
use crate::shared::provider_auth::prompt_password;

fn api_secret_type(secret_type: SecretTypeArg) -> types::SecretType {
    match secret_type {
        SecretTypeArg::Environment => types::SecretType::Environment,
        SecretTypeArg::File => types::SecretType::File,
    }
}

async fn resolve_value(args: &SecretSetArgs) -> Result<String> {
    if let Some(value) = &args.value {
        return Ok(value.clone());
    }

    if args.value_stdin {
        let value = spawn_blocking(|| {
            let mut raw = String::new();
            std::io::stdin()
                .read_to_string(&mut raw)
                .context("failed to read secret value from stdin")?;
            let trimmed = raw.trim_end_matches(['\r', '\n']).to_string();
            anyhow::ensure!(!trimmed.is_empty(), "secret value from stdin is empty");
            Ok(trimmed)
        })
        .await??;
        return Ok(value);
    }

    if std::io::stdin().is_terminal() {
        let key = args.key.clone();
        let value = spawn_blocking(move || prompt_password(&format!("Value for {key}"))).await??;
        anyhow::ensure!(!value.is_empty(), "secret value is empty");
        return Ok(value);
    }

    bail!("secret value required: pass <VALUE>, use --value-stdin, or run interactively")
}

pub(super) async fn set_command(
    client: &Client,
    args: &SecretSetArgs,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    let value = resolve_value(args).await?;
    let meta = client
        .create_secret(types::CreateSecretRequest {
            name: args.key.clone(),
            value,
            type_: api_secret_type(args.r#type),
            description: args.description.clone(),
        })
        .await?;
    if json_output {
        print_json_pretty(&meta)?;
    } else {
        fabro_util::printerr!(printer, "Set {}", meta.name);
    }
    Ok(())
}
