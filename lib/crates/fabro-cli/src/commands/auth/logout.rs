use anyhow::{Result, bail};
use fabro_client::{AuthEntry, AuthStore, OAuthEntry};
use fabro_http::header::AUTHORIZATION;

use crate::args::AuthLogoutArgs;
use crate::command_context::CommandContext;
use crate::user_config;
use crate::user_config::ServerTarget;

pub(super) async fn logout_command(args: AuthLogoutArgs, base_ctx: &CommandContext) -> Result<()> {
    base_ctx.require_no_json_override()?;
    let printer = base_ctx.printer();

    let store = AuthStore::default();
    if args.all {
        let entries = store.list()?;
        if entries.is_empty() {
            fabro_util::printerr!(printer, "Not logged in to any servers.");
            return Ok(());
        }

        let mut warnings = Vec::new();
        for (target, entry) in entries {
            if let AuthEntry::OAuth(entry) = &entry {
                if let Err(error) = revoke_remote_session(&target, entry).await {
                    warnings.push(format_warning(&target, &error.to_string()));
                }
            }
            store.remove(&target)?;
        }

        for warning in warnings {
            fabro_util::printerr!(printer, "{warning}");
        }
        fabro_util::printerr!(printer, "Cleared local CLI auth sessions.");
        return Ok(());
    }

    let target = user_config::resolve_server_target(&args.server, base_ctx.user_settings())?;
    let Some(entry) = store.get(&target)? else {
        fabro_util::printerr!(printer, "Not logged in to {}.", target);
        return Ok(());
    };

    if let AuthEntry::OAuth(entry) = &entry {
        if let Err(error) = revoke_remote_session(&target, entry).await {
            fabro_util::printerr!(printer, "{}", format_warning(&target, &error.to_string()));
        }
    }
    store.remove(&target)?;
    fabro_util::printerr!(printer, "Logged out from {}.", target);
    Ok(())
}

async fn revoke_remote_session(target: &ServerTarget, entry: &OAuthEntry) -> Result<()> {
    let (http_client, base_url) = target.build_public_http_client()?;
    let response = http_client
        .post(format!("{base_url}/auth/cli/logout"))
        .header(AUTHORIZATION, format!("Bearer {}", entry.refresh_token))
        .send()
        .await?;
    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if body.is_empty() {
        bail!("request failed with status {status}");
    }
    bail!("request failed with status {status}: {body}")
}

fn format_warning(target: &ServerTarget, error: &str) -> String {
    format!(
        "Warning: removed local session for {target}, but remote revocation failed: {error}. The refresh token may remain valid until it expires."
    )
}

#[cfg(test)]
mod tests {
    use super::format_warning;
    use crate::user_config::ServerTarget;

    #[test]
    fn warning_mentions_local_removal_and_remote_failure() {
        let target = ServerTarget::http_url("https://fabro.example.com").unwrap();
        let warning = format_warning(&target, "request failed with status 500");
        assert!(warning.contains("removed local session"));
        assert!(warning.contains("remote revocation failed"));
    }
}
