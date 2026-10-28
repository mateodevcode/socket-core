// FASE 5 - Red, secretos y configuración. Tests primero.
mod common;

use common::{Hub, Project};
use serde_json::json;
use std::time::Duration;

// ---------- A) Arranque estricto ----------

#[test]
fn allow_bootstrap_por_defecto_es_false() {
    let cfg = socket_core::config::AppConfig::from_env_reader(|_| None);
    assert!(
        !cfg.allow_bootstrap,
        "ALLOW_BOOTSTRAP debe ser false por defecto"
    );
}

#[test]
fn jwt_ttl_fuera_de_rango_bloquea_arranque() {
    let base = |ttl: i64| socket_core::config::AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: "sqlite::memory:".into(),
        jwt_secret: common::JWT_SECRET.into(),
        jwt_ttl_secs: ttl,
        master_key: common::MASTER_KEY.into(),
        allowed_origins: vec!["https://app.example".into()],
        app_env: "prod".into(),
        allow_bootstrap: false,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
        ip_rate_per_min: 600,
    };
    assert!(base(59).validate_secrets().is_err());
    assert!(base(3601).validate_secrets().is_err());
    assert!(base(60).validate_secrets().is_ok());
    assert!(base(3600).validate_secrets().is_ok());
}

#[test]
fn cors_produccion_exige_origenes() {
    let base = |env: &str, origins: Vec<String>| socket_core::config::AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: "sqlite::memory:".into(),
        jwt_secret: common::JWT_SECRET.into(),
        jwt_ttl_secs: 600,
        master_key: common::MASTER_KEY.into(),
        allowed_origins: origins,
        app_env: env.into(),
        allow_bootstrap: false,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
        ip_rate_per_min: 600,
    };
    assert!(base("prod", vec![]).validate_origins().is_err());
    assert!(base("prod", vec!["*".into()]).validate_origins().is_err());
    assert!(base("prod", vec!["https://app.example".into()])
        .validate_origins()
        .is_ok());
    assert!(base("dev", vec![]).validate_origins().is_ok());
}

#[test]
fn bootstrap_escribe_fichero_0600_y_no_loguea_la_key() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    struct W(Buf);
    impl Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 .0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = W;
        fn make_writer(&'a self) -> Self::Writer {
            W(self.clone())
        }
    }

    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("boot.db");
    let cfg = socket_core::config::AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", db.display()),
        jwt_secret: common::JWT_SECRET.into(),
        jwt_ttl_secs: 600,
        master_key: common::MASTER_KEY.into(),
        allowed_origins: vec![],
        app_env: "dev".into(),
        allow_bootstrap: true,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
        ip_rate_per_min: 600,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _state = rt.block_on(socket_core::init_state(&cfg));

    let path = std::path::Path::new("./data/bootstrap-key.txt");
    let contents = std::fs::read_to_string(path).expect("fichero de bootstrap");
    assert!(
        contents.starts_with("sc_live_"),
        "el fichero debe contener la key"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "permisos 0600");
    }
    let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).to_string();
    assert!(!logs.contains("sc_live_"), "el log NO debe contener la key");
    assert!(
        logs.contains("bootstrap-key.txt"),
        "el log debe indicar la ruta"
    );
    let _ = std::fs::remove_file(path);
}

// ---------- B) IP del cliente y rate limiting ----------

fn limits_with_ip_rate(rate: i64) -> socket_core::limits::Limits {
    socket_core::limits::Limits {
        ip_rate_per_min: rate,
        ..Default::default()
    }
}

#[tokio::test]
async fn xff_rotante_de_cliente_no_confiable_no_evade_limite() {
    let hub = Hub::start_with_limits(limits_with_ip_rate(5)).await;
    let id = hub.create_channel_ok(Project::A, "a:chat").await;
    let mut any_429 = false;
    for i in 0..10u32 {
        let res = hub
            .client
            .post(format!("{}/channels/{}/events", hub.base, id))
            .header("x-api-key", &hub.key_a)
            .header("x-forwarded-for", format!("10.9.9.{}", i))
            .json(&json!({"source": "b", "targets": ["*"], "payload": {"i": i}}))
            .send()
            .await
            .unwrap();
        if res.status() == 429 {
            any_429 = true;
            break;
        }
    }
    assert!(
        any_429,
        "XFF de cliente no confiable no debe evadir el límite por IP"
    );
}

#[tokio::test]
async fn xff_desde_proxy_confiable_se_respeta() {
    let trusted: Vec<ipnet::IpNet> = vec!["127.0.0.1/32".parse().unwrap()];
    let hub = Hub::start_with_options(limits_with_ip_rate(5), trusted).await;
    let id = hub.create_channel_ok(Project::A, "a:chat").await;
    let mut any_429 = false;
    for i in 0..10u32 {
        let res = hub
            .client
            .post(format!("{}/channels/{}/events", hub.base, id))
            .header("x-api-key", &hub.key_a)
            .header("x-forwarded-for", format!("10.9.9.{}", i))
            .json(&json!({"source": "b", "targets": ["*"], "payload": {"i": i}}))
            .send()
            .await
            .unwrap();
        if res.status() == 429 {
            any_429 = true;
            break;
        }
    }
    assert!(
        !any_429,
        "IPs distintas tras un proxy confiable no comparten cupo"
    );
}

#[tokio::test]
async fn rate_limit_incluye_retry_after() {
    let hub = Hub::start_with_limits(limits_with_ip_rate(2)).await;
    let id = hub.create_channel_ok(Project::A, "a:chat").await;
    let mut retry_after = false;
    for i in 0..6u32 {
        let res = hub
            .client
            .post(format!("{}/channels/{}/events", hub.base, id))
            .header("x-api-key", &hub.key_a)
            .json(&json!({"source": "b", "targets": ["*"], "payload": {"i": i}}))
            .send()
            .await
            .unwrap();
        if res.status() == 429 {
            retry_after = res.headers().get("retry-after").is_some();
            break;
        }
    }
    assert!(retry_after, "429 debe incluir cabecera Retry-After");
}

// ---------- C) /admin separado ----------

#[tokio::test]
async fn admin_no_existe_en_puerto_publico() {
    let hub = Hub::start().await;
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        404,
        "el router público NO debe tener /admin/*"
    );
}

#[tokio::test]
async fn admin_listener_separado_funciona() {
    let hub = Hub::start().await;
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.admin_base))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn admin_bloquea_tras_fallos_de_master() {
    let hub = Hub::start().await;
    for _ in 0..5 {
        let res = hub
            .client
            .get(format!("{}/admin/keys", hub.admin_base))
            .header("x-master-key", "incorrecta")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
    }
    // Incluso con la master correcta: bloqueado.
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.admin_base))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        429,
        "tras N fallos debe bloquear temporalmente"
    );
}

#[tokio::test]
async fn admin_stats_global_por_proyecto() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .client
        .get(format!("{}/admin/stats", hub.admin_base))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: serde_json::Value = res.json().await.unwrap();
    let projects = v["data"]["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 2, "A y B");
    let a = projects
        .iter()
        .find(|p| p["project_id"] == hub.project_a.as_str())
        .expect("proyecto A");
    assert_eq!(a["channels"].as_u64(), Some(1));
    assert!(a.get("connections").is_some() && a.get("clients").is_some());
}

#[tokio::test]
async fn admin_error_db_sin_detalles_internos() {
    let hub = Hub::start().await;
    hub.state.keys.pool.close().await;
    let res = hub
        .client
        .post(format!("{}/admin/keys", hub.admin_base))
        .header("x-master-key", common::MASTER_KEY)
        .json(&json!({"name": "x", "channel_prefix": "z:*"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
    let body = res.text().await.unwrap();
    assert!(
        !body.contains("DB error") && !body.to_lowercase().contains("sqlite"),
        "no debe filtrar detalles internos: {}",
        body
    );
}

// ---------- D) Auditoría ----------

#[tokio::test]
async fn auditoria_registra_acciones_sin_filtrar_secretos() {
    let hub = Hub::start().await;
    let created: serde_json::Value = hub
        .admin_create_key("audit-proj", "z:*", &["publish", "subscribe"])
        .await
        .json()
        .await
        .unwrap();
    let raw = created["data"]["api_key"]
        .as_str()
        .expect("api_key")
        .to_string();
    let key_id = created["data"]["id"].as_str().unwrap().to_string();

    // Autenticación fallida con una key inventada y con un JWT inválido.
    let _ = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", "sc_live_inventada_inventada_inventada")
        .send()
        .await;
    let _ = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("Authorization", "Bearer no.es.jwt")
        .send()
        .await;
    // Revocación
    let _ = hub
        .client
        .post(format!("{}/admin/keys/{}/revoke", hub.admin_base, key_id))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await;

    let entries = hub.state.audit.snapshot();
    assert!(!entries.is_empty(), "debe haber entradas de auditoría");
    assert!(entries.iter().any(|e| e.contains("key_create")));
    assert!(entries.iter().any(|e| e.contains("key_revoke")));
    assert!(entries.iter().any(|e| e.contains("auth_failed")));
    for e in &entries {
        assert!(
            !e.contains(&raw),
            "la auditoría NO debe contener la API key: {}",
            e
        );
        assert!(
            !e.contains("no.es.jwt"),
            "la auditoría NO debe contener JWT: {}",
            e
        );
        assert!(
            !e.contains("inventada_inventada"),
            "ni keys inventadas: {}",
            e
        );
    }
}

// ---------- E) Presencia y apagado ----------

#[tokio::test]
async fn evento_join_no_incluye_lista_de_presencia() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt1 = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let jwt2 = hub
        .mint_jwt(Project::A, "user-2", "a:chat", "subscribe")
        .await;
    let mut c1 = hub.ws_open(&jwt1, "a:chat").await.expect("c1");
    let _c2 = hub.ws_open(&jwt2, "a:chat").await.expect("c2");

    // c1 recibe el client_joined de user-2.
    let mut join = None;
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut c1, Duration::from_secs(2)).await {
            if v["payload"]["type"] == "client_joined" {
                join = Some(v);
                break;
            }
        }
    }
    let join = join.expect("evento client_joined");
    let payload = &join["payload"];
    assert_eq!(payload["user"], "user-2");
    assert!(payload["connection_id"].as_str().is_some());
    assert!(payload["total_clients"].as_u64().unwrap() >= 2);
    assert!(
        payload.get("clients").is_none(),
        "el evento NO debe exponer la lista completa: {:?}",
        payload
    );
}

#[tokio::test]
async fn apagado_respeta_timeout_de_gracia() {
    // Un servidor que no termina nunca debe rendirse tras la gracia.
    let inicio = std::time::Instant::now();
    let termino =
        socket_core::await_shutdown(std::future::pending::<()>(), Duration::from_millis(200)).await;
    assert!(!termino, "no terminó => false");
    assert!(inicio.elapsed() < Duration::from_secs(2));

    // Un servidor que termina rápido devuelve true.
    let termino = socket_core::await_shutdown(async {}, Duration::from_millis(500)).await;
    assert!(termino);
}

#[tokio::test]
async fn servidor_no_se_apaga_solo_antes_de_la_senal() {
    // Regresión: el plazo de gracia NO debe iniciarse al arrancar (antes el
    // servicio se auto-apagaba a los SHUTDOWN_GRACE_SECS).
    let bus = socket_core::control::ControlBus::new();
    let fut = socket_core::await_servers_with_grace(
        std::future::pending::<()>(),
        std::future::pending::<()>(),
        &bus,
        None,
        Duration::from_millis(150),
    );
    tokio::pin!(fut);
    let seguia_vivo = tokio::time::timeout(Duration::from_millis(700), &mut fut)
        .await
        .is_err();
    assert!(
        seguia_vivo,
        "sin señal no debe terminar aunque pase la gracia"
    );
}

#[tokio::test]
async fn apagado_por_senal_respeta_gracia_y_termina() {
    let bus = socket_core::control::ControlBus::new();
    let servidores = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        socket_core::await_servers_with_grace(
            servidores,
            std::future::ready(()),
            &bus,
            None,
            Duration::from_millis(500),
        ),
    )
    .await
    .expect("debe terminar dentro de la gracia");
    assert_eq!(outcome, socket_core::ShutdownOutcome::Completed);
}

#[tokio::test]
async fn servidores_terminando_antes_de_senal_es_error() {
    let bus = socket_core::control::ControlBus::new();
    let outcome = socket_core::await_servers_with_grace(
        async {},
        std::future::pending::<()>(),
        &bus,
        None,
        Duration::from_millis(200),
    )
    .await;
    assert_eq!(
        outcome,
        socket_core::ShutdownOutcome::ServersFinishedEarly,
        "si los servidores mueren solos debe ser un fallo (no Completed)"
    );
    assert_eq!(
        outcome.exit_code(),
        1,
        "el proceso debe salir con código != 0"
    );
    assert_eq!(socket_core::ShutdownOutcome::GraceExpired.exit_code(), 1);
    assert_eq!(socket_core::ShutdownOutcome::Completed.exit_code(), 0);
}
