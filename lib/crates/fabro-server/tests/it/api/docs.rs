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
