// FASE 1 - Tests de aislamiento entre proyectos (items 1 y 2 del plan):
// A no debe crear/borrar/emitir/listar/ver nada de B. Canal ajeno = 404 (no 403).
mod common;

use common::{Hub, Project};
use serde_json::Value;

#[tokio::test]
async fn a_no_puede_crear_canal_en_prefijo_de_b() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "b:invasion").await;
    assert!(
        res.status() == 403 || res.status() == 404,
        "debería denegarse, status={}",
        res.status()
    );
}

#[tokio::test]
async fn a_no_puede_borrar_canal_de_b() {
    let hub = Hub::start().await;
    let (id_a, id_b) = hub.seed_channels().await;
    // Borrar por nombre
    let res = hub
        .client
        .delete(format!("{}/channels/b:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert!(
        res.status() == 404 || res.status() == 403,
        "debería denegarse, status={}",
        res.status()
    );

    // Borrar por id
    let res = hub
        .client
        .delete(format!("{}/channels/{}", hub.base, id_b))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert!(
        res.status() == 404 || res.status() == 403,
        "debería denegarse, status={}",
        res.status()
    );

    // Y el canal de B sigue existiendo para B
    let res = hub
        .client
        .get(format!("{}/channels/{}", hub.base, id_b))
        .header("x-api-key", &hub.key_b)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let _ = id_a;
}

#[tokio::test]
async fn a_no_puede_emitir_en_canal_de_b() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .emit_with_key(
            &hub.key_a,
            "b:chat",
            vec!["user-b".into()],
            serde_json::json!({"msg": "invasión"}),
        )
        .await;
    assert!(
        res.status() == 403 || res.status() == 404,
        "debería denegarse, status={}",
        res.status()
    );
}

#[tokio::test]
async fn segunda_emision_de_a_a_canal_de_b_tampoco_pasa() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .emit_with_key(
            &hub.key_a,
            "b:chat",
            vec!["*".into()],
            serde_json::json!({"x": 1}),
        )
        .await;
    assert!(
        res.status() == 403 || res.status() == 404,
        "debería denegarse, status={}",
        res.status()
    );
}

#[tokio::test]
async fn lista_de_canales_solo_muestra_propios() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: Value = res.json().await.unwrap();
    let list = v["data"].as_array().expect("array de canales");
    assert_eq!(list.len(), 1, "A solo debe ver sus canales; vió {:?}", list);
    assert_eq!(list[0]["name"].as_str(), Some("a:chat"));
}

#[tokio::test]
async fn canal_ajeno_da_404_no_403() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .client
        .get(format!("{}/channels/b:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        404,
        "existencia de canal ajeno NO debe filtrarse"
    );
}

#[tokio::test]
async fn clientes_de_canal_ajeno_da_404() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub.get_clients(&hub.key_a, "b:chat").await;
    assert_eq!(
        res.status(),
        404,
        "clients de canal ajeno NO debe exponerse"
    );
}

#[tokio::test]
async fn stats_filtradas_por_proyecto() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let res = hub
        .client
        .get(format!("{}/stats", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: Value = res.json().await.unwrap();
    let channels = v["data"]["channels"].as_u64().expect("channels");
    assert_eq!(
        channels, 1,
        "A debe ver solo su canal en stats; vió {}",
        channels
    );
}
