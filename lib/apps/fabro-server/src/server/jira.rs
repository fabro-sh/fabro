use std::sync::{Arc, Mutex};

use fabro_jira::JiraClient;
use fabro_static::EnvVars;
use fabro_types::settings::server::JiraIntegrationSettings;

use super::sanitize_integration_error;

const SITE_URL_SETTING: &str = "server.integrations.jira.site_url";
const EMAIL_SETTING: &str = "server.integrations.jira.email";

/// The Jira client and the last error it hit, shown on the integrations page.
pub struct JiraService {
    #[allow(
        dead_code,
        reason = "read by the Jira polling and write-back tasks that follow"
    )]
    client:     JiraClient,
    last_error: Arc<Mutex<Option<String>>>,
}

#[allow(
    dead_code,
    reason = "used by the Jira polling and write-back tasks that follow"
)]
impl JiraService {
    pub(crate) fn new(client: JiraClient) -> Self {
        Self {
            client,
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn client(&self) -> &JiraClient {
        &self.client
    }

    pub(crate) fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .expect("jira last_error lock should not be poisoned")
            .clone()
    }

    pub(crate) fn record_error(&self, error: &str) {
        *self
            .last_error
            .lock()
            .expect("jira last_error lock should not be poisoned") =
            Some(sanitize_integration_error(error));
    }

    pub(crate) fn clear_error(&self) {
        *self
            .last_error
            .lock()
            .expect("jira last_error lock should not be poisoned") = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum JiraCredentialResolution {
    Configured {
        site_url:  String,
        email:     String,
        api_token: String,
    },
    Missing {
        names: Vec<String>,
    },
}

pub(crate) fn resolve_jira_credentials(
    settings: &JiraIntegrationSettings,
    lookup: impl Fn(&str) -> Option<String>,
) -> JiraCredentialResolution {
    let non_empty = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let site_url = non_empty(settings.site_url.as_deref());
    let email = non_empty(settings.email.as_deref());
    let api_token = non_empty(lookup(EnvVars::FABRO_JIRA_API_TOKEN).as_deref());

    match (site_url, email, api_token) {
        (Some(site_url), Some(email), Some(api_token)) => JiraCredentialResolution::Configured {
            site_url,
            email,
            api_token,
        },
        (site_url, email, api_token) => {
            let mut names = Vec::new();
            if site_url.is_none() {
                names.push(SITE_URL_SETTING.to_string());
            }
            if email.is_none() {
                names.push(EMAIL_SETTING.to_string());
            }
            if api_token.is_none() {
                names.push(EnvVars::FABRO_JIRA_API_TOKEN.to_string());
            }
            names.sort();
            JiraCredentialResolution::Missing { names }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(site_url: Option<&str>, email: Option<&str>) -> JiraIntegrationSettings {
        JiraIntegrationSettings {
            enabled:  true,
            site_url: site_url.map(str::to_string),
            email:    email.map(str::to_string),
        }
    }

    #[test]
    fn resolve_jira_credentials_reports_each_missing_input_sorted() {
        let resolution = resolve_jira_credentials(&settings(None, None), |_| None);

        assert_eq!(resolution, JiraCredentialResolution::Missing {
            names: vec![
                "FABRO_JIRA_API_TOKEN".to_string(),
                "server.integrations.jira.email".to_string(),
                "server.integrations.jira.site_url".to_string(),
            ],
        });
    }

    #[test]
    fn resolve_jira_credentials_configured_when_all_present() {
        let resolution = resolve_jira_credentials(
            &settings(Some("https://acme.atlassian.net"), Some("bot@acme.com")),
            |name| (name == "FABRO_JIRA_API_TOKEN").then(|| "tok".to_string()),
        );

        assert_eq!(resolution, JiraCredentialResolution::Configured {
            site_url:  "https://acme.atlassian.net".to_string(),
            email:     "bot@acme.com".to_string(),
            api_token: "tok".to_string(),
        });
    }

    #[test]
    fn record_error_sanitizes_and_clear_error_resets() {
        let service = JiraService::new(
            JiraClient::new(
                "https://acme.atlassian.net".to_string(),
                "bot@acme.com".to_string(),
                "tok".to_string(),
            )
            .unwrap(),
        );
        service.record_error("boom\nline");
        assert_eq!(service.last_error().as_deref(), Some("boom line"));
        service.clear_error();
        assert_eq!(service.last_error(), None);
    }
}
