#![expect(
    clippy::disallowed_methods,
    reason = "build script: runs at compile time outside any runtime"
)]

use std::path::{Path, PathBuf};
use std::{env, fs};

use progenitor::{GenerationSettings, Generator, InterfaceStyle};

/// Recursively convert OpenAPI 3.1 `type: "null"` patterns to 3.0 `nullable:
/// true`.
///
/// Handles two patterns:
/// - `oneOf: [{...}, {type: "null"}]` → the non-null schema with `nullable:
///   true`
/// - `type: [T1, ..., "null"]` → the remaining types with `nullable: true`
fn patch_nullable(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Pattern: oneOf with a {type: "null"} variant
            if let Some(one_of) = map.get_mut("oneOf") {
                if let Some(variants) = one_of.as_array_mut() {
                    let null_idx = variants.iter().position(|v| {
                        v.get("type").and_then(serde_json::Value::as_str) == Some("null")
                    });
                    if let Some(idx) = null_idx {
                        variants.remove(idx);
                        if variants.len() == 1 {
                            // Collapse single-variant oneOf into the schema itself
                            let mut inner = variants.remove(0);
                            inner
                                .as_object_mut()
                                .unwrap()
                                .insert("nullable".to_string(), serde_json::Value::Bool(true));
                            patch_nullable(&mut inner);
                            *value = inner;
                            return;
                        }
                        map.insert("nullable".to_string(), serde_json::Value::Bool(true));
                    }
                }
            }

            // Pattern: type array containing "null"
            let needs_nullable_from_type = map
                .get("type")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("null")));
            if needs_nullable_from_type {
                if let Some(type_val) = map.get_mut("type") {
                    if let Some(arr) = type_val.as_array_mut() {
                        arr.retain(|v| v.as_str() != Some("null"));
                        if arr.len() == 1 {
                            *type_val = arr.remove(0);
                        }
                    }
                }
                map.insert("nullable".to_string(), serde_json::Value::Bool(true));
            }

            for v in map.values_mut() {
                patch_nullable(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                patch_nullable(v);
            }
        }
        _ => {}
    }
}

/// Progenitor currently panics when an operation advertises more than one
/// request-body media type.
///
/// Keep the source OpenAPI spec accurate for docs, but collapse the
/// generated-client view down to a single preferred media type so code
/// generation can proceed.
fn patch_codegen_request_body_media_types(value: &mut serde_json::Value) {
    let Some(paths) = value
        .get_mut("paths")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    for path_item in paths.values_mut() {
        let Some(item) = path_item.as_object_mut() else {
            continue;
        };

        for method in ["get", "put", "post", "delete", "patch"] {
            let Some(operation) = item
                .get_mut(method)
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            let Some(content) = operation
                .get_mut("requestBody")
                .and_then(|request_body| request_body.get_mut("content"))
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            if content.len() <= 1 {
                continue;
            }

            let preferred = content
                .get("application/octet-stream")
                .cloned()
                .map(|value| ("application/octet-stream".to_string(), value))
                .or_else(|| {
                    content
                        .iter()
                        .next()
                        .map(|(key, value)| (key.clone(), value.clone()))
                });
            if let Some((key, value)) = preferred {
                content.clear();
                content.insert(key, value);
            }
        }
    }
}

fn spec_path_from_manifest_dir(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/api-reference/fabro-api.yaml")
}

fn main() {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .expect("CARGO_MANIFEST_DIR should be set for build scripts");
    let spec_path = spec_path_from_manifest_dir(&manifest_dir);

    println!("cargo::rerun-if-changed={}", spec_path.display());

    let spec_text = fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", spec_path.display()));
    let mut spec_value: serde_json::Value =
        serde_yaml::from_str(&spec_text).unwrap_or_else(|e| panic!("failed to parse YAML: {e}"));

    // TODO: Remove 3.1→3.0 patch when progenitor supports OpenAPI 3.1.
    // Progenitor only supports OpenAPI 3.0.x; our spec uses 3.1.0 but doesn't
    // rely on any 3.1-only features that affect codegen.
    spec_value["openapi"] = serde_json::Value::String("3.0.3".to_string());
    patch_nullable(&mut spec_value);
    patch_codegen_request_body_media_types(&mut spec_value);

    let spec: openapiv3::OpenAPI =
        serde_json::from_value(spec_value).expect("failed to deserialize OpenAPI spec");

    let mut settings = GenerationSettings::default();
    settings.with_interface(InterfaceStyle::Builder);

    let mut generator = Generator::new(&settings);
    let tokens = generator
        .generate_tokens(&spec)
        .expect("failed to generate tokens from OpenAPI spec");
    let syntax_tree = syn::parse2::<syn::File>(tokens).expect("failed to parse generated tokens");
    let formatted = prettyplease::unparse(&syntax_tree);

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("codegen.rs");
    fs::write(&out_path, formatted).expect("failed to write generated code");
}
