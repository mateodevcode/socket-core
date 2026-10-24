// FASE 4 - Ciclo de vida: órdenes de cierre activas (canal borrado, key revocada,
// key editada). Los WS deben cerrarse al instante vía bus de control.
mod common;

use common::{Hub, Project};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn borrar_canal_cierra_sus_ws() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn abierta");

    let res = hub
        .client
        .delete(format!("{}/channels/a:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(3)).await;
    assert!(
        cerrada,
        "borrar el canal debe cerrar sus conexiones explícitamente"
    );
}

#[tokio::test]
async fn admin_revoke_key_cierra_ws_abiertos() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn abierta");

    let res = hub
        .client
        .post(format!("{}/admin/keys/{}/revoke", hub.base, hub.project_a))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(3)).await;
    assert!(cerrada, "revocar por admin debe cerrar los WS al instante");
}

#[tokio::test]
async fn editar_prefijo_de_key_cierra_ws_abiertos() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn abierta");

    let res = hub
        .client
        .patch(format!("{}/admin/keys/{}", hub.base, hub.project_a))
        .header("x-master-key", common::MASTER_KEY)
        .json(&json!({"channel_prefix": "a:otro"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(3)).await;
    assert!(
        cerrada,
        "editar prefijo (seguridad) debe revalidar y cerrar WS"
    );
}

#[tokio::test]
async fn admin_revoke_jti_cierra_el_ws_de_ese_token() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let (jwt, jti) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn abierta");

    let res = hub
        .client
        .post(format!("{}/admin/tokens/revoke?jti={}", hub.base, jti))
        .header("x-master-key", common::MASTER_KEY)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(3)).await;
    assert!(cerrada, "revocar el jti por admin debe cerrar ese WS");
}
