use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::Error;
use crate::provider::StreamEventStream;
use crate::types::{Request, Response};

/// The next handler in the middleware chain.
pub type NextFn = Arc<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send>> + Send + Sync,
>;

/// The next handler for streaming.
pub type NextStreamFn = Arc<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Result<StreamEventStream, Error>> + Send>>
        + Send
        + Sync,
>;

/// Middleware for intercepting `complete()` and streaming calls (Section 2.3).
#[async_trait::async_trait]
pub trait Middleware: Send + Sync {
    async fn handle_complete(&self, request: Request, next: NextFn) -> Result<Response, Error>;

    async fn handle_stream(
        &self,
        request: Request,
        next: NextStreamFn,
    ) -> Result<StreamEventStream, Error>;
}
