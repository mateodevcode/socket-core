// FASE 1 - Item 14: arranque estricto. El servicio debe NEGARSE a iniciar con
// JWT_SECRET por defecto o corto (<32) o MASTER_KEY vacía/corta (<32).
mod common;

use socket_core::config::AppConfig;

fn cfg(jwt: &str, master: &str) -> AppConfig {
    AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: "sqlite::memory:".into(),
        jwt_secret: jwt.into(),
        jwt_ttl_secs: 600,
        master_key: master.into(),
        allowed_origins: vec![],
        app_env: "prod".into(),
        allow_bootstrap: false,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
    }
}

#[test]
fn jwt_secret_por_defecto_debe_bloquear_arranque() {
    assert!(
        cfg("dev-change-me-please-rotate", common::MASTER_KEY)
            .validate_secrets()
            .is_err(),
        "JWT_SECRET por defecto debe impedir el arranque"
    );
}

#[test]
fn jwt_secret_corto_debe_bloquear_arranque() {
    assert!(cfg("corta", common::MASTER_KEY).validate_secrets().is_err());
    assert!(cfg(&"a".repeat(31), common::MASTER_KEY)
        .validate_secrets()
        .is_err());
    let s32 = "a".repeat(32);
    assert!(cfg(&s32, common::MASTER_KEY).validate_secrets().is_ok());
}

#[test]
fn master_key_vacia_o_corta_debe_bloquear_arranque() {
    assert!(cfg(common::JWT_SECRET, "").validate_secrets().is_err());
    assert!(cfg(common::JWT_SECRET, "corta").validate_secrets().is_err());
    assert!(cfg(common::JWT_SECRET, &"b".repeat(31))
        .validate_secrets()
        .is_err());
}

#[test]
fn secretos_validos_arrancan() {
    assert!(cfg(common::JWT_SECRET, common::MASTER_KEY)
        .validate_secrets()
        .is_ok());
}

#[test]
fn trusted_proxies_no_puede_ser_loopback_ni_todo() {
    let con = |tp: &str| {
        let mut c = cfg(common::JWT_SECRET, common::MASTER_KEY);
        c.trusted_proxies = tp.split(',').map(|x| x.trim().to_string()).collect();
        c
    };
    for tp in ["127.0.0.1", "127.0.0.0/8", "::1", "0.0.0.0/0", "::/0"] {
        assert!(
            con(tp).validate_secrets().is_err(),
            "TRUSTED_PROXIES={} debe rechazarse",
            tp
        );
    }
    assert!(con("10.0.0.0/8").validate_secrets().is_ok());
    assert!(con("172.18.0.1").validate_secrets().is_ok());
}

#[test]
fn admin_se_enlaza_siempre_a_loopback() {
    let c = socket_core::config::AppConfig::from_env_reader(|k| match k {
        "HOST" => Some("0.0.0.0".to_string()),
        "PORT" => Some("3105".to_string()),
        "ADMIN_PORT" => Some("3015".to_string()),
        _ => None,
    });
    assert!(c.addr().starts_with("0.0.0.0:"));
    assert!(
        c.admin_addr().starts_with("127.0.0.1:"),
        "admin debe ser 127.0.0.1 aunque HOST sea 0.0.0.0: {}",
        c.admin_addr()
    );
    assert_eq!(c.admin_addr(), "127.0.0.1:3015");
}

#[test]
fn bootstrap_prohibido_en_produccion() {
    let mut c = cfg(common::JWT_SECRET, common::MASTER_KEY);
    c.allow_bootstrap = true;
    c.app_env = "prod".into();
    assert!(
        c.validate_secrets().is_err(),
        "ALLOW_BOOTSTRAP=true con APP_ENV=prod debe abortar el arranque"
    );
    let mut dev = cfg(common::JWT_SECRET, common::MASTER_KEY);
    dev.allow_bootstrap = true;
    dev.app_env = "dev".into();
    assert!(dev.validate_secrets().is_ok());
}

#[tokio::test]
async fn init_state_no_crea_bootstrap_en_produccion() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut c = cfg(common::JWT_SECRET, common::MASTER_KEY);
    c.allow_bootstrap = true;
    c.app_env = "prod".into();
    c.database_url = format!("sqlite:{}?mode=rwc", dir.path().join("p.db").display());
    let st = socket_core::init_state(&c).await;
    assert_eq!(st.keys.count().await.unwrap(), 0);
}
