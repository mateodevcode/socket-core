pub mod auth;
pub mod channel_manager;
pub mod config;
pub mod db;
pub mod handlers;
pub mod keys;
pub mod middleware;
pub mod models;
pub mod websocket;

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, post},
    Router,
};
use handlers::AppState;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use axum::http::{header, HeaderValue};

/// Construye el Router completo del hub de transporte.
/// Tanto el binario standalone como consumidores embebidos usan esto.
pub fn build_router(state: AppState, allowed_origins: &[String]) -> Router {
    let cors = if allowed_origins.is_empty() || allowed_origins.iter().any(|o| o == "*") {
        tracing::warn!("CORS permisivo activo (ALLOWED_ORIGINS vacío o *). Solo para dev.");
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    };

    Router::new()
        // Públicas
        .route("/health", get(handlers::health))
        // Auth (requiere X-Api-Key)
        .route("/auth/token", post(handlers::mint_token))
        // Protegidas (API key / JWT)
        .route("/stats", get(handlers::stats))
        .route("/channels", post(handlers::create_channel))
        .route("/channels", get(handlers::list_channels))
        .route("/channels/:id", get(handlers::get_channel))
        .route("/channels/:id/clients", get(handlers::get_channel_clients))
        .route("/channels/:id/events", post(handlers::emit_event))
        // Admin (MASTER_KEY)
        .route("/admin/keys", get(handlers::admin_list_keys))
        .route("/admin/keys", post(handlers::admin_create_key))
        .route("/admin/keys/:id/revoke", post(handlers::admin_revoke_key))
        .route("/admin/keys/:id", delete(handlers::admin_delete_key))
        .route("/admin/tokens/revoke", post(handlers::admin_revoke_token))
        // WS (JWT / service key, pre-auth antes del upgrade)
        .route("/ws", get(websocket::websocket_handler))
        .layer(DefaultBodyLimit::max(1 * 1024 * 1024)) // 1MB hardening (antes 10MB)
        .layer(cors)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .with_state(state)
}

/// Arranca el servidor socket-core en la addr dada (bloqueante).
/// Mantiene compat con la firma vieja; usa ENV para secrets/DB.
pub async fn serve(addr: SocketAddr) {
    let _ = dotenv::dotenv();
    let cfg = config::AppConfig::from_env();
    let pool = db::init_pool(&cfg.database_url).await.expect("DB init");
    let keys = keys::KeyStore::new(pool);

    // Bootstrap dev: si no hay keys y está permitido, crea una inicial y la loguea UNA vez.
    if cfg.allow_bootstrap && keys.count().await == 0 {
        match keys
            .create(
                "bootstrap-dev".into(),
                vec!["*".into()],
                "*".into(),
                vec!["publish".into(), "subscribe".into(), "manage".into()],
                600,
                None,
            )
            .await
        {
            Ok((_, raw)) => tracing::warn!(
                "BOOTSTRAP: key inicial creada (solo dev, rota esto): {}",
                raw
            ),
            Err(e) => tracing::error!("bootstrap key error: {}", e),
        }
    }

    let state = AppState {
        channel_manager: channel_manager::ChannelManager::new(),
        keys,
        jwt_secret: cfg.jwt_secret.clone(),
        jwt_ttl_secs: cfg.jwt_ttl_secs,
        master_key: cfg.master_key.clone(),
        limiter: middleware::RateLimiter::new(),
    };

    // Restaurar canales persistidos (conservan sus IDs: los JWT siguen valiendo).
    let restored = state
        .channel_manager
        .load_persisted(&state.keys.pool)
        .await;
    if restored > 0 {
        tracing::info!("Canales restaurados: {}", restored);
    }

    serve_with_config(addr, state, &cfg.allowed_origins).await
}

pub async fn serve_with_config(addr: SocketAddr, state: AppState, allowed_origins: &[String]) {
    let app = build_router(state, allowed_origins);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("Socket-Core seguro en http://{}", addr);
    axum::serve(listener, app).await.unwrap();
}
