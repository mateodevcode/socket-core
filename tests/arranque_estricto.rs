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
