use std::{env, fs, path::Path};

use typify::{TypeSpace, TypeSpaceSettings};

fn main() {
    let spec_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/api-reference/fabro-api.yaml");

    println!("cargo::rerun-if-changed={}", spec_path.display());

    let spec_text = fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", spec_path.display()));
    let spec: serde_json::Value =
        serde_yaml::from_str(&spec_text).unwrap_or_else(|e| panic!("failed to parse YAML: {e}"));

    let schemas = spec["components"]["schemas"]
        .as_object()
        .expect("no components/schemas in spec");

    let named_schemas: Vec<(String, schemars::schema::Schema)> = schemas
        .iter()
        .map(|(name, value)| {
            let schema: schemars::schema::Schema = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("failed to parse schema {name}: {e}"));
            (name.clone(), schema)
        })
        .collect();

    let settings = TypeSpaceSettings::default();
    let mut type_space = TypeSpace::new(&settings);
    type_space
        .add_ref_types(named_schemas)
        .expect("failed to add schemas to type space");

    let token_stream = type_space.to_stream();
    let syntax_tree =
        syn::parse2::<syn::File>(token_stream).expect("failed to parse generated tokens");
    let formatted = prettyplease::unparse(&syntax_tree);

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("openapi_types.rs");
    fs::write(&out_path, formatted).expect("failed to write generated types");
}
