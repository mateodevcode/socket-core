// FASE 1 - Item 6: roles JWT.
// subscribe no puede publicar (REST ni WS); JWT de un canal no entra en otro;
// publish-only no puede suscribirse.
mod common;

use common::{Hub, Project};
use futures_util::SinkExt;
use serde_json::json;

#[tokio::test]
async fn jwt_subscribe_no_puede_publicar_rest() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let res = hub
        .emit_with_bearer(&jwt, &id_a, vec!["user-1".into()], json!({"x": 1}))
        .await;
    assert_eq!(
        res.status(),
        403,
        "JWT subscribe no puede publicar por REST"
    );
}

#[tokio::test]
async fn jwt_subscribe_no_puede_publicar_ws() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt_sub = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let jwt_peer = hub
        .mint_jwt(Project::A, "user-2", "a:chat", "subscribe")
        .await;

    let mut peer = hub
        .ws_open_handshake(&jwt_peer, "a:chat", Some(json!({"client_id": "peer"})))
        .await
        .expect("peer entra");
    let mut pub_conn = hub
        .ws_open_handshake(&jwt_sub, "a:chat", Some(json!({"client_id": "pub"})))
        .await
        .expect("conexión con JWT subscribe debe entrar (rol válido para suscribir)");

    // El publish-only intenta publicar por WS
    let _ = pub_conn
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type": "chat", "text": "hola"}).to_string(),
        ))
        .await;

    // El peer NO debe recibir el mensaje del suscriptor sin rol publish
    let respuesta_peer = common::next_text(&mut peer, std::time::Duration::from_secs(2)).await;
    if let Some(v) = &respuesta_peer {
        assert_ne!(
            v["payload"]["type"].as_str(),
            Some("chat"),
            "un JWT subscribe no debe poder publish por WS"
        );
    }

    // Target target: además, el autor debe recibir un evento de error explícito.
    let respuesta_pub = common::next_text(&mut pub_conn, std::time::Duration::from_secs(2)).await;
    let hay_error = respuesta_pub
        .as_ref()
        .map(|v| v.get("error").or_else(|| v.get("payload")).is_some())
        .unwrap_or(false);
    assert!(
        hay_error,
        "el publish denegado debe informar un error al autor: {:?}",
        respuesta_pub
    );
}

#[tokio::test]
async fn jwt_de_un_canal_no_entra_en_otro() {
    let hub = Hub::start().await;
    let _id1 = hub.create_channel_ok(Project::A, "a:chat1").await;
    let _id2 = hub.create_channel_ok(Project::A, "a:chat2").await;
    let jwt = hub.mint_jwt(Project::A, "user-1", "a:chat1", "both").await;
    // Intenta entrar a a:chat2 con JWT acotado a a:chat1
    let res = hub
        .ws_open_handshake(&jwt, "a:chat2", Some(json!({"client_id": "user-1"})))
        .await;
    assert!(
        res.is_err(),
        "JWT de a:chat1 no puede suscribirse a a:chat2"
    );
}

#[tokio::test]
async fn jwt_publish_only_no_puede_suscribirse_ws() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "publish")
        .await;
    let res = hub
        .ws_open_handshake(&jwt, "a:chat", Some(json!({"client_id": "user-1"})))
        .await;
    assert!(
        res.is_err(),
        "JWT publish-only no puede abrir WS (subscribe)"
    );
}

#[tokio::test]
async fn jwt_both_puede_suscribir_y_publicar() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let marca_ambos = hub.mint_jwt(Project::A, "user-1", "a:chat", "both").await;
    // Peer para poder recibir
    let peer = hub
        .mint_jwt(Project::A, "user-2", "a:chat", "subscribe")
        .await;
    let mut peer_ws = hub
        .ws_open_handshake(&peer, "a:chat", Some(json!({"client_id": "peer"})))
        .await
        .expect("peer");
    let mut ws = hub
        .ws_open_handshake(&marca_ambos, "a:chat", Some(json!({"client_id": "autor"})))
        .await
        .expect("both entra");

    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type": "chat", "text": "hola ambos"}).to_string(),
    ))
    .await
    .expect("send");

    // El peer debe recibir el evento publicado (rol both puede publicar)
    let mut visto = false;
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut peer_ws, std::time::Duration::from_secs(2)).await {
            if v["payload"]["type"] == "chat" {
                visto = true;
                break;
            }
        }
    }
    let _ = ws; // keep alive
    assert!(visto, "rol both debe poder publicar por WS");
}
