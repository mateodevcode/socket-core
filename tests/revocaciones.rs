// FASE 1 - Items 7, 8 y 15: revocación y ciclo de vida.
// JWT expirado -> 401; jti revocado -> 401 y WS se cierra; key revocada ->
// JWT public no vale y sus WS se cierran; fallo de DB al consultar revocación -> fail-closed.
mod common;

use common::{Hub, Project};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn jwt_expirado_da_401_y_ws_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = common::expired_jwt(&hub, "user-1", "a:chat", "both");
    let res = hub
        .emit_with_bearer(&jwt, "a:chat", vec!["*".into()], json!({"x": 1}))
        .await;
    assert_eq!(res.status(), 401, "JWT expirado debe dar 401");
    let res_ws = hub.ws_connect_raw(&jwt).await;
    assert!(res_ws.is_err(), "WS con JWT expirado debe rechazarse");
}

#[tokio::test]
async fn jti_revocado_cierra_ws_abierto() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let (jwt, jti) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn abierta");

    // Revoco el jti desde el estado del hub (vía admin)
    let ok = hub
        .state
        .keys
        .revoke_token(&jti, chrono::Utc::now().timestamp() + 600)
        .await;
    assert!(ok, "revocación de jti debe insertarse");

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(5)).await;
    assert!(
        cerrada,
        "el WS abierto debe CERRARSE al revocarse su jti (<= 5s)"
    );
}

#[tokio::test]
async fn key_revocada_inutiliza_jwt_de_inmediato() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;
    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");

    hub.state.keys.revoke(&hub.project_a).await;

    let res = hub
        .emit_with_bearer(&jwt, &id_a, vec!["*".into()], json!({"x": 1}))
        .await;
    assert_eq!(
        res.status(),
        401,
        "jwt de key revocada debe dejar de valer de inmediato"
    );

    // y una key revocada ya no autentica por REST
    let res = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401, "key revocada debe dar 401");
}

#[tokio::test]
async fn key_revocada_cierra_sus_ws_abiertos() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub
        .ws_open_handshake(&jwt, "a:chat", Some(json!({"client_id": "user-1"})))
        .await
        .expect("conn abierta");

    hub.state.keys.revoke(&hub.project_a).await;

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(5)).await;
    assert!(
        cerrada,
        "revocar la key debe cerrar sus WS abiertos (<= 5s)"
    );
}

#[tokio::test]
async fn fallo_de_db_en_revocacion_es_fail_closed() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;
    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");

    // Simulamos la caída de la DB cerrando el pool.
    hub.state.keys.pool.close().await;

    let res = hub
        .emit_with_bearer(&jwt, &id_a, vec!["*".into()], json!({"x": 1}))
        .await;
    let status = res.status().as_u16();
    assert!(
        status == 500,
        "si no se puede comprobar la revocación, DENIEGA (fail-closed); recibió {}",
        status
    );
}
