#![expect(
    clippy::disallowed_methods,
    reason = "build script: runs at compile time outside any runtime"
)]

use std::path::{Path, PathBuf};
use std::{env, fs};

use progenitor::{GenerationSettings, Generator, InterfaceStyle, TypeImpl};

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
                            if inner.get("$ref").is_some() {
                                inner = serde_json::json!({
                                    "allOf": [inner],
                                    "nullable": true,
                                });
                            } else {
                                inner
                                    .as_object_mut()
                                    .expect("oneOf collapse should leave an object schema")
                                    .insert("nullable".to_string(), serde_json::Value::Bool(true));
                            }
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

/// Progenitor currently panics when an operation advertises a typed success
/// response alongside a no-content success response. Keep the source spec
/// accurate for docs and other generators, but preserve the existing Rust
/// client shape for `DELETE /runs/{id}`: success with no body.
fn patch_codegen_delete_run_responses(value: &mut serde_json::Value) {
    let Some(responses) = value
        .get_mut("paths")
        .and_then(|paths| paths.get_mut("/api/v1/runs/{id}"))
        .and_then(|path| path.get_mut("delete"))
        .and_then(|operation| operation.get_mut("responses"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    responses.remove("200");
}

fn spec_path_from_manifest_dir(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .nth(3)
        .expect("fabro-api manifest dir should be nested under <repo>/lib/crates/fabro-api")
        .join("docs/public/api-reference/fabro-api.yaml")
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
    patch_codegen_delete_run_responses(&mut spec_value);

    let spec: openapiv3::OpenAPI =
        serde_json::from_value(spec_value).expect("failed to deserialize OpenAPI spec");

    let mut settings = GenerationSettings::default();
    settings.with_interface(InterfaceStyle::Builder);
    let replacements: &[(&str, &str, &[TypeImpl])] = &[
        ("RunStatus", "fabro_types::status::RunStatus", &[]),
        ("PendingReason", "fabro_types::status::PendingReason", &[]),
        ("SuccessReason", "fabro_types::status::SuccessReason", &[]),
        ("FailureReason", "fabro_types::status::FailureReason", &[]),
        ("FailureCategory", "fabro_types::FailureCategory", &[]),
        ("FailureSignature", "fabro_types::FailureSignature", &[]),
        ("FailureDetail", "fabro_types::FailureDetail", &[]),
        ("RunFailure", "fabro_types::RunFailure", &[]),
        ("BlockedReason", "fabro_types::status::BlockedReason", &[]),
        (
            "RunControlAction",
            "fabro_types::status::RunControlAction",
            &[],
        ),
        ("Run", "fabro_types::Run", &[]),
        ("RunApproval", "fabro_types::RunApproval", &[]),
        ("RunApprovalState", "fabro_types::RunApprovalState", &[]),
        ("RunRunnableSource", "fabro_types::RunRunnableSource", &[]),
        ("RunSize", "fabro_types::RunSize", &[]),
        ("DiffSummary", "fabro_types::DiffSummary", &[]),
        ("RepositoryRef", "fabro_types::RepositoryRef", &[]),
        ("WorkflowSettings", "fabro_types::WorkflowSettings", &[]),
        ("ServerSettings", "fabro_types::ServerSettings", &[]),
        (
            "ServerNamespace",
            "fabro_types::settings::ServerNamespace",
            &[],
        ),
        (
            "ServerListenSettings",
            "fabro_types::settings::server::ServerListenSettings",
            &[],
        ),
        (
            "ServerApiSettings",
            "fabro_types::settings::server::ServerApiSettings",
            &[],
        ),
        (
            "ServerWebSettings",
            "fabro_types::settings::server::ServerWebSettings",
            &[],
        ),
        (
            "ServerAuthSettings",
            "fabro_types::settings::server::ServerAuthSettings",
            &[],
        ),
        (
            "ServerAuthMethod",
            "fabro_types::settings::server::ServerAuthMethod",
            &[],
        ),
        (
            "ServerAuthGithubSettings",
            "fabro_types::settings::server::ServerAuthGithubSettings",
            &[],
        ),
        (
            "ServerIpAllowlistSettings",
            "fabro_types::settings::server::ServerIpAllowlistSettings",
            &[],
        ),
        (
            "ServerIpAllowlistOverrideSettings",
            "fabro_types::settings::server::ServerIpAllowlistOverrideSettings",
            &[],
        ),
        (
            "IpAllowEntry",
            "fabro_types::settings::server::IpAllowEntry",
            &[],
        ),
        (
            "ServerStorageSettings",
            "fabro_types::settings::server::ServerStorageSettings",
            &[],
        ),
        (
            "ServerArtifactsSettings",
            "fabro_types::settings::server::ServerArtifactsSettings",
            &[],
        ),
        (
            "ServerSlateDbSettings",
            "fabro_types::settings::server::ServerSlateDbSettings",
            &[],
        ),
        (
            "ObjectStoreSettings",
            "fabro_types::settings::server::ObjectStoreSettings",
            &[],
        ),
        (
            "ServerSchedulerSettings",
            "fabro_types::settings::server::ServerSchedulerSettings",
            &[],
        ),
        (
            "ServerLoggingSettings",
            "fabro_types::settings::server::ServerLoggingSettings",
            &[],
        ),
        (
            "LogDestination",
            "fabro_types::settings::server::LogDestination",
            &[],
        ),
        (
            "ServerIntegrationsSettings",
            "fabro_types::settings::server::ServerIntegrationsSettings",
            &[],
        ),
        (
            "GithubIntegrationSettings",
            "fabro_types::settings::server::GithubIntegrationSettings",
            &[],
        ),
        (
            "GithubIntegrationStrategy",
            "fabro_types::settings::server::GithubIntegrationStrategy",
            &[],
        ),
        (
            "SlackIntegrationSettings",
            "fabro_types::settings::server::SlackIntegrationSettings",
            &[],
        ),
        (
            "IntegrationWebhooksSettings",
            "fabro_types::settings::server::IntegrationWebhooksSettings",
            &[],
        ),
        (
            "WebhookStrategy",
            "fabro_types::settings::server::WebhookStrategy",
            &[],
        ),
        ("AuthMethod", "fabro_types::AuthMethod", &[]),
        ("IdpIdentity", "fabro_types::IdpIdentity", &[]),
        (
            "RunClientProvenance",
            "fabro_types::RunClientProvenance",
            &[],
        ),
        ("RunProvenance", "fabro_types::RunProvenance", &[]),
        (
            "RunServerProvenance",
            "fabro_types::RunServerProvenance",
            &[],
        ),
        ("Principal", "fabro_types::Principal", &[]),
        ("PrincipalUser", "fabro_types::UserPrincipal", &[]),
        ("SystemActorKind", "fabro_types::SystemActorKind", &[]),
        ("QuestionType", "fabro_types::QuestionType", &[]),
        ("StageCompletion", "fabro_types::StageCompletion", &[]),
        ("Conclusion", "fabro_types::Conclusion", &[]),
        ("StageOutcome", "fabro_types::StageOutcome", &[]),
        ("StageHandler", "fabro_types::StageHandler", &[]),
        ("StageState", "fabro_types::StageState", &[]),
        ("CommandTermination", "fabro_types::CommandTermination", &[]),
        ("StageModelUsage", "fabro_types::StageModelUsage", &[]),
        ("StageProjection", "fabro_types::StageProjection", &[]),
        ("PermissionLevel", "fabro_types::PermissionLevel", &[]),
        (
            "AgentSessionActivatedProps",
            "fabro_types::run_event::AgentSessionActivatedProps",
            &[],
        ),
        ("TodoListProjection", "fabro_types::TodoListProjection", &[]),
        ("SubAgentProjection", "fabro_types::SubAgentProjection", &[]),
        ("SubAgentStatus", "fabro_types::SubAgentStatus", &[]),
        ("SkillsProjection", "fabro_types::SkillsProjection", &[]),
        ("ActivatedSkill", "fabro_types::ActivatedSkill", &[]),
        ("AgentSkillSummary", "fabro_types::AgentSkillSummary", &[]),
        (
            "AgentSkillActivationSource",
            "fabro_types::AgentSkillActivationSource",
            &[],
        ),
        (
            "McpServerProjection",
            "fabro_types::McpServerProjection",
            &[],
        ),
        ("McpServerStatus", "fabro_types::McpServerStatus", &[]),
        (
            "AgentMcpToolSummary",
            "fabro_types::AgentMcpToolSummary",
            &[],
        ),
        ("SecretMetadata", "fabro_types::SecretMetadata", &[]),
        ("InterviewOption", "fabro_types::InterviewOption", &[]),
        (
            "InterviewQuestionRecord",
            "fabro_types::InterviewQuestionRecord",
            &[],
        ),
        (
            "PendingInterviewRecord",
            "fabro_types::PendingInterviewRecord",
            &[],
        ),
        ("BilledTokenCounts", "fabro_types::BilledTokenCounts", &[]),
        ("BillingModelRef", "fabro_model::ModelRef", &[]),
        ("BillingSpeed", "fabro_model::Speed", &[]),
        ("ExecOutputTail", "fabro_types::ExecOutputTail", &[]),
        ("StageTiming", "fabro_types::StageTiming", &[]),
        ("RunTiming", "fabro_types::RunTiming", &[]),
        ("ProviderId", "fabro_model::ProviderId", &[]),
        ("Model", "fabro_model::Model", &[]),
        ("Provider", "fabro_model::Provider", &[]),
        ("ModelLimits", "fabro_model::ModelLimits", &[]),
        (
            "ReasoningEffortFeature",
            "fabro_model::ReasoningEffortFeature",
            &[],
        ),
        ("ReasoningEffort", "fabro_model::ReasoningEffort", &[]),
        ("ModelFeatures", "fabro_model::ModelFeatures", &[]),
        ("ModelCosts", "fabro_model::ModelCosts", &[]),
        ("ModelTestMode", "fabro_model::ModelTestMode", &[]),
        ("RunProjection", "fabro_types::RunProjection", &[]),
        ("RunEvent", "fabro_types::RunEvent", &[]),
        ("PairId", "fabro_types::PairId", &[]),
        ("PairMessageId", "fabro_types::PairMessageId", &[]),
        ("PairStatus", "fabro_types::PairStatus", &[]),
        ("PairTarget", "fabro_types::PairTarget", &[]),
        ("PairRecord", "fabro_types::PairRecord", &[]),
        (
            "RunPairStatusResponse",
            "fabro_types::RunPairStatusResponse",
            &[],
        ),
        ("PairStartRequest", "fabro_types::PairStartRequest", &[]),
        ("PairMessageRequest", "fabro_types::PairMessageRequest", &[]),
        ("PairMessageRecord", "fabro_types::PairMessageRecord", &[]),
        (
            "PairTranscriptResponse",
            "fabro_types::PairTranscriptResponse",
            &[],
        ),
        (
            "PairTranscriptEntry",
            "fabro_types::PairTranscriptEntry",
            &[],
        ),
        (
            "RunEventDetailResponse",
            "fabro_types::RunEventDetailResponse",
            &[],
        ),
        ("EventEnvelope", "fabro_types::EventEnvelope", &[]),
        ("PullRequest", "fabro_types::PullRequest", &[]),
        ("PullRequestLink", "fabro_types::PullRequestLink", &[]),
        ("PullRequestDetails", "fabro_types::PullRequestDetails", &[]),
        ("PullRequestMeta", "fabro_types::PullRequestMeta", &[]),
        (
            "PullRequestResponse",
            "fabro_types::PullRequestResponse",
            &[],
        ),
        (
            "PullRequestDetailsStatus",
            "fabro_types::PullRequestDetailsStatus",
            &[],
        ),
        (
            "PullRequestDetailsUnavailableReason",
            "fabro_types::PullRequestDetailsUnavailableReason",
            &[],
        ),
        ("RunSandboxRuntime", "fabro_types::RunSandboxRuntime", &[]),
        ("PullRequestUser", "fabro_types::PullRequestUser", &[]),
        ("PullRequestRef", "fabro_types::PullRequestRef", &[]),
        (
            "MergeMethod",
            "fabro_types::settings::run::MergeStrategy",
            &[],
        ),
        ("SecretType", "fabro_types::SecretType", &[]),
        ("DiffStats", "fabro_types::DiffStats", &[]),
        ("PreRunPushOutcome", "fabro_types::PreRunPushOutcome", &[]),
        ("DirtyStatus", "fabro_types::DirtyStatus", &[]),
        ("GitContext", "fabro_types::GitContext", &[]),
        ("SandboxProvider", "fabro_types::SandboxProvider", &[]),
        ("RunSandbox", "fabro_types::RunSandbox", &[]),
        ("SandboxDetails", "fabro_types::SandboxDetails", &[]),
        ("SandboxNetwork", "fabro_types::SandboxNetwork", &[]),
        (
            "SandboxNetworkPolicy",
            "fabro_types::SandboxNetworkPolicy",
            &[],
        ),
        (
            "SandboxNetworkPolicyMode",
            "fabro_types::SandboxNetworkPolicyMode",
            &[],
        ),
        ("SandboxService", "fabro_types::SandboxService", &[]),
        (
            "SandboxServiceDiscoverySource",
            "fabro_types::SandboxServiceDiscoverySource",
            &[],
        ),
        (
            "SandboxServiceListMeta",
            "fabro_types::SandboxServiceListMeta",
            &[],
        ),
        (
            "SandboxServiceListResponse",
            "fabro_types::SandboxServiceListResponse",
            &[],
        ),
        ("SandboxState", "fabro_types::SandboxState", &[]),
        ("SandboxResources", "fabro_types::SandboxResources", &[]),
        ("SandboxTimestamps", "fabro_types::SandboxTimestamps", &[]),
        ("AskFabro", "fabro_types::AskFabro", &[]),
        ("SessionId", "fabro_types::SessionId", &[]),
        ("TurnId", "fabro_types::TurnId", &[]),
        ("SessionStatus", "fabro_types::SessionStatus", &[]),
        ("SessionTurn", "fabro_types::SessionTurn", &[]),
        ("SessionMessage", "fabro_types::SessionMessage", &[]),
        ("SessionRecord", "fabro_types::SessionRecord", &[]),
        ("SessionSummary", "fabro_types::SessionSummary", &[]),
        ("SessionDetail", "fabro_types::SessionDetail", &[]),
    ];
    for (name, path, impls) in replacements {
        settings.with_replacement(*name, *path, impls.iter().copied());
    }

    let mut generator = Generator::new(&settings);
    let tokens = generator
        .generate_tokens(&spec)
        .expect("failed to generate tokens from OpenAPI spec");
    let syntax_tree = syn::parse2::<syn::File>(tokens).expect("failed to parse generated tokens");
    let formatted = prettyplease::unparse(&syntax_tree);

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR should be set for build scripts");
    let out_path = Path::new(&out_dir).join("codegen.rs");
    fs::write(&out_path, formatted).expect("failed to write generated code");
}
