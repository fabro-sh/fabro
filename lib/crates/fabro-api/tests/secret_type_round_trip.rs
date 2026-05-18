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
        serde_json::to_value(SecretType::Token).unwrap(),
        json!("token")
    );
    assert_eq!(
        serde_json::to_value(SecretType::Oauth).unwrap(),
        json!("oauth")
    );
    assert_eq!(
        serde_json::to_value(SecretType::File).unwrap(),
        json!("file")
    );
}

#[test]
fn secret_type_deserializes_each_variant() {
    let token: SecretType = serde_json::from_value(json!("token")).unwrap();
    assert_eq!(token, SecretType::Token);
    let oauth: SecretType = serde_json::from_value(json!("oauth")).unwrap();
    assert_eq!(oauth, SecretType::Oauth);
    let file: SecretType = serde_json::from_value(json!("file")).unwrap();
    assert_eq!(file, SecretType::File);
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
