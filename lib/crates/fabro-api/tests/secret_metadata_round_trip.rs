use std::any::{TypeId, type_name};

use fabro_api::types::SecretMetadata as ApiSecretMetadata;
use fabro_types::{SecretMetadata, SecretType};
use serde_json::json;

#[test]
fn secret_metadata_reuses_canonical_type() {
    assert_same_type::<ApiSecretMetadata, SecretMetadata>();
}

#[test]
fn secret_metadata_round_trips_representative_json() {
    let value = json!({
        "name": "ANTHROPIC_API_KEY",
        "type": "token",
        "description": "Anthropic API key",
        "created_at": "2026-04-29T12:34:56Z",
        "updated_at": "2026-04-29T12:40:00Z"
    });

    let metadata: SecretMetadata = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(metadata.name, "ANTHROPIC_API_KEY");
    assert_eq!(metadata.secret_type, SecretType::Token);
    assert_eq!(metadata.description, Some("Anthropic API key".to_string()));
    assert_eq!(serde_json::to_value(metadata).unwrap(), value);
}

#[test]
fn secret_metadata_omits_absent_description() {
    let value = json!({
        "name": "/run/secrets/key.pem",
        "type": "file",
        "created_at": "2026-04-29T12:34:56Z",
        "updated_at": "2026-04-29T12:40:00Z"
    });

    let metadata: SecretMetadata = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(metadata).unwrap(), value);
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
