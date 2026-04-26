use std::any::{TypeId, type_name};

use fabro_api::types::{
    FeaturesNamespace as ApiFeaturesNamespace, LogDestination as ApiLogDestination,
    ObjectStoreSettings as ApiObjectStoreSettings, ServerNamespace as ApiServerNamespace,
    ServerSettings as ApiServerSettings,
};
use fabro_config::ServerSettingsBuilder;
use fabro_types::ServerSettings;
use fabro_types::settings::server::{LogDestination, ObjectStoreSettings};
use fabro_types::settings::{FeaturesNamespace, ServerNamespace};

#[test]
fn server_settings_family_reuses_domain_types() {
    assert_same_type::<ApiServerSettings, ServerSettings>();
    assert_same_type::<ApiServerNamespace, ServerNamespace>();
    assert_same_type::<ApiFeaturesNamespace, FeaturesNamespace>();
    assert_same_type::<ApiObjectStoreSettings, ObjectStoreSettings>();
    assert_same_type::<ApiLogDestination, LogDestination>();
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

[features]
session_sandboxes = true
"#,
    )
    .expect("settings should resolve");

    let json = serde_json::to_value(&settings).expect("server settings should serialize");
    assert_eq!(json["server"]["listen"]["type"], "tcp");
    assert_eq!(json["server"]["listen"]["address"], "127.0.0.1:32276");
    assert_eq!(json["server"]["storage"]["root"], "/srv/fabro");
    assert_eq!(json["server"]["logging"]["destination"], "stdout");
    assert_eq!(json["features"]["session_sandboxes"], true);

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
