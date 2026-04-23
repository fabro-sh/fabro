use twin_openai::config::Config;

#[test]
fn config_loads_from_environment() {
    let config = Config::from_lookup(&|name| match name {
        "TWIN_OPENAI_BIND_ADDR" => Some("127.0.0.1:4100".to_string()),
        "TWIN_OPENAI_REQUIRE_AUTH" | "TWIN_OPENAI_ENABLE_ADMIN" => Some("false".to_string()),
        _ => None,
    })
    .expect("config should load");

    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:4100");
    assert!(!config.require_auth);
    assert!(!config.enable_admin);
}
