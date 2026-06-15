use std::any::{TypeId, type_name};

use fabro_api::types::{
    GitlabIntegrationSettings as ApiGitlabIntegrationSettings,
    GitlabIntegrationStrategy as ApiGitlabIntegrationStrategy, LogDestination as ApiLogDestination,
    ObjectStoreSettings as ApiObjectStoreSettings,
    ServerAuthGitlabSettings as ApiServerAuthGitlabSettings, ServerNamespace as ApiServerNamespace,
    ServerSandboxProviderSettings as ApiServerSandboxProviderSettings,
    ServerSandboxProvidersSettings as ApiServerSandboxProvidersSettings,
    ServerSandboxSettings as ApiServerSandboxSettings, ServerSettings as ApiServerSettings,
};
use fabro_config::ServerSettingsBuilder;
use fabro_types::ServerSettings;
use fabro_types::settings::ServerNamespace;
use fabro_types::settings::server::{
    GitlabIntegrationSettings, GitlabIntegrationStrategy, LogDestination, ObjectStoreSettings,
    ServerAuthGitlabSettings, ServerSandboxProviderSettings, ServerSandboxProvidersSettings,
    ServerSandboxSettings,
};

#[test]
fn server_settings_family_reuses_domain_types() {
    assert_same_type::<ApiServerSettings, ServerSettings>();
    assert_same_type::<ApiServerNamespace, ServerNamespace>();
    assert_same_type::<ApiObjectStoreSettings, ObjectStoreSettings>();
    assert_same_type::<ApiLogDestination, LogDestination>();
    assert_same_type::<ApiServerAuthGitlabSettings, ServerAuthGitlabSettings>();
    assert_same_type::<ApiGitlabIntegrationSettings, GitlabIntegrationSettings>();
    assert_same_type::<ApiGitlabIntegrationStrategy, GitlabIntegrationStrategy>();
    assert_same_type::<ApiServerSandboxSettings, ServerSandboxSettings>();
    assert_same_type::<ApiServerSandboxProvidersSettings, ServerSandboxProvidersSettings>();
    assert_same_type::<ApiServerSandboxProviderSettings, ServerSandboxProviderSettings>();
}

#[test]
fn server_settings_json_matches_openapi_shape() {
    let settings = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.api]
url = "https://api.fabro.example.com"

[server.web]
enabled = true
url = "https://fabro.example.com"

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.auth.gitlab]
allowed_usernames = ["bob"]
allowed_groups = ["platform/fabro-admins"]

[server.sandbox.providers.daytona]
enabled = false

[server.storage]
root = "/srv/fabro"

[server.logging]
destination = "stdout"

[server.integrations.github]
enabled = true
strategy = "app"
app_id = "12345"
client_id = "Iv1.abcdef"
slug = "fabro-dev"

[server.integrations.github.webhooks]
strategy = "tailscale_funnel"

[server.integrations.gitlab]
enabled = true
strategy = "app"
base_url = "https://gitlab.ipt.example/gitlab"
client_id = "gitlab-client"
"#,
    )
    .expect("settings should resolve");

    let json = serde_json::to_value(&settings).expect("server settings should serialize");
    assert_eq!(json["server"]["listen"]["type"], "tcp");
    assert_eq!(json["server"]["listen"]["address"], "127.0.0.1:32276");
    assert_eq!(json["server"]["storage"]["root"], "/srv/fabro");
    assert_eq!(json["server"]["logging"]["destination"], "stdout");
    assert_eq!(
        json["server"]["auth"]["gitlab"]["allowed_usernames"],
        serde_json::json!(["bob"])
    );
    assert_eq!(
        json["server"]["auth"]["gitlab"]["allowed_groups"],
        serde_json::json!(["platform/fabro-admins"])
    );
    assert_eq!(json["server"]["integrations"]["gitlab"]["enabled"], true);
    assert_eq!(json["server"]["integrations"]["gitlab"]["strategy"], "app");
    assert_eq!(
        json["server"]["integrations"]["gitlab"]["base_url"],
        "https://gitlab.ipt.example/gitlab"
    );
    assert_eq!(
        json["server"]["integrations"]["gitlab"]["client_id"],
        "gitlab-client"
    );
    assert_eq!(
        json["server"]["sandbox"]["providers"]["local"]["enabled"],
        true
    );
    assert_eq!(
        json["server"]["sandbox"]["providers"]["docker"]["enabled"],
        true
    );
    assert_eq!(
        json["server"]["sandbox"]["providers"]["daytona"]["enabled"],
        false
    );
    assert!(
        json["server"].get("ip_allowlist").is_none(),
        "server settings API should not expose removed IP allowlist settings"
    );
    assert!(
        json["server"]["integrations"]["github"]["webhooks"]
            .get("ip_allowlist")
            .is_none(),
        "github webhook settings API should not expose removed IP allowlist settings"
    );
    assert!(json.get("features").is_none());

    let round_trip: ApiServerSettings =
        serde_json::from_value(json).expect("server settings should deserialize");
    assert_eq!(round_trip, settings);
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
