use std::sync::Arc;

use axum::Router;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use crate::handlers;
use crate::state::{AppState, init_git_repos};

pub type SharedState = Arc<RwLock<AppState>>;

/// A running test server instance.
pub struct TestServer {
    url:         String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle:      Option<tokio::task::JoinHandle<()>>,
    _git_root:   TempDir, // Kept alive for the server's lifetime; cleaned up on drop
}

impl TestServer {
    /// Start the fake GitHub API server on a random port.
    pub async fn start(mut state: AppState) -> Self {
        let git_root = TempDir::new().expect("failed to create temp git root");
        init_git_repos(&mut state, git_root.path()).expect("failed to initialize git repos");

        let shared_state: SharedState = Arc::new(RwLock::new(state));
        let router = build_router(shared_state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        Self {
            url,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
            _git_root: git_root,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

pub fn build_router(state: SharedState) -> Router {
    handlers::build_router(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_starts_and_responds() {
        let state = AppState::new();
        let server = TestServer::start(state).await;
        let resp = crate::test_support::test_http_client()
            .get(format!("{}/nonexistent", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }
}
