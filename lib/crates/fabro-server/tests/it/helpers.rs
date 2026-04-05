use std::sync::Arc;

use fabro_server::server::{AppState, create_app_state};

pub(crate) fn test_app_state() -> Arc<AppState> {
    create_app_state()
}
