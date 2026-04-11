use anyhow::Result;
use tokio::net::TcpListener;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use twin_openai::app;
use twin_openai::config::Config;
use twin_openai::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(fmt::layer())
        .init();

    let config = Config::from_env()?;
    let listener = TcpListener::bind(config.bind_addr).await?;
    axum::serve(listener, app::router(AppState::new(config))).await?;
    Ok(())
}
