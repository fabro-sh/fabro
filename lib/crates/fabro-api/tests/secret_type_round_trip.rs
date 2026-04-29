use std::any::{TypeId, type_name};

use fabro_api::types::SecretType as ApiSecretType;
use fabro_types::SecretType;
use serde_json::json;

#[test]
fn secret_type_reuses_canonical_type() {
    assert_same_type::<ApiSecretType, SecretType>();
}

#[test]
fn secret_type_serializes_as_snake_case_strings() {
    assert_eq!(
        serde_json::to_value(SecretType::Environment).unwrap(),
        json!("environment")
    );
    assert_eq!(
        serde_json::to_value(SecretType::File).unwrap(),
        json!("file")
    );
    assert_eq!(
        serde_json::to_value(SecretType::Credential).unwrap(),
        json!("credential")
    );
}

#[test]
fn secret_type_deserializes_each_variant() {
    let env: SecretType = serde_json::from_value(json!("environment")).unwrap();
    assert_eq!(env, SecretType::Environment);
    let file: SecretType = serde_json::from_value(json!("file")).unwrap();
    assert_eq!(file, SecretType::File);
    let cred: SecretType = serde_json::from_value(json!("credential")).unwrap();
    assert_eq!(cred, SecretType::Credential);
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
