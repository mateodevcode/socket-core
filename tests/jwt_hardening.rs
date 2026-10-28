// B3 — Endurecimiento JWT: cada caso debe rechazarse (401) en REST y WS.
mod common;

use common::{Hub, Project};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

fn b64url(bytes: &[u8]) -> String {
    // base64url sin padding (suficiente para tokens de prueba)
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        }
    }
    out
}

fn claims_json(sub: &str, iss: Option<&str>, aud: Option<&str>, exp: i64) -> Value {
    let mut v = json!({
        "sub": sub,
        "project_id": "pid",
        "project_name": "pname",
        "channel": "a:chat",
        "role": "both",
        "jti": uuid::Uuid::new_v4().to_string(),
        "iat": chrono::Utc::now().timestamp(),
        "exp": exp,
    });
    let o = v.as_object_mut().unwrap();
    if let Some(i) = iss {
        o.insert("iss".into(), Value::String(i.into()));
    }
    if let Some(a) = aud {
        o.insert("aud".into(), Value::String(a.into()));
    }
    v
}

fn sign(claims: &Value, alg: Algorithm, secret: &str) -> String {
    encode(
        &Header::new(alg),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("firmar")
}

fn sign_with_real_claims(hub: &Hub, sub: &str, iss: &str, aud: &str, exp: i64) -> String {
    let mut c = claims_json(sub, Some(iss), Some(aud), exp);
    c["project_id"] = Value::String(hub.project_a.clone());
    c["project_name"] = Value::String("proj-a".into());
    sign(&c, Algorithm::HS256, common::JWT_SECRET)
}

/// Comprueba que un token no entra ni por REST (emit) ni por WS (upgrade).
async fn assert_rechazado(hub: &Hub, channel: &str, token: &str) {
    // Control positivo: el MISMO helper con un JWT válido debe funcionar (REST+WS),
    // para demostrar que el test no pasa por un fallo general del hub.
    let (valido, _) = hub.signed_claims("control-positivo", channel, "both");
    let ok = hub
        .emit_with_bearer(
            &valido,
            channel,
            vec!["*".into()],
            json!({"type": "chat", "text": "ok"}),
        )
        .await;
    assert_eq!(ok.status(), 200, "control positivo REST debe poder emitir");
    let mut ws_ok = hub
        .ws_connect_raw(&valido)
        .await
        .expect("control positivo WS debe conectar");
    {
        use futures_util::SinkExt;
        let _ = ws_ok
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
    }

    // Token bajo prueba: exactamente 401 y body JSON válido.
    let res = hub
        .emit_with_bearer(
            token,
            channel,
            vec!["*".into()],
            json!({"type": "chat", "text": "no"}),
        )
        .await;
    assert_eq!(
        res.status(),
        401,
        "REST debía rechazar el token (exactamente 401)"
    );
    let body: Value = res
        .json()
        .await
        .expect("el body del 401 debe ser JSON válido");
    assert!(
        body.get("success").is_some(),
        "respuesta de error inesperada: {:?}",
        body
    );
    let ws = hub.ws_connect_raw(token).await;
    assert!(ws.is_err(), "WS debía rechazar el upgrade");
}

#[tokio::test]
async fn jwt_alg_none_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let c = sign_with_real_claims(
        &hub,
        "u",
        "socket-core",
        "socket-core",
        chrono::Utc::now().timestamp() + 600,
    );
    let parts: Vec<&str> = c.split('.').collect();
    let none = format!("{}.{}.", b64url(br#"{"alg":"none","typ":"JWT"}"#), parts[1]);
    assert_rechazado(&hub, "a:chat", &none).await;
}

#[tokio::test]
async fn jwt_hs384_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let c = claims_json(
        "u",
        Some("socket-core"),
        Some("socket-core"),
        chrono::Utc::now().timestamp() + 600,
    );
    let tok = sign(&c, Algorithm::HS384, common::JWT_SECRET);
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn jwt_sin_aud_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let c = claims_json(
        "u",
        Some("socket-core"),
        None,
        chrono::Utc::now().timestamp() + 600,
    );
    let tok = sign(&c, Algorithm::HS256, common::JWT_SECRET);
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn jwt_aud_ajena_rechazada() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let tok = sign_with_real_claims(
        &hub,
        "u",
        "socket-core",
        "otra-audiencia",
        chrono::Utc::now().timestamp() + 600,
    );
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn jwt_iss_ajeno_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let tok = sign_with_real_claims(
        &hub,
        "u",
        "otro-emisor",
        "socket-core",
        chrono::Utc::now().timestamp() + 600,
    );
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn jwt_expirado_fuera_de_leeway_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let tok = sign_with_real_claims(
        &hub,
        "u",
        "socket-core",
        "socket-core",
        chrono::Utc::now().timestamp() - 3600, // muy pasado (leeway 5 s)
    );
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn jwt_firmado_con_otro_secreto_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let c = claims_json(
        "u",
        Some("socket-core"),
        Some("socket-core"),
        chrono::Utc::now().timestamp() + 600,
    );
    let tok = sign(
        &c,
        Algorithm::HS256,
        "otro-secreto-de-firma-32-caracteres!!",
    );
    assert_rechazado(&hub, "a:chat", &tok).await;
}

#[tokio::test]
async fn sc_live_como_token_rechazado() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    assert_rechazado(&hub, "a:chat", &hub.key_a).await;
}

// Evita warnings si el helper no se usa en algún caso.
#[allow(dead_code)]
fn _marker(_: Project) {}
