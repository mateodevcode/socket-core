// FASE 1 - Items 3 y 4: el token sc_live_ NO debe abrir WS (puente service-key eliminado)
// ni publicar por REST con Bearer. API key de otro proyecto tampoco.
mod common;

use common::Hub;
use serde_json::json;

#[tokio::test]
async fn ws_rechaza_api_key_cruda_como_token() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    // Con la raw key como token en query
    match hub.ws_connect_raw(&hub.key_a).await {
        Err(status) => assert!(
            status == 401 || status == 403 || status == 0,
            "sc_live_ debe rechazarse en WS, status={}",
            status
        ),
        Ok(mut stream) => {
            // Si el upgrade pasara, el handshake debe fracasar: nadie autentica con API key por WS.
            let hs = json!({"channel": "a:chat", "client_id": "x"}).to_string();
            use futures_util::SinkExt;
            let _ = stream
                .send(tokio_tungstenite::tungstenite::Message::Text(hs))
                .await;
            let msg = common::next_text(&mut stream, std::time::Duration::from_secs(2)).await;
            let rechazado = match &msg {
                None => true,
                Some(m) => m.get("error").is_some(),
            };
            assert!(
                rechazado,
                "la conexión con sc_live_ NO debe quedar suscrita: {:?}",
                msg
            );
        }
    }
}

#[tokio::test]
async fn events_rechaza_bearer_con_api_key_cruda() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .emit_with_bearer(&hub.key_a, "a:chat", vec!["u".into()], json!({"x": 1}))
        .await;
    let status = res.status().as_u16();
    assert!(
        status == 401 || status == 403,
        "Bearer con sc_live_ debe rechazarse en REST ({})",
        status
    );
}

#[tokio::test]
async fn events_rechaza_api_key_de_otro_proyecto() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .emit_with_key(&hub.key_b, "a:chat", vec!["u".into()], json!({"x": 1}))
        .await;
    let status = res.status().as_u16();
    assert!(
        status == 403 || status == 404,
        "API key de B en canal de A debe denegarse ({})",
        status
    );
}

#[tokio::test]
async fn api_key_de_otro_proyecto_no_puede_ver_canal_ajeno() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .client
        .get(format!("{}/channels/b:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn ws_rechaza_token_basura() {
    let hub = Hub::start().await;
    match hub.ws_connect_raw("not-a-token").await {
        Err(_status) => {} // rechazado: correcto
        Ok(_stream) => panic!("un token cualquiera no debe abrir WS"),
    }
}

#[tokio::test]
async fn ws_rechaza_sin_token() {
    let hub = Hub::start().await;
    let base = hub.base.replacen("http://", "ws://", 1);
    let url = format!("{}/ws", base);
    match tokio_tungstenite::connect_async(url).await {
        Err(_) => {}
        Ok(_) => panic!("WS sin token no debe abrirse"),
    }
}
