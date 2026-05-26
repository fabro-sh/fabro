use std::any::{TypeId, type_name};
use std::collections::BTreeMap;

use fabro_api::types::{
    IntegrationConnectionKind as ApiIntegrationConnectionKind,
    IntegrationConnectionState as ApiIntegrationConnectionState,
    IntegrationConnectionStatus as ApiIntegrationConnectionStatus,
    IntegrationProvider as ApiIntegrationProvider, IntegrationStatus as ApiIntegrationStatus,
    SystemIntegrationStatus as ApiSystemIntegrationStatus,
    SystemIntegrationsResponse as ApiSystemIntegrationsResponse,
};
use fabro_types::{
    IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
    IntegrationProvider, IntegrationStatus, SystemIntegrationStatus, SystemIntegrationsResponse,
};
use serde_json::json;

#[test]
fn system_integrations_family_reuses_domain_types() {
    assert_same_type::<ApiSystemIntegrationsResponse, SystemIntegrationsResponse>();
    assert_same_type::<ApiSystemIntegrationStatus, SystemIntegrationStatus>();
    assert_same_type::<ApiIntegrationProvider, IntegrationProvider>();
    assert_same_type::<ApiIntegrationStatus, IntegrationStatus>();
    assert_same_type::<ApiIntegrationConnectionStatus, IntegrationConnectionStatus>();
    assert_same_type::<ApiIntegrationConnectionKind, IntegrationConnectionKind>();
    assert_same_type::<ApiIntegrationConnectionState, IntegrationConnectionState>();
}

#[test]
fn system_integrations_round_trips_representative_json() {
    let value = json!({
        "data": [
            {
                "provider": "slack",
                "enabled": true,
                "configured": true,
                "status": "connected",
                "missing_credentials": [],
                "connection": {
                    "kind": "socket_mode",
                    "status": "connected",
                    "last_connected_at": "2026-05-26T04:00:00Z",
                    "last_error": null
                },
                "metadata": {
                    "default_channel": "#feed-fabro"
                }
            },
            {
                "provider": "github",
                "enabled": true,
                "configured": false,
                "status": "missing_credentials",
                "missing_credentials": ["GITHUB_TOKEN"],
                "connection": null,
                "metadata": {
                    "strategy": "token"
                }
            }
        ]
    });

    let response: SystemIntegrationsResponse = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(response.data.len(), 2);
    assert_eq!(response.data[0].provider, IntegrationProvider::Slack);
    assert_eq!(response.data[0].status, IntegrationStatus::Connected);
    assert_eq!(
        response.data[0].connection.as_ref().unwrap().kind,
        IntegrationConnectionKind::SocketMode
    );
    assert_eq!(
        response.data[0].metadata,
        BTreeMap::from([("default_channel".to_string(), "#feed-fabro".to_string())])
    );
    assert_eq!(serde_json::to_value(response).unwrap(), value);
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
