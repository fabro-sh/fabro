pub(crate) async fn test_db() -> sqlx::SqlitePool {
    let pool = fabro_db::connect_memory().await.unwrap();
    fabro_db::initialize_db(&pool).await.unwrap();
    pool
}
