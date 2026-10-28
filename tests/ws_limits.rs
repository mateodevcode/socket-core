// FASE 1 - Item 12: límites de recursos WS.
// Timeout de handshake, frame máximo, flood por segundo, máximo de conexiones por proyecto.
mod common;

use common::Hub;
use futures_util::SinkExt;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn ws_sin_handshake_se_cierra() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(common::Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let base = hub.base.replacen("http://", "ws://", 1);
    let url = format!("{}/ws?token={}", base, common::encode_query(&jwt));
    let (mut stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("upgrade ok");
    // NO envía handshake. El servidor debe cerrar en 5-10s.
    let cerrada = common::waits_closed(&mut stream, common::HANDSHAKE_CLOSE_TIMEOUT).await;
    assert!(
        cerrada,
        "el WS debe cerrar por timeout de handshake (<= 10s)"
    );
}

#[tokio::test]
async fn frame_muy_grande_se_cerra() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(common::Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let mut ws = hub
        .ws_open_handshake(&jwt, "a:chat", Some(json!({"client_id": "user-1"})))
        .await
        .expect("conn");
    // Payload de 80 KB (max 64 KB)
    let grande = json!({"type": "chat", "text": "x".repeat(80 * 1024)}).to_string();
    let _ = ws
        .send(tokio_tungstenite::tungstenite::Message::Text(grande))
        .await;
    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(3)).await;
    assert!(cerrada, "frame de 80KB debe provocar cierre (max 64KB)");
}

#[tokio::test]
async fn flood_de_mensajes_se_cierre() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub
        .ws_open_handshake(&jwt, "a:chat", Some(json!({"client_id": "user-1"})))
        .await
        .expect("conn");
    // Ráfaga de 200 publicaciones inmediatas (impl: límite >= 60/s con tolerancia > 100).
    for i in 0..200u32 {
        let msg = json!({"type": "chat", "seq": i}).to_string();
        // Si el envío falla (conexión ya cerrada por el servidor), el test pasa por ahí.
        if ws
            .send(tokio_tungstenite::tungstenite::Message::Text(msg))
            .await
            .is_err()
        {
            return; // ya cerró: bien
        }
    }
    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(5)).await;
    assert!(
        cerrada,
        "flood de publicación debe provocar cierre por rate limit"
    );
}

#[tokio::test]
async fn max_conexiones_por_proyecto() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    // Límite target: 50 conexiones por proyecto. Intento 60, al menos 10 deben rechazarse.
    let mut aceptadas = 0u32;
    let mut streams = std::collections::HashMap::new();
    for i in 0..60u32 {
        let jwt = hub
            .mint_jwt(
                common::Project::A,
                &format!("user-{}", i),
                "a:chat",
                "subscribe",
            )
            .await;
        if let Ok(s) = hub
            .ws_open_handshake(
                &jwt,
                "a:chat",
                Some(json!({"client_id": format!("user-{}", i)})),
            )
            .await
        {
            aceptadas += 1;
            streams.insert(i, s);
        }
        if aceptadas > 50 {
            break;
        }
    }
    streams.clear();
    assert!(
        aceptadas <= 50,
        "máximo de conexiones por proyecto debe imponerse (aceptadas={})",
        aceptadas
    );
}
