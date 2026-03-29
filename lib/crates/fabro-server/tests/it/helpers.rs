use fabro_workflows::pipeline::LlmSpec;

pub(crate) fn test_llm_spec() -> LlmSpec {
    LlmSpec {
        model: "test-model".to_string(),
        provider: fabro_llm::Provider::Anthropic,
        fallback_chain: Vec::new(),
        mcp_servers: Vec::new(),
        dry_run: true,
    }
}

pub(crate) async fn test_db() -> sqlx::SqlitePool {
    let pool = fabro_db::connect_memory().await.unwrap();
    fabro_db::initialize_db(&pool).await.unwrap();
    pool
}
