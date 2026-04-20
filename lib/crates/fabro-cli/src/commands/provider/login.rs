use anyhow::Result;
use fabro_api::types;
use fabro_auth::credential_id_for;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{ProviderLoginArgs, require_no_json_override};
use crate::command_context::CommandContext;
use crate::shared::provider_auth;

pub(super) async fn login_command(
    args: ProviderLoginArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    require_no_json_override(process_local_json)?;
    let s = Styles::detect_stderr();
    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    let server = ctx.server().await?;
    let credential = if args.api_key_stdin {
        provider_auth::authenticate_provider_with_api_key_source(
            args.provider,
            provider_auth::ApiKeySource::Stdin,
            &s,
            printer,
        )
        .await?
    } else {
        provider_auth::authenticate_provider(args.provider, &s, printer).await?
    };
    let credential_id = credential_id_for(&credential).map_err(anyhow::Error::msg)?;
    let value = serde_json::to_string(&credential)?;
    let request_credential_id = credential_id.clone();
    let request_value = value.clone();

    server
        .send_api(|client| async move {
            client
                .create_secret()
                .body(types::CreateSecretRequest {
                    name:        request_credential_id.clone(),
                    value:       request_value.clone(),
                    type_:       types::SecretType::Credential,
                    description: None,
                })
                .send()
                .await
        })
        .await?;
    fabro_util::printerr!(
        printer,
        "  {} Saved {}",
        s.green.apply_to("✔"),
        credential_id
    );
    Ok(())
}
