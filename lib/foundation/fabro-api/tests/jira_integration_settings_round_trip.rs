use std::any::{TypeId, type_name};

use fabro_api::types::JiraIntegrationSettings as ApiJiraIntegrationSettings;
use fabro_types::settings::server::JiraIntegrationSettings;
use serde_json::json;

#[test]
fn jira_integration_settings_reuses_domain_type() {
    assert_same_type::<ApiJiraIntegrationSettings, JiraIntegrationSettings>();
}

#[test]
fn jira_integration_settings_round_trips_representative_json() {
    let value = json!({
        "enabled": true,
        "site_url": "https://acme.atlassian.net",
        "email": "bot@acme.com"
    });

    let settings: ApiJiraIntegrationSettings = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(settings).unwrap(), value);
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
