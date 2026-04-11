//! Outward-facing view of [`SettingsLayer`] for API responses.
//!
//! `/api/v1/settings` and `/api/v1/runs/:id/settings` return the server's v2
//! [`SettingsLayer`] directly as JSON so authenticated clients (the `fabro
//! settings` CLI, the web UI) can see the effective configuration. Before
//! serialization, this module drops the handful of fields that would leak
//! operational secrets or host-specific filesystem layout.
//!
//! ## What gets dropped
//!
//! Per the requirements doc (R16, R52, R53, R79–R81) and the Stage 6.6 plan:
//!
//! - `server.listen` — the whole subtree. Bind address reveals network
//!   topology; `[server.listen.tls]` cert/key/ca paths reveal the host
//!   filesystem layout.
//! - `server.auth.api.jwt.issuer` and `jwt.audience` — auth topology. Keeps
//!   `enabled` so clients can tell whether JWT auth is on.
//! - `server.auth.api.mtls.ca` — filesystem path to the CA bundle. Keeps
//!   `enabled`.
//! - `server.auth.web.providers.github.client_secret` — explicit OAuth
//!   secret. Keeps `enabled` and `client_id` (the latter is public in OAuth).
//!
//! ## Why that's all
//!
//! The rest of the v2 tree is either:
//!
//! - A literal non-secret value (storage root, scheduler limit, integration
//!   slug, feature flag), OR
//! - An [`InterpString`] containing `{{ env.NAME }}` tokens. `InterpString`'s
//!   default serialization preserves the *unresolved* template form, so the
//!   wire payload surfaces `"Bearer {{ env.TOKEN }}"` instead of the resolved
//!   secret value. No additional redaction pass is needed.
//!
//! Any future field that carries a raw secret in-band (without env
//! interpolation) must be added to the drop list below.

use fabro_types::settings::SettingsLayer;

/// Build a redacted clone of `settings` safe to serialize outward.
///
/// See the module docs for the drop-list rationale.
#[must_use]
pub(crate) fn redact_for_api(settings: &SettingsLayer) -> SettingsLayer {
    let mut out = settings.clone();

    if let Some(server) = out.server.as_mut() {
        // Bind address + TLS key/cert paths: host operational details.
        server.listen = None;

        if let Some(auth) = server.auth.as_mut() {
            if let Some(api) = auth.api.as_mut() {
                if let Some(jwt) = api.jwt.as_mut() {
                    jwt.issuer = None;
                    jwt.audience = None;
                }
                if let Some(mtls) = api.mtls.as_mut() {
                    mtls.ca = None;
                }
            }
            if let Some(web) = auth.web.as_mut() {
                if let Some(providers) = web.providers.as_mut() {
                    if let Some(github) = providers.github.as_mut() {
                        github.client_secret = None;
                    }
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_config::parse_settings_layer;

    fn parse(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("fixture should parse")
    }

    #[test]
    fn drops_server_listen_entirely() {
        let settings = parse(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/tls/cert.pem"
key = "/etc/fabro/tls/key.pem"
ca = "/etc/fabro/tls/ca.pem"
"#,
        );
        let redacted = redact_for_api(&settings);
        assert!(redacted.server.unwrap().listen.is_none());
    }

    #[test]
    fn drops_jwt_issuer_and_audience_but_keeps_enabled() {
        let settings = parse(
            r#"
_version = 1

[server.auth.api.jwt]
enabled = true
issuer = "https://auth.example.com"
audience = "fabro"
"#,
        );
        let redacted = redact_for_api(&settings);
        let jwt = redacted
            .server
            .unwrap()
            .auth
            .unwrap()
            .api
            .unwrap()
            .jwt
            .unwrap();
        assert_eq!(jwt.enabled, Some(true));
        assert!(jwt.issuer.is_none());
        assert!(jwt.audience.is_none());
    }

    #[test]
    fn drops_mtls_ca_path_but_keeps_enabled() {
        let settings = parse(
            r#"
_version = 1

[server.auth.api.mtls]
enabled = true
ca = "/etc/fabro/tls/ca.pem"
"#,
        );
        let redacted = redact_for_api(&settings);
        let mtls = redacted
            .server
            .unwrap()
            .auth
            .unwrap()
            .api
            .unwrap()
            .mtls
            .unwrap();
        assert_eq!(mtls.enabled, Some(true));
        assert!(mtls.ca.is_none());
    }

    #[test]
    fn drops_github_client_secret_but_keeps_client_id_and_enabled() {
        let settings = parse(
            r#"
_version = 1

[server.auth.web.providers.github]
enabled = true
client_id = "Iv1.abcdef"
client_secret = "{{ env.GITHUB_OAUTH_SECRET }}"
"#,
        );
        let redacted = redact_for_api(&settings);
        let github = redacted
            .server
            .unwrap()
            .auth
            .unwrap()
            .web
            .unwrap()
            .providers
            .unwrap()
            .github
            .unwrap();
        assert_eq!(github.enabled, Some(true));
        assert!(github.client_id.is_some());
        assert!(github.client_secret.is_none());
    }

    #[test]
    fn preserves_run_cli_project_and_features() {
        let settings = parse(
            r#"
_version = 1

[project]
name = "Fabro"

[run]
goal = "ship it"

[run.model]
provider = "anthropic"
name = "sonnet"

[cli.output]
verbosity = "verbose"

[features]
session_sandboxes = true

[server.scheduler]
max_concurrent_runs = 9

[server.storage]
root = "/srv/fabro"

[server.integrations.github]
app_id = "12345"
client_id = "Iv1.abcdef"
slug = "fabro-app"
"#,
        );
        let redacted = redact_for_api(&settings);
        assert!(redacted.project.is_some());
        let run = redacted.run.unwrap();
        assert!(run.goal.is_some());
        assert!(run.model.is_some());
        assert!(redacted.cli.is_some());
        assert!(redacted.features.is_some());
        let server = redacted.server.unwrap();
        assert_eq!(
            server.scheduler.and_then(|s| s.max_concurrent_runs),
            Some(9)
        );
        assert!(server.storage.is_some());
        let github = server.integrations.unwrap().github.unwrap();
        assert!(github.app_id.is_some());
        assert!(github.client_id.is_some());
        assert!(github.slug.is_some());
    }

    #[test]
    fn preserves_env_templates_for_non_redacted_fields() {
        let settings = parse(
            r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"

[server.integrations.slack]
default_channel = "{{ env.SLACK_CHANNEL }}"
"#,
        );

        let redacted = redact_for_api(&settings);
        let server = redacted
            .server
            .expect("server config should remain present");
        assert_eq!(
            server
                .storage
                .and_then(|storage| storage.root)
                .map(|value| value.as_source()),
            Some("{{ env.FABRO_STORAGE_ROOT }}".to_string())
        );
        assert_eq!(
            server
                .integrations
                .and_then(|integrations| integrations.slack)
                .and_then(|slack| slack.default_channel)
                .map(|value| value.as_source()),
            Some("{{ env.SLACK_CHANNEL }}".to_string())
        );
    }
}
