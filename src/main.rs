use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use shared_audio_clock::config::Config;
use shared_audio_clock::tls::ensure_tls_config;
use shared_audio_clock::{router, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let state = AppState::new(config.schedule_lead_time);

    let app = router(state)
        .fallback_service(ServeDir::new("public"))
        .layer(TraceLayer::new_for_http());

    let addr: std::net::SocketAddr = config
        .bind_address
        .parse()
        .expect("BIND_ADDRESS must be a valid host:port");

    if config.tls_enabled {
        let (tls_config, sans) = ensure_tls_config(&config.tls_cert_dir)
            .await
            .expect("failed to prepare TLS certificate");

        tracing::info!(bind_address = %config.bind_address, "starting shared audio clock server over HTTPS");
        for san in sans.iter().filter(|san| san.as_str() != "::1") {
            tracing::info!("open https://{san}:{}/ - accept the self-signed certificate warning once per browser/device", addr.port());
        }

        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
            .expect("server error");
    } else {
        tracing::info!(bind_address = %config.bind_address, "starting shared audio clock server");

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("failed to bind address");

        axum::serve(listener, app).await.expect("server error");
    }
}
