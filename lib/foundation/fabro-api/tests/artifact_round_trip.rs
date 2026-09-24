use std::any::TypeId;

use fabro_api::types;
use fabro_types::{ArtifactSource, BlobHash, RunArtifact, StageId};
use serde_json::{Value, json};

fn validator(name: &str) -> jsonschema::Validator {
    let yaml: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../docs/public/api-reference/fabro-api.yaml"
    ))
    .unwrap();
    let mut spec = serde_json::to_value(yaml).unwrap();
    spec["$ref"] = json!(format!("#/components/schemas/{name}"));
    jsonschema::validator_for(&spec).unwrap()
}

#[test]
fn artifact_api_types_reuse_the_canonical_types_and_wire_format() {
    assert_eq!(
        TypeId::of::<types::ArtifactSource>(),
        TypeId::of::<ArtifactSource>()
    );
    assert_eq!(
        TypeId::of::<types::RunArtifact>(),
        TypeId::of::<RunArtifact>()
    );
    let hash = BlobHash::new(b"payload");
    for source in [
        ArtifactSource::SqliteBlob(hash),
        ArtifactSource::ObjectStore(hash),
    ] {
        let source_json = serde_json::to_value(source).unwrap();
        assert!(validator("ArtifactSource").is_valid(&source_json));
        assert_eq!(
            serde_json::from_value::<types::ArtifactSource>(source_json).unwrap(),
            source
        );
        let artifact = RunArtifact {
            stage_id: StageId::new("write", 1),
            retry: 1,
            relative_path: "assets/report.bin".to_string(),
            size: 7,
            source,
        };
        let json = serde_json::to_value(&artifact).unwrap();
        assert!(validator("RunArtifact").is_valid(&json));
        assert_eq!(
            serde_json::from_value::<types::RunArtifact>(json).unwrap(),
            artifact
        );
    }
}

#[test]
fn artifact_schema_and_rust_reject_the_same_invalid_sources() {
    let schema = validator("ArtifactSource");
    let hash = BlobHash::new(b"payload");
    for json in [
        json!({}),
        json!({"blob": hash, "object": hash}),
        json!({"blob": hash, "object": Value::Null}),
        json!({"object":"invalid"}),
    ] {
        assert!(!schema.is_valid(&json), "{json}");
        assert!(serde_json::from_value::<types::ArtifactSource>(json).is_err());
    }
}
