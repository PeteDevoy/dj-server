pub mod clock;
pub mod config;
pub mod protocol;
pub mod room;
pub mod websocket;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;

pub use websocket::AppState;

/// Builds the axum router: the `/ws` upgrade endpoint plus whatever
/// additional routes/state the caller layers on top (e.g. static files).
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ws", get(websocket::ws_handler))
        .with_state(state)
}
