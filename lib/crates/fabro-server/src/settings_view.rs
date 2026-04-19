//! Outward-facing view of [`SettingsLayer`] for API responses.
//!
//! `/api/v1/settings` and `/api/v1/runs/:id/settings` return the server's v2
//! [`SettingsLayer`] directly as JSON so authenticated clients (the `fabro
//! settings` CLI, the web UI) can see the effective configuration. Before
//! serialization, this module drops the handful of fields that would leak
//! host-specific filesystem or network layout.
//!
//! ## What gets dropped
//!
//! Per the requirements doc, only the transport bind needs redaction now:
//!
//! - `server.listen` — the whole subtree. Bind addresses and socket paths
//!   reveal network topology and host filesystem layout.
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

use fabro_types::settings::{Settings, SettingsLayer};
use serde::Deserialize;

pub(crate) const RESOLVED_VIEW_HEADER_NAME: &str = "X-Fabro-Settings-View";
pub(crate) const RESOLVED_VIEW_HEADER_VALUE: &str = "resolved";

const REDACTED_PATHS: &[&[&str]] = &[&["server", "listen"]];

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SettingsApiView {
    #[default]
    Layer,
    Resolved,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct SettingsQuery {
    #[serde(default)]
    pub(crate) view: SettingsApiView,
}

/// Build a redacted clone of `settings` safe to serialize outward.
///
/// See the module docs for the drop-list rationale.
#[must_use]
pub(crate) fn redact_for_api(settings: &SettingsLayer) -> SettingsLayer {
    let mut out = settings.clone();

    if let Some(server) = out.server.as_mut() {
        // Bind address + TLS key/cert paths: host operational details.
        server.listen = None;
    }

    out
}

pub(crate) fn redact_resolved_value(settings: &Settings) -> serde_json::Result<serde_json::Value> {
    let mut value = serde_json::to_value(settings)?;
    redact_value_paths(&mut value);
    Ok(value)
}

pub(crate) fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                strip_nulls(child);
            }
            map.retain(|_, child| !child.is_null());
        }
        serde_json::Value::Array(values) => {
            for child in values {
                strip_nulls(child);
            }
        }
        _ => {}
    }
}

fn redact_value_paths(value: &mut serde_json::Value) {
    for path in REDACTED_PATHS {
        remove_path(value, path);
    }
}

fn remove_path(value: &mut serde_json::Value, path: &[&str]) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };

    let Some(object) = value.as_object_mut() else {
        return;
    };

    if tail.is_empty() {
        object.remove(*head);
        return;
    }

    if let Some(child) = object.get_mut(*head) {
        remove_path(child, tail);
    }
}

#[cfg(test)]
mod tests {
    use fabro_config::parse_settings_layer;

    use super::*;

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
"#,
        );
        let redacted = redact_for_api(&settings);
        assert!(redacted.server.unwrap().listen.is_none());
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

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

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
        let auth = server.auth.unwrap();
        assert_eq!(auth.methods.unwrap().len(), 2);
        assert_eq!(auth.github.unwrap().allowed_usernames, vec!["alice"]);
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

    #[test]
    fn redacts_dense_resolved_settings_with_the_same_secret_paths() {
        let settings = parse(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.auth]
methods = ["github", "dev-token"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
        );

        let resolved = fabro_config::resolve(&settings).expect("settings should resolve");
        let mut redacted =
            redact_resolved_value(&resolved).expect("resolved settings should serialize");

        assert!(redacted["server"].get("listen").is_none());
        assert_eq!(redacted["server"]["auth"]["methods"][0], "github");
        assert_eq!(
            redacted["server"]["auth"]["github"]["allowed_usernames"][0],
            "alice"
        );
        assert_eq!(
            redacted["server"]["storage"]["root"],
            "{{ env.FABRO_STORAGE_ROOT }}"
        );

        strip_nulls(&mut redacted);
        assert!(redacted["server"].get("listen").is_none());
    }
}
