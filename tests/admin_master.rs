// FASE 1 - Item 13: /admin/* con MASTER_KEY (ausente/incorrecta/correcta).
// La comparación en tiempo constante se garantiza con la crate `subtle` en la fase
// de red y secretos; aquí fijamos el comportamiento funcional.
mod common;

use serde_json::json;

#[tokio::test]
async fn admin_sin_master_key_da_403() {
    let hub = common::Hub::start().await;
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.base))
        .send()
        .await
        .unwrap();
    let s = res.status().as_u16();
    assert!(
        s == 401 || s == 403,
        "sin master key no puede pasar ({})",
        s
    );
}

#[tokio::test]
async fn admin_con_master_incorrecta_da_401() {
    let hub = common::Hub::start().await;
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.base))
        .header("x-master-key", "incorrecta")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn admin_con_master_correcta_da_200() {
    let hub = common::Hub::start().await;
    let res = hub
        .client
        .get(format!("{}/admin/keys", hub.base))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: serde_json::Value = res.json().await.unwrap();
    let list = v["data"].as_array().expect("array");
    assert!(list.len() >= 2, "deben existir al menos las keys de A y B");
    for k in list {
        assert!(
            k.get("key_hash").is_none() && k.get("api_key").is_none(),
            "los metadatos de admin NO deben exponer la key cruda ni el hash"
        );
    }
}

#[tokio::test]
async fn admin_revoke_de_jti_funciona_con_master() {
    let hub = common::Hub::start().await;
    hub.seed_channels().await;
    let (jwt, jti) = hub.signed_claims("user-1", "a:chat", "both");
    let res = hub
        .client
        .post(format!("{}/admin/tokens/revoke?jti={}", hub.base, jti))
        .header("x-master-key", common::MASTER_KEY)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // El JWT revocado ya no sirve para publicar.
    let res = hub
        .emit_with_bearer(&jwt, "a:chat", vec!["*".into()], json!({"x": 1}))
        .await;
    assert_eq!(res.status(), 401, "jti revocado debe dar 401 al publicar");
}
