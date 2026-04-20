#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::path::PathBuf;

fn read_doc(relative_path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../")
        .join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn active_server_docs_describe_the_unix_socket_default() {
    let architecture = read_doc("docs/reference/architecture.mdx");
    assert!(
        architecture.contains("~/.fabro/fabro.sock"),
        "architecture doc should mention the default Unix socket bind"
    );

    let api_overview = read_doc("docs/api-reference/overview.mdx");
    assert!(
        api_overview.contains("~/.fabro/fabro.sock"),
        "API overview should mention the default Unix socket bind"
    );
}

#[test]
fn security_doc_does_not_require_jwt_keys_for_the_current_web_flow() {
    let security = read_doc("docs/administration/security.mdx");
    assert!(
        security.contains("SESSION_SECRET"),
        "security doc should still mention the session secret"
    );
    assert!(
        !security.contains("`FABRO_JWT_PRIVATE_KEY`, `FABRO_JWT_PUBLIC_KEY`, and `SESSION_SECRET`"),
        "security doc should not describe JWT keys as required for the current web flow"
    );
}

#[test]
fn deploy_server_doc_links_to_the_cli_target_section_slug() {
    let deploy_server = read_doc("docs/administration/deploy-server.mdx");
    assert!(
        deploy_server.contains("/reference/user-configuration#cli-target-section"),
        "deploy-server doc should link to the Mintlify slug for the [cli.target] section"
    );
}

#[test]
fn changelog_marks_removed_mutual_tls_as_historical() {
    let changelog = read_doc("docs/changelog/2026-03-03.mdx");
    assert!(
        changelog.contains("removed inbound mutual TLS listener support"),
        "historical changelog should clarify that inbound mutual TLS is no longer supported"
    );
}
