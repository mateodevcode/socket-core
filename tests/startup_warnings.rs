// Q7 — Al arrancar avisa (solo nombres) de scopes inválidos y prefijos solapados.
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

#[tokio::test]
async fn avisa_de_scopes_invalidos_y_prefijos_solapados_sin_secretos() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("warn.db");
    let url = format!("sqlite:{}?mode=rwc", db.display());
    let pool = socket_core::db::init_pool(&url).await.unwrap();
    let keys = socket_core::keys::KeyStore::new(pool);
    keys.create(
        "bad-scopes".into(),
        vec!["*".into()],
        "w1:*".into(),
        vec!["*".into()],
        60,
        None,
    )
    .await
    .unwrap();
    keys.create(
        "overlap-a".into(),
        vec!["*".into()],
        "dup:*".into(),
        vec!["publish".into()],
        60,
        None,
    )
    .await
    .unwrap();
    keys.create(
        "overlap-b".into(),
        vec!["*".into()],
        "dup:sub:*".into(),
        vec!["publish".into()],
        60,
        None,
    )
    .await
    .unwrap();

    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let cfg = socket_core::config::AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        admin_port: 0,
        database_url: url,
        jwt_secret: "warn-jwt-secret-32-chars-minimum!!!!!".into(),
        jwt_ttl_secs: 600,
        master_key: "warn-master-key-32-chars-minimum!!!!".into(),
        allowed_origins: vec![],
        app_env: "dev".into(),
        allow_bootstrap: false,
        trusted_proxies: vec![],
        shutdown_grace_secs: 10,
        ip_rate_per_min: 600,
    };
    let _state = socket_core::init_state(&cfg).await;

    let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).to_string();
    assert!(
        logs.contains("bad-scopes"),
        "debe nombrar la key con scopes inválidos: {}",
        logs
    );
    assert!(
        logs.to_lowercase().contains("scope"),
        "debe avisar de scopes inválidos: {}",
        logs
    );
    assert!(
        logs.contains("overlap-a") && logs.contains("overlap-b"),
        "debe nombrar las keys con prefijos solapados: {}",
        logs
    );
    assert!(!logs.contains("sc_live_"), "nunca debe loguear keys");
}
