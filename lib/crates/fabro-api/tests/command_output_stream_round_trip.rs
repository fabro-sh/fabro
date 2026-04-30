use std::any::{TypeId, type_name};

use fabro_api::types::CommandOutputStream as ApiCommandOutputStream;
use fabro_types::CommandOutputStream;
use serde_json::json;

#[test]
fn command_output_stream_reuses_canonical_type() {
    assert_same_type::<ApiCommandOutputStream, CommandOutputStream>();
}

#[test]
fn command_output_stream_serializes_as_stream_names() {
    assert_eq!(
        serde_json::to_value(CommandOutputStream::Stdout).unwrap(),
        json!("stdout")
    );
    assert_eq!(
        serde_json::to_value(CommandOutputStream::Stderr).unwrap(),
        json!("stderr")
    );
}

#[test]
fn command_output_stream_deserializes_representative_values() {
    assert_eq!(
        serde_json::from_value::<ApiCommandOutputStream>(json!("stdout")).unwrap(),
        CommandOutputStream::Stdout
    );
    assert_eq!(
        serde_json::from_value::<ApiCommandOutputStream>(json!("stderr")).unwrap(),
        CommandOutputStream::Stderr
    );
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
