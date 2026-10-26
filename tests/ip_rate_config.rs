mod common;

use common::{Hub, Project};
use serde_json::json;
use socket_core::config::AppConfig;

fn cfg(rate: i64) -> AppConfig {
    AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: "sqlite::memory:".into(),
        jwt_secret: common::JWT_SECRET.into(),
        jwt_ttl_secs: 600,
        master_key: common::MASTER_KEY.into(),
        allowed_origins: vec![],
        app_env: "dev".into(),
        allow_bootstrap: false,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
        ip_rate_per_min: rate,
    }
}

#[test]
fn ip_rate_per_min_por_defecto_es_600() {
    let cfg = AppConfig::from_env_reader(|_| None);
    assert_eq!(cfg.ip_rate_per_min, 600);
}

#[test]
fn ip_rate_per_min_se_lee_de_env() {
    let cfg = AppConfig::from_env_reader(|k| match k {
        "IP_RATE_PER_MIN" => Some("17".to_string()),
        _ => None,
    });
    assert_eq!(cfg.ip_rate_per_min, 17);
}

#[test]
fn ip_rate_per_min_fuera_de_rango_bloquea_arranque() {
    assert!(cfg(0).validate_secrets().is_err());
    assert!(cfg(100_001).validate_secrets().is_err());
    assert!(cfg(1).validate_secrets().is_ok());
    assert!(cfg(100_000).validate_secrets().is_ok());
}

#[tokio::test]
async fn init_state_aplica_ip_rate_per_min_de_la_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("ip-rate.db");
    let cfg = AppConfig {
        database_url: format!("sqlite:{}?mode=rwc", db.display()),
        ip_rate_per_min: 23,
        ..cfg(23)
    };
    let state = socket_core::init_state(&cfg).await;
    assert_eq!(state.limits.ip_rate_per_min, 23);
}

#[tokio::test]
async fn emit_por_jwt_usa_el_limite_por_ip_configurado() {
    let limits = socket_core::limits::Limits {
        ip_rate_per_min: 2,
        ..Default::default()
    };
    let hub = Hub::start_with_limits(limits).await;
    hub.create_channel_ok(Project::A, "a:chat").await;
    let token = hub.mint_jwt(Project::A, "u1", "a:chat", "publish").await;

    let mut statuses = Vec::new();
    for i in 0..3 {
        let res = hub
            .emit_with_bearer(&token, "a:chat", vec!["*".into()], json!({"i": i}))
            .await;
        statuses.push(res.status().as_u16());
    }
    assert_eq!(
        statuses,
        vec![200, 200, 429],
        "el bucket emit: debe usar IP_RATE_PER_MIN, no 600 fijo"
    );
}
