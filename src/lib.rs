pub mod auth;
pub mod authz;
pub mod channel_manager;
pub mod config;
pub mod control;
pub mod db;
pub mod handlers;
pub mod keys;
pub mod limits;
pub mod middleware;
pub mod models;
pub mod websocket;

use axum::http::{header, HeaderValue};
use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, patch, post},
    Router,
};
use handlers::AppState;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

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
        .route("/channels/:id", delete(handlers::delete_channel))
        .route("/channels/:id/clients", get(handlers::get_channel_clients))
        .route("/channels/:id/events", post(handlers::emit_event))
        // Admin (MASTER_KEY)
        .route("/admin/keys", get(handlers::admin_list_keys))
        .route("/admin/keys", post(handlers::admin_create_key))
        .route("/admin/keys/:id/revoke", post(handlers::admin_revoke_key))
        .route("/admin/keys/:id", delete(handlers::admin_delete_key))
        .route("/admin/keys/:id", patch(handlers::admin_update_key))
        .route("/admin/tokens/revoke", post(handlers::admin_revoke_token))
        // WS (JWT / service key, pre-auth antes del upgrade)
        .route("/ws", get(websocket::websocket_handler))
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1MB hardening (antes 10MB)
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
    let _ = dotenvy::dotenv();
    let cfg = config::AppConfig::from_env();
    let state = init_state(&cfg).await;

    // Tarea periódica: purga de JTIs revocados expirados y entradas viejas del limiter.
    spawn_cleanup_task(state.clone());

    serve_with_config(addr, state, &cfg.allowed_origins).await
}

/// Limpieza periódica en segundo plano (no bloquea el arranque).
fn spawn_cleanup_task(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(
            limits::CLEANUP_INTERVAL_SECS,
        ));
        loop {
            tick.tick().await;
            let now = chrono::Utc::now().timestamp();
            match sqlx::query("DELETE FROM revoked_tokens WHERE exp < ?")
                .bind(now)
                .execute(&state.keys.pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!("cleanup: {} jti expirados purgados", r.rows_affected());
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("cleanup revoked_tokens: {}", e),
            }
            state.limiter.cleanup();
            state.registry.cleanup();
        }
    });
}

/// Construye el AppState completo (pool DB + bootstrap dev) a partir de la config.
/// Reutilizado por el binario y por los tests de integración (DB temporal aislada).
pub async fn init_state(cfg: &config::AppConfig) -> handlers::AppState {
    let pool = db::init_pool(&cfg.database_url).await.expect("DB init");
    let control = control::ControlBus::new();
    let keys = keys::KeyStore::new_with_control(pool, control.clone());

    // Bootstrap dev: si no hay keys y está permitido, crea una inicial y la loguea UNA vez.
    // FAIL-CLOSED: si no se puede contar, NO se crea bootstrap.
    let empty = matches!(keys.count().await, Ok(0));
    if cfg.allow_bootstrap && empty {
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

    let state = handlers::AppState {
        channel_manager: channel_manager::ChannelManager::new(),
        keys,
        jwt_secret: cfg.jwt_secret.clone(),
        jwt_ttl_secs: cfg.jwt_ttl_secs,
        master_key: cfg.master_key.clone(),
        limiter: middleware::RateLimiter::new(),
        control,
        registry: limits::ConnectionRegistry::new(),
    };

    // Restaurar canales persistidos (conservan sus IDs: los JWT siguen valiendo).
    let restored = state.channel_manager.load_persisted(&state.keys.pool).await;
    if restored > 0 {
        tracing::info!("Canales restaurados: {}", restored);
    }
    state
}

/// Levanta el router en 127.0.0.1 con puerto aleatorio; diseñado para tests de integración.
/// Devuelve la dirección real para que los tests la usen con sus clientes HTTP/WS.
pub async fn start_test_server(state: AppState, allowed_origins: &[String]) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind puerto aleatorio");
    let addr = listener.local_addr().expect("addr local");
    let app = build_router(state, allowed_origins);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server test");
    });
    addr
}

pub async fn serve_with_config(addr: SocketAddr, state: AppState, allowed_origins: &[String]) {
    let app = build_router(state.clone(), allowed_origins);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("Socket-Core seguro en http://{}", addr);

    let shutdown_bus = state.control.clone();
    let shutdown = async move {
        shutdown_signal().await;
        tracing::info!("apagado ordenado: cerrando WebSockets");
        shutdown_bus.send(control::ControlEvent::Shutdown);
        // Pequeña ventana para que los WS reciban el cierre limpio.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .unwrap();
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
