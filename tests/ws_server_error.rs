// S2 — Si la DB falla a mitad de sesión (watchdog), el WS cierra con
// "server_error" (reintentable), no con "credential_revoked".
mod common;

use common::{Hub, Project};
use std::time::Duration;

#[tokio::test]
async fn pool_cerrado_a_mitad_de_sesion_cierra_con_server_error() {
    let limits = socket_core::limits::Limits {
        auth_recheck_secs: 1,
        ..Default::default()
    };
    let hub = Hub::start_with_limits(limits).await;
    hub.seed_channels().await;
    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("ws abierto");

    // Simulamos caída de DB a mitad de sesión.
    hub.state.keys.pool.close().await;

    let mut code: Option<String> = None;
    for _ in 0..15 {
        if let Some(v) = common::next_text(&mut ws, Duration::from_secs(1)).await {
            if v["type"] == "error" {
                code = v["code"].as_str().map(|s| s.to_string());
                break;
            }
        }
    }
    assert_eq!(
        code.as_deref(),
        Some("server_error"),
        "una caída de DB debe ser reintentable (server_error), no revoked"
    );
    let _ = Project::A;
}
