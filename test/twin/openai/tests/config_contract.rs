use twin_openai::config::Config;

#[test]
fn config_loads_from_environment() {
    let prior_bind = std::env::var("TWIN_OPENAI_BIND_ADDR").ok();
    let prior_auth = std::env::var("TWIN_OPENAI_REQUIRE_AUTH").ok();
    let prior_admin = std::env::var("TWIN_OPENAI_ENABLE_ADMIN").ok();

    std::env::set_var("TWIN_OPENAI_BIND_ADDR", "127.0.0.1:4100");
    std::env::set_var("TWIN_OPENAI_REQUIRE_AUTH", "false");
    std::env::set_var("TWIN_OPENAI_ENABLE_ADMIN", "false");

    let config = Config::from_env().expect("config should load");

    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:4100");
    assert!(!config.require_auth);
    assert!(!config.enable_admin);

    match prior_bind {
        Some(value) => std::env::set_var("TWIN_OPENAI_BIND_ADDR", value),
        None => std::env::remove_var("TWIN_OPENAI_BIND_ADDR"),
    }
    match prior_auth {
        Some(value) => std::env::set_var("TWIN_OPENAI_REQUIRE_AUTH", value),
        None => std::env::remove_var("TWIN_OPENAI_REQUIRE_AUTH"),
    }
    match prior_admin {
        Some(value) => std::env::set_var("TWIN_OPENAI_ENABLE_ADMIN", value),
        None => std::env::remove_var("TWIN_OPENAI_ENABLE_ADMIN"),
    }
}
