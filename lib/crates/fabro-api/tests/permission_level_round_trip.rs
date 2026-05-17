use std::any::{TypeId, type_name};

use fabro_api::types::PermissionLevel as ApiPermissionLevel;
use fabro_types::PermissionLevel;
use serde_json::json;

#[test]
fn permission_level_reuses_canonical_type() {
    assert_same_type::<ApiPermissionLevel, PermissionLevel>();
}

#[test]
fn permission_level_serializes_as_kebab_case_strings() {
    assert_eq!(
        serde_json::to_value(PermissionLevel::ReadOnly).unwrap(),
        json!("read-only")
    );
    assert_eq!(
        serde_json::to_value(PermissionLevel::ReadWrite).unwrap(),
        json!("read-write")
    );
    assert_eq!(
        serde_json::to_value(PermissionLevel::Full).unwrap(),
        json!("full")
    );
}

#[test]
fn permission_level_deserializes_each_variant() {
    let read_only: PermissionLevel = serde_json::from_value(json!("read-only")).unwrap();
    assert_eq!(read_only, PermissionLevel::ReadOnly);
    let read_write: PermissionLevel = serde_json::from_value(json!("read-write")).unwrap();
    assert_eq!(read_write, PermissionLevel::ReadWrite);
    let full: PermissionLevel = serde_json::from_value(json!("full")).unwrap();
    assert_eq!(full, PermissionLevel::Full);
}

#[test]
fn permission_level_rejects_unknown_values() {
    assert!(serde_json::from_value::<PermissionLevel>(json!("readonly")).is_err());
    assert!(serde_json::from_value::<PermissionLevel>(json!("read_only")).is_err());
    assert!(serde_json::from_value::<PermissionLevel>(json!("")).is_err());
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
