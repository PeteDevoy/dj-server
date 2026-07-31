use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use shared_audio_clock::config::Config;
use shared_audio_clock::{router, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Config::from_env();
    let state = AppState::new(config.schedule_lead_time);

    let app = router(state)
        .fallback_service(ServeDir::new("public"))
        .layer(TraceLayer::new_for_http());

    tracing::info!(bind_address = %config.bind_address, "starting shared audio clock server");

    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("failed to bind address");

    axum::serve(listener, app)
        .await
        .expect("server error");
}
