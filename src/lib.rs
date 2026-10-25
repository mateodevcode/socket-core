pub mod audit;
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

/// Métodos permitidos por CORS (explícitos, nunca Any).
fn cors_methods() -> [axum::http::Method; 5] {
    [
        axum::http::Method::GET,
        axum::http::Method::POST,
        axum::http::Method::PATCH,
        axum::http::Method::DELETE,
        axum::http::Method::OPTIONS,
    ]
}

fn cors_headers() -> [header::HeaderName; 3] {
    [
        header::CONTENT_TYPE,
        header::AUTHORIZATION,
        header::HeaderName::from_static("x-api-key"),
    ]
}

/// Router público: SIN rutas /admin. CORS con métodos/cabeceras explícitos.
pub fn build_public_router(state: AppState) -> Router {
    let cors =
        if !state.allowed_origins.is_empty() && !state.allowed_origins.iter().any(|o| o == "*") {
            let origins: Vec<HeaderValue> = state
                .allowed_origins
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect();
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(cors_methods())
                .allow_headers(cors_headers())
        } else if state.dev_mode {
            tracing::warn!("CORS permisivo (modo dev): ALLOWED_ORIGINS vacío o *.");
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(cors_methods())
                .allow_headers(cors_headers())
        } else {
            // Prod sin orígenes: el arranque estricto lo impide; aquí CORS cerrado.
            CorsLayer::new().allow_origin(Vec::<HeaderValue>::new())
        };

    Router::new()
        .route("/health", get(handlers::health))
        .route("/auth/token", post(handlers::mint_token))
        .route("/stats", get(handlers::stats))
        .route("/channels", post(handlers::create_channel))
        .route("/channels", get(handlers::list_channels))
        .route("/channels/:id", get(handlers::get_channel))
        .route("/channels/:id", delete(handlers::delete_channel))
        .route("/channels/:id/clients", get(handlers::get_channel_clients))
        .route("/channels/:id/events", post(handlers::emit_event))
        .route("/ws", get(websocket::websocket_handler))
        .layer(DefaultBodyLimit::max(1024 * 1024))
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

/// Router de administración: se sirve SOLO en loopback (ADMIN_PORT).
pub fn build_admin_router(state: AppState) -> Router {
    Router::new()
        // Health sin auth en el listener admin: lo usa el HEALTHCHECK del contenedor.
        .route("/health", get(handlers::health))
        .route("/admin/keys", get(handlers::admin_list_keys))
        .route("/admin/keys", post(handlers::admin_create_key))
        .route("/admin/keys/:id/revoke", post(handlers::admin_revoke_key))
        .route("/admin/keys/:id", delete(handlers::admin_delete_key))
        .route("/admin/keys/:id", patch(handlers::admin_update_key))
        .route("/admin/tokens/revoke", post(handlers::admin_revoke_token))
        .route("/admin/stats", get(handlers::admin_stats))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

/// Compat: router público (sin admin).
pub fn build_router(state: AppState) -> Router {
    build_public_router(state)
}

/// Arranca el servidor socket-core (bloqueante). Usa ENV para secrets/DB.
pub async fn serve(addr: SocketAddr) {
    let _ = dotenvy::dotenv();
    let cfg = config::AppConfig::from_env();

    // Arranque estricto: aborta con mensaje claro (sin secretos) si algo falta.
    if let Err(e) = cfg.validate_secrets().and_then(|_| cfg.validate_origins()) {
        eprintln!("socket-core: configuración inválida: {}", e);
        std::process::exit(1);
    }

    let state = init_state(&cfg).await;
    spawn_cleanup_task(state.clone());

    let admin_addr: SocketAddr = cfg.admin_addr().parse().expect("ADMIN_PORT inválido");
    serve_with_config(addr, admin_addr, state, cfg.shutdown_grace_secs).await
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
            state.publish_limits.cleanup();
            state.admin_guard.cleanup();
        }
    });
}

/// Construye el AppState completo (pool DB + bootstrap) a partir de la config.
/// Reutilizado por el binario y por los tests de integración (DB temporal aislada).
pub async fn init_state(cfg: &config::AppConfig) -> handlers::AppState {
    let pool = db::init_pool(&cfg.database_url).await.expect("DB init");
    let control = control::ControlBus::new();
    let keys = keys::KeyStore::new_with_control(pool, control.clone());

    // Bootstrap dev: crea una key inicial y la escribe SOLO a un fichero 0600.
    // NUNCA se loguea la key en claro. FAIL-CLOSED: sin poder contar, no hay bootstrap.
    let empty = matches!(keys.count().await, Ok(0));
    if cfg.allow_bootstrap && cfg.is_dev() && empty {
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
            Ok((_, raw)) => match write_bootstrap_key(&raw) {
                Ok(path) => tracing::warn!(
                    "BOOTSTRAP: key inicial creada; credencial en {} (0600)",
                    path
                ),
                Err(e) => tracing::error!("bootstrap: no se pudo escribir la credencial: {}", e),
            },
            Err(e) => tracing::error!("bootstrap key error: {}", e),
        }
    }

    // Q7: avisos de configuración de keys existentes (SOLO nombres, nunca secretos).
    if let Ok(keys_list) = keys.list().await {
        for k in &keys_list {
            if let Err(e) = crate::models::validate_scopes(&k.scopes) {
                tracing::warn!("key '{}' con scopes inválidos: {}", k.name, e);
            }
        }
        for i in 0..keys_list.len() {
            for j in (i + 1)..keys_list.len() {
                if crate::keys::prefixes_overlap(
                    &keys_list[i].channel_prefixes,
                    &keys_list[j].channel_prefixes,
                ) {
                    tracing::warn!(
                        "keys '{}' y '{}' tienen prefijos solapados",
                        keys_list[i].name,
                        keys_list[j].name
                    );
                }
            }
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
        limits: limits::Limits::default(),
        publish_limits: limits::ProjectPublishLimiters::new(),
        dev_mode: cfg.is_dev(),
        trusted_proxies: cfg.trusted_proxy_nets(),
        audit: audit::AuditLog::new(),
        admin_guard: middleware::AdminGuard::new(),
        allowed_origins: cfg.allowed_origins.clone(),
    };

    match state.channel_manager.load_persisted(&state.keys.pool).await {
        Ok(n) if n > 0 => tracing::info!("Canales restaurados: {}", n),
        Ok(_) => {}
        Err(e) => tracing::error!("no se pudieron cargar los canales: {}", e),
    }
    state
}

/// Escribe la key de bootstrap en un fichero 0600 (nunca se loguea en claro).
fn write_bootstrap_key(raw: &str) -> std::io::Result<String> {
    use std::io::Write;
    let dir = std::path::Path::new("./data");
    std::fs::create_dir_all(dir)?;
    let path = dir.join("bootstrap-key.txt");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(raw.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(raw.as_bytes())?;
    }
    Ok(path.display().to_string())
}

/// Levanta el router público en 127.0.0.1:0 (tests de integración).
pub async fn start_test_server(state: AppState) -> SocketAddr {
    let app = build_public_router(state);
    start_router(app).await
}

/// Levanta el router admin en 127.0.0.1:0 (tests de integración).
pub async fn start_test_admin_server(state: AppState) -> SocketAddr {
    let app = build_admin_router(state);
    start_router(app).await
}

async fn start_router(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind puerto aleatorio");
    let addr = listener.local_addr().expect("addr local");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("server test");
    });
    addr
}

/// Sirve público + admin con apagado ordenado y timeout total (gracia).
pub async fn serve_with_config(
    addr: SocketAddr,
    admin_addr: SocketAddr,
    state: AppState,
    grace_secs: u64,
) {
    let public = build_public_router(state.clone());
    let admin = build_admin_router(state.clone());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    let admin_listener = tokio::net::TcpListener::bind(&admin_addr).await.unwrap();
    tracing::info!("Socket-Core público en http://{}", addr);
    tracing::info!("Socket-Core admin (loopback) en http://{}", admin_addr);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut rx1 = shutdown_rx.clone();
    let server1 = axum::serve(
        listener,
        public.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = rx1.changed().await;
    });
    let mut rx2 = shutdown_rx;
    let server2 = axum::serve(
        admin_listener,
        admin.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = rx2.changed().await;
    });
    let servers = async {
        let _ = tokio::join!(server1, server2);
    };
    tokio::pin!(servers);

    // El timeout de gracia NO empieza al arrancar: solo tras la señal de apagado.
    let outcome = await_servers_with_grace(
        servers,
        shutdown_signal(),
        &state.control,
        Some(shutdown_tx),
        std::time::Duration::from_secs(grace_secs),
    )
    .await;
    match outcome {
        ShutdownOutcome::Completed => {}
        ShutdownOutcome::ServersFinishedEarly => {
            tracing::error!("un listener terminó antes de la señal de apagado; saliendo con error");
            std::process::exit(outcome.exit_code());
        }
        ShutdownOutcome::GraceExpired => {
            tracing::warn!(
                "apagado: tiempo de gracia ({}s) agotado; terminando de todas formas",
                grace_secs
            );
            std::process::exit(outcome.exit_code());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    Completed,
    GraceExpired,
    /// Los servidores terminaron ANTES de recibir la señal (fallo inesperado).
    ServersFinishedEarly,
}

impl ShutdownOutcome {
    /// Código de salida del proceso para este desenlace.
    pub fn exit_code(self) -> i32 {
        match self {
            ShutdownOutcome::Completed => 0,
            ShutdownOutcome::GraceExpired | ShutdownOutcome::ServersFinishedEarly => 1,
        }
    }
}

/// Ejecuta los servidores hasta que llegue `signal`; entonces avisa por el bus y
/// espera como máximo `grace`. Si los servidores no terminan, devuelve GraceExpired.
/// (El plazo de gracia empieza SOLO al recibir la señal, no al arrancar.)
pub async fn await_servers_with_grace<Fut, Sig>(
    servers: Fut,
    signal: Sig,
    bus: &control::ControlBus,
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    grace: std::time::Duration,
) -> ShutdownOutcome
where
    Fut: std::future::Future<Output = ()>,
    Sig: std::future::Future<Output = ()>,
{
    tokio::pin!(servers);
    tokio::select! {
        _ = &mut servers => return ShutdownOutcome::ServersFinishedEarly,
        _ = signal => {}
    }

    tracing::info!("apagado ordenado: cerrando WebSockets");
    bus.send(control::ControlEvent::Shutdown);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if let Some(tx) = shutdown_tx {
        let _ = tx.send(true);
    }

    if tokio::time::timeout(grace, &mut servers).await.is_err() {
        ShutdownOutcome::GraceExpired
    } else {
        ShutdownOutcome::Completed
    }
}

/// Espera a que el servidor termine, como máximo `grace`. true si terminó a tiempo.
pub async fn await_shutdown<F>(server: F, grace: std::time::Duration) -> bool
where
    F: std::future::Future<Output = ()>,
{
    tokio::select! {
        _ = server => true,
        _ = tokio::time::sleep(grace) => false,
    }
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
