//! Conformance tests: spec ↔ router ↔ Rust struct consistency.

#![allow(
    clippy::absolute_paths,
    clippy::default_trait_access,
    clippy::manual_assert,
    clippy::manual_let_else
)]

use super::helpers::test_app_state;
use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use fabro_config::run::*;
use fabro_config::sandbox::SandboxSettings;
use fabro_hooks::*;
use fabro_sandbox::daytona::*;
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use fabro_server::server_config::*;
use fabro_types::settings::{
    ClientTlsSettings, ExecSettings, OutputFormat, PermissionLevel, ProjectSettings,
    ServerSettings as UserServerSettings,
};
use tower::ServiceExt;

fn load_spec() -> openapiv3::OpenAPI {
    let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/api-reference/fabro-api.yaml");
    let text = std::fs::read_to_string(&spec_path).expect("failed to read spec");
    serde_yaml::from_str(&text).expect("failed to parse spec")
}

fn resolve_path(path: &str) -> String {
    path.replace("{id}", "test-id")
        .replace("{qid}", "test-qid")
        .replace("{stageId}", "test-stage")
        .replace("{name}", "test-name")
        .replace("{slug}", "test-slug")
}

fn methods_for_path_item(item: &openapiv3::PathItem) -> Vec<Method> {
    let mut methods = Vec::new();
    if item.get.is_some() {
        methods.push(Method::GET);
    }
    if item.post.is_some() {
        methods.push(Method::POST);
    }
    if item.put.is_some() {
        methods.push(Method::PUT);
    }
    if item.delete.is_some() {
        methods.push(Method::DELETE);
    }
    if item.patch.is_some() {
        methods.push(Method::PATCH);
    }
    methods
}

#[tokio::test]
async fn all_spec_routes_are_routable() {
    let spec = load_spec();
    let state = test_app_state();
    let app = build_router(state, AuthMode::Disabled);

    let mut checked = 0;
    for (path, item) in &spec.paths.paths {
        let path_item = match item {
            openapiv3::ReferenceOr::Item(item) => item,
            openapiv3::ReferenceOr::Reference { .. } => continue,
        };

        let uri = resolve_path(path);
        for method in methods_for_path_item(path_item) {
            let mut builder = Request::builder().method(&method).uri(&uri);

            let body = if method == Method::POST {
                builder = builder.header("content-type", "application/json");
                Body::from("{}")
            } else {
                Body::empty()
            };

            let req = builder.body(body).unwrap();
            let response = app.clone().oneshot(req).await.unwrap();

            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "Route {method} {path} returned 405 — not registered in the router"
            );
            checked += 1;
        }
    }

    assert!(checked > 0, "No routes were checked — is the spec empty?");
}

// ── ServerConfig ↔ OpenAPI schema drift detection ──────────────────────

/// Load the spec as serde_json::Value for schema introspection.
fn load_spec_json() -> serde_json::Value {
    let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/api-reference/fabro-api.yaml");
    let text = std::fs::read_to_string(&spec_path).expect("read spec");
    serde_yaml::from_str(&text).expect("parse spec")
}

/// Follow a `$ref` pointer, or return the value unchanged.
fn resolve_ref<'a>(
    value: &'a serde_json::Value,
    root: &'a serde_json::Value,
) -> &'a serde_json::Value {
    match value.get("$ref").and_then(|v| v.as_str()) {
        Some(ref_str) => {
            let mut cur = root;
            for seg in ref_str.trim_start_matches("#/").split('/') {
                cur = &cur[seg];
            }
            cur
        }
        None => value,
    }
}

/// Collect property names from an OpenAPI schema object.
fn spec_keys(schema: &serde_json::Value) -> BTreeSet<String> {
    schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Recursively compare serialized JSON keys against OpenAPI schema properties.
fn compare_schema(
    path: &str,
    json: &serde_json::Value,
    schema: &serde_json::Value,
    root: &serde_json::Value,
    errors: &mut Vec<String>,
) {
    let obj = match json.as_object() {
        Some(o) => o,
        None => return,
    };

    // Skip pure-map schemas (additionalProperties without properties).
    if schema.get("additionalProperties").is_some() && schema.get("properties").is_none() {
        return;
    }

    let json_keys: BTreeSet<String> = obj.keys().cloned().collect();
    let schema_keys = spec_keys(schema);

    for key in json_keys.difference(&schema_keys) {
        errors.push(format!(
            "{path}.{key}: in Rust but missing from OpenAPI spec"
        ));
    }
    for key in schema_keys.difference(&json_keys) {
        errors.push(format!(
            "{path}.{key}: in OpenAPI spec but missing from Rust"
        ));
    }

    let properties = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return,
    };

    for key in json_keys.intersection(&schema_keys) {
        let json_val = &obj[key];
        let prop_schema = resolve_ref(&properties[key], root);

        // Skip maps and union types.
        if prop_schema.get("additionalProperties").is_some() || prop_schema.get("oneOf").is_some() {
            continue;
        }

        match json_val {
            serde_json::Value::Object(_) => {
                compare_schema(
                    &format!("{path}.{key}"),
                    json_val,
                    prop_schema,
                    root,
                    errors,
                );
            }
            serde_json::Value::Array(arr) => {
                // Union keys across all array elements.
                let union: BTreeSet<String> = arr
                    .iter()
                    .filter_map(|e| e.as_object())
                    .flat_map(|o| o.keys().cloned())
                    .collect();
                if union.is_empty() {
                    continue;
                }
                let items = match prop_schema.get("items") {
                    Some(i) => resolve_ref(i, root),
                    None => continue,
                };
                let synthetic = serde_json::Value::Object(
                    union
                        .into_iter()
                        .map(|k| (k, serde_json::Value::Null))
                        .collect(),
                );
                compare_schema(&format!("{path}.{key}[]"), &synthetic, items, root, errors);
            }
            _ => {}
        }
    }
}

/// Build a Settings with every Option set to Some so all keys appear
/// in the serialized JSON.
fn fully_populated_server_config() -> Settings {
    Settings {
        version: Some(1),
        goal: Some("default goal".into()),
        goal_file: Some("/tmp/goal.txt".into()),
        graph: Some("workflow.fabro".into()),
        labels: std::collections::HashMap::from([("scope".into(), "server".into())]),
        server: Some(UserServerSettings {
            target: Some("https://server.example.com".into()),
            tls: Some(ClientTlsSettings {
                cert: "client-cert.pem".into(),
                key: "client-key.pem".into(),
                ca: "ca.pem".into(),
            }),
        }),
        exec: Some(ExecSettings {
            provider: Some("openai".into()),
            model: Some("gpt-5.4".into()),
            permissions: Some(PermissionLevel::ReadWrite),
            output_format: Some(OutputFormat::Json),
        }),
        prevent_idle_sleep: Some(true),
        verbose: Some(true),
        upgrade_check: Some(false),
        dry_run: Some(true),
        auto_approve: Some(true),
        no_retro: Some(true),
        storage_dir: Some("/data".into()),
        max_concurrent_runs: Some(10),
        web: Some(WebSettings {
            enabled: true,
            url: "https://example.com".into(),
            auth: AuthSettings {
                provider: AuthProvider::Github,
                allowed_usernames: vec!["user".into()],
            },
        }),
        api: Some(ApiSettings {
            base_url: "https://api.example.com".into(),
            authentication_strategies: vec![ApiAuthStrategy::Jwt],
            tls: Some(TlsSettings {
                cert: "c".into(),
                key: "k".into(),
                ca: "ca".into(),
            }),
        }),
        git: Some(GitSettings {
            provider: GitProvider::Github,
            app_id: Some("123".into()),
            client_id: Some("456".into()),
            slug: Some("fabro".into()),
            author: GitAuthorSettings {
                name: Some("bot".into()),
                email: Some("bot@x".into()),
            },
            webhooks: Some(WebhookSettings {
                strategy: WebhookStrategy::TailscaleFunnel,
            }),
        }),
        features: Some(FeaturesSettings {
            session_sandboxes: true,
            retros: false,
        }),
        log: Some(LogSettings {
            level: Some("debug".into()),
        }),
        work_dir: Some("/work".into()),
        llm: Some(LlmSettings {
            model: Some("m".into()),
            provider: Some("p".into()),
            fallbacks: Some(Default::default()),
        }),
        setup: Some(SetupSettings {
            commands: vec!["echo hi".into()],
            timeout_ms: Some(5000),
        }),
        sandbox: Some(SandboxSettings {
            provider: Some("daytona".into()),
            preserve: Some(true),
            devcontainer: None,
            local: None,
            daytona: Some(DaytonaConfig {
                auto_stop_interval: Some(60),
                labels: Some(Default::default()),
                snapshot: Some(DaytonaSnapshotConfig {
                    name: "snap".into(),
                    cpu: Some(2),
                    memory: Some(4),
                    disk: Some(10),
                    dockerfile: Some(DockerfileSource::Inline("FROM x".into())),
                }),
                network: Some(DaytonaNetwork::Block),
                skip_clone: false,
            }),
            env: Some(Default::default()),
        }),
        vars: Some(Default::default()),
        checkpoint: CheckpointSettings {
            exclude_globs: vec!["**/node_modules/**".into()],
        },
        pull_request: Some(PullRequestSettings {
            enabled: true,
            draft: false,
            auto_merge: false,
            merge_strategy: MergeStrategy::Squash,
        }),
        artifacts: Some(ArtifactsSettings {
            include: vec!["test-results/**".into()],
        }),
        // One hook per HookType variant so the key union covers all fields.
        hooks: vec![
            HookDefinition {
                name: Some("cmd".into()),
                event: HookEvent::RunStart,
                command: Some("echo".into()),
                hook_type: None,
                matcher: Some("*".into()),
                blocking: Some(true),
                timeout_ms: Some(5000),
                sandbox: Some(true),
            },
            HookDefinition {
                name: Some("http".into()),
                event: HookEvent::RunStart,
                command: None,
                hook_type: Some(HookType::Http {
                    url: "http://x".into(),
                    headers: Some(Default::default()),
                    allowed_env_vars: vec!["X".into()],
                    tls: TlsMode::Verify,
                }),
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            },
            HookDefinition {
                name: Some("prompt".into()),
                event: HookEvent::RunStart,
                command: None,
                hook_type: Some(HookType::Prompt {
                    prompt: "hi".into(),
                    model: Some("m".into()),
                }),
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            },
            HookDefinition {
                name: Some("agent".into()),
                event: HookEvent::RunStart,
                command: None,
                hook_type: Some(HookType::Agent {
                    prompt: "hi".into(),
                    model: Some("m".into()),
                    max_tool_rounds: Some(5),
                }),
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            },
        ],
        mcp_servers: std::collections::HashMap::from([(
            "test".into(),
            fabro_config::mcp::McpServerEntry {
                transport: fabro_config::mcp::McpTransport::Stdio {
                    command: vec!["echo".into()],
                    env: Default::default(),
                },
                startup_timeout_secs: fabro_config::mcp::default_startup_timeout_secs(),
                tool_timeout_secs: fabro_config::mcp::default_tool_timeout_secs(),
            },
        )]),
        github: Some(GitHubSettings {
            permissions: std::collections::HashMap::from([("contents".into(), "read".into())]),
        }),
        fabro: Some(ProjectSettings {
            root: "fabro".into(),
        }),
        ..Default::default()
    }
}

#[test]
fn server_settings_keys_match_openapi_spec() {
    let settings = fully_populated_server_config();
    let json = serde_json::to_value(&settings).expect("serialize ServerSettings");
    let spec = load_spec_json();
    let schema = &spec["components"]["schemas"]["ServerSettings"];

    let mut errors = Vec::new();
    compare_schema("ServerSettings", &json, schema, &spec, &mut errors);

    if !errors.is_empty() {
        panic!(
            "ServerSettings ↔ OpenAPI schema drift:\n  {}",
            errors.join("\n  ")
        );
    }
}
