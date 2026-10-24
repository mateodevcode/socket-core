use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let cfg = socket_core::config::AppConfig::from_env();

    if cfg.jwt_secret == "dev-change-me-please-rotate" {
        tracing::warn!("JWT_SECRET por defecto en uso. Define JWT_SECRET en producción.");
    }
    if cfg.master_key.is_empty() {
        tracing::warn!("MASTER_KEY vacío: endpoints /admin/* bloqueados hasta configurarlo.");
    }

    let addr: SocketAddr = cfg
        .addr()
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 3005)));
    tracing::info!("Socket-Core (definitivo, seguro) en {}", addr);
    socket_core::serve(addr).await;
}
