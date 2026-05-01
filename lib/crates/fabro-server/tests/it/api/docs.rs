use crate::helpers::read_repo_file as read_doc;

#[test]
fn active_server_docs_describe_the_unix_socket_default() {
    let architecture = read_doc("docs/public/reference/architecture.mdx");
    assert!(
        architecture.contains("~/.fabro/fabro.sock"),
        "architecture doc should mention the default Unix socket bind"
    );

    let api_overview = read_doc("docs/public/api-reference/overview.mdx");
    assert!(
        api_overview.contains("~/.fabro/fabro.sock"),
        "API overview should mention the default Unix socket bind"
    );
}

#[test]
fn security_doc_does_not_require_jwt_keys_for_the_current_web_flow() {
    let security = read_doc("docs/public/administration/security.mdx");
    assert!(
        security.contains("SESSION_SECRET"),
        "security doc should still mention the session secret"
    );
    assert!(
        !security.contains("FABRO_JWT_PRIVATE_KEY"),
        "security doc should not mention removed JWT key settings"
    );
    assert!(
        !security.contains("FABRO_JWT_PUBLIC_KEY"),
        "security doc should not mention removed JWT key settings"
    );
}

#[test]
fn server_operations_doc_links_to_the_cli_target_section_slug() {
    let server_operations = read_doc("docs/public/reference/server-operations.mdx");
    assert!(
        server_operations.contains("/reference/user-configuration#cli-target-section"),
        "server-operations doc should link to the Mintlify slug for the [cli.target] section"
    );
}

#[test]
fn changelog_marks_removed_mutual_tls_as_historical() {
    let changelog = read_doc("docs/public/changelog/2026-03-03.mdx");
    assert!(
        changelog.contains("removed inbound mutual TLS listener support"),
        "historical changelog should clarify that inbound mutual TLS is no longer supported"
    );
}

#[test]
fn github_docs_describe_webhooks_as_strategy_dependent() {
    let github = read_doc("docs/public/integrations/github.mdx");
    assert!(
        !github.contains("enables browser OAuth and webhooks"),
        "GitHub integration docs should not imply app auth alone enables webhook delivery"
    );
    assert!(
        !github.contains("| Webhooks | No | Yes |"),
        "GitHub strategy matrix should describe webhook delivery as strategy-dependent"
    );

    let server_configuration = read_doc("docs/public/administration/server-configuration.mdx");
    assert!(
        !server_configuration.contains("enables the GitHub App flow, browser OAuth, and webhooks"),
        "server configuration docs should not imply app auth alone enables webhook delivery"
    );
}
