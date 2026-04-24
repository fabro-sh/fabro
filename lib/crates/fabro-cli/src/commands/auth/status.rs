use anyhow::Result;
use chrono::{DateTime, Utc};
use fabro_client::{AuthEntry, AuthStore};
use fabro_util::dev_token::{read_dev_token_file, validate_dev_token_format};
use serde::Serialize;

use crate::args::AuthStatusArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config;
use crate::user_config::ServerTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OAuthState {
    Active,
    ExpiredRefreshable,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StatusRow {
    server:                   String,
    oauth_state:              OAuthState,
    access_token_expires_at:  DateTime<Utc>,
    refresh_token_expires_at: DateTime<Utc>,
    logged_in_at:             DateTime<Utc>,
    login:                    String,
    name:                     String,
    email:                    String,
    idp_issuer:               String,
    idp_subject:              String,
}

#[derive(Serialize)]
struct StatusOutput {
    servers:   Vec<StatusRow>,
    dev_token: &'static str,
}

pub(super) fn status_command(args: &AuthStatusArgs, ctx: &CommandContext) -> Result<()> {
    let printer = ctx.printer();
    let store = AuthStore::default();
    let now = Utc::now();
    let rows = if args.server.as_deref().is_some() {
        let target = user_config::resolve_server_target(&args.server, ctx.user_settings())?;
        filter_rows(&store, &target, now)?
    } else {
        all_rows(&store, now)?
    };
    let dev_token = if load_dev_token_if_available() {
        "active"
    } else {
        "not_set"
    };

    if ctx.explicit_json_requested() {
        print_json_pretty(&StatusOutput {
            servers: rows,
            dev_token,
        })?;
        return Ok(());
    }

    if rows.is_empty() {
        fabro_util::printerr!(printer, "Not logged in to any servers.");
        fabro_util::printerr!(printer, "Dev token: {dev_token}");
        return Ok(());
    }

    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            fabro_util::printerr!(printer, "");
        }
        fabro_util::printerr!(printer, "{}", row.server);
        fabro_util::printerr!(
            printer,
            "  OAuth: {} as {}",
            human_state(row.oauth_state),
            row.login
        );
        fabro_util::printerr!(
            printer,
            "  Name: {}",
            if row.name.is_empty() {
                "(not set)"
            } else {
                row.name.as_str()
            }
        );
        fabro_util::printerr!(
            printer,
            "  Email: {}",
            if row.email.is_empty() {
                "(not set)"
            } else {
                row.email.as_str()
            }
        );
        fabro_util::printerr!(
            printer,
            "  Access expires: {}",
            row.access_token_expires_at.to_rfc3339()
        );
        fabro_util::printerr!(
            printer,
            "  Refresh expires: {}",
            row.refresh_token_expires_at.to_rfc3339()
        );
    }
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(printer, "Dev token: {dev_token}");
    Ok(())
}

fn all_rows(store: &AuthStore, now: DateTime<Utc>) -> Result<Vec<StatusRow>> {
    Ok(store
        .list()?
        .into_iter()
        .map(|(target, entry)| status_row(&target, entry, now))
        .collect())
}

fn filter_rows(
    store: &AuthStore,
    target: &ServerTarget,
    now: DateTime<Utc>,
) -> Result<Vec<StatusRow>> {
    Ok(store
        .get(target)?
        .into_iter()
        .map(|entry| status_row(target, entry, now))
        .collect())
}

fn status_row(target: &ServerTarget, entry: AuthEntry, now: DateTime<Utc>) -> StatusRow {
    StatusRow {
        server:                   target.to_string(),
        oauth_state:              oauth_state(&entry, now),
        access_token_expires_at:  entry.access_token_expires_at,
        refresh_token_expires_at: entry.refresh_token_expires_at,
        logged_in_at:             entry.logged_in_at,
        login:                    entry.subject.login,
        name:                     entry.subject.name,
        email:                    entry.subject.email,
        idp_issuer:               entry.subject.idp_issuer,
        idp_subject:              entry.subject.idp_subject,
    }
}

fn oauth_state(entry: &AuthEntry, now: DateTime<Utc>) -> OAuthState {
    if entry.access_token_expires_at > now {
        OAuthState::Active
    } else if entry.refresh_token_expires_at > now {
        OAuthState::ExpiredRefreshable
    } else {
        OAuthState::Expired
    }
}

fn human_state(state: OAuthState) -> &'static str {
    match state {
        OAuthState::Active => "active",
        OAuthState::ExpiredRefreshable => "expired (refreshable)",
        OAuthState::Expired => "expired",
    }
}

fn load_dev_token_if_available() -> bool {
    let env_token = std::env::var("FABRO_DEV_TOKEN")
        .ok()
        .filter(|token| validate_dev_token_format(token));
    env_token.is_some()
        || read_dev_token_file(&fabro_util::Home::from_env().dev_token_path()).is_some()
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use fabro_client::{AuthEntry, StoredSubject};

    use super::{OAuthState, human_state, oauth_state};

    fn entry(access_offset_secs: i64, refresh_offset_secs: i64) -> AuthEntry {
        let now = chrono::Utc::now();
        AuthEntry {
            access_token:             "access".to_string(),
            access_token_expires_at:  now + Duration::seconds(access_offset_secs),
            refresh_token:            "refresh".to_string(),
            refresh_token_expires_at: now + Duration::seconds(refresh_offset_secs),
            subject:                  StoredSubject {
                idp_issuer:  "https://github.com".to_string(),
                idp_subject: "12345".to_string(),
                login:       "octocat".to_string(),
                name:        "The Octocat".to_string(),
                email:       "octocat@example.com".to_string(),
            },
            logged_in_at:             now,
        }
    }

    #[test]
    fn reports_active_when_access_token_is_live() {
        assert_eq!(
            oauth_state(&entry(60, 120), chrono::Utc::now()),
            OAuthState::Active
        );
    }

    #[test]
    fn reports_refreshable_when_access_is_expired_but_refresh_is_live() {
        assert_eq!(
            oauth_state(&entry(-60, 120), chrono::Utc::now()),
            OAuthState::ExpiredRefreshable
        );
    }

    #[test]
    fn reports_expired_when_both_tokens_are_expired() {
        assert_eq!(
            oauth_state(&entry(-120, -60), chrono::Utc::now()),
            OAuthState::Expired
        );
    }

    #[test]
    fn human_labels_match_expected_output() {
        assert_eq!(human_state(OAuthState::Active), "active");
        assert_eq!(
            human_state(OAuthState::ExpiredRefreshable),
            "expired (refreshable)"
        );
        assert_eq!(human_state(OAuthState::Expired), "expired");
    }
}
