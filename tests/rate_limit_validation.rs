// M2 — rate_limit_per_min solo válido en [1, 100000] al crear y editar keys.
mod common;

use common::Hub;
use serde_json::json;

async fn crear(hub: &Hub, name: &str, prefix: &str, rate: i64) -> reqwest::Response {
    hub.client
        .post(format!("{}/admin/keys", hub.admin_base))
        .header("x-master-key", common::MASTER_KEY)
        .json(&json!({
            "name": name,
            "channel_prefix": prefix,
            "scopes": ["publish","subscribe"],
            "rate_limit_per_min": rate
        }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn alta_rechaza_rate_fuera_de_rango() {
    let hub = Hub::start().await;
    for (i, v) in [0i64, -1, 100001].into_iter().enumerate() {
        let res = crear(&hub, &format!("bad-{i}"), &format!("bad{i}:*"), v).await;
        assert_eq!(res.status(), 400, "rate {v} debe rechazarse");
    }
}

#[tokio::test]
async fn alta_acepta_limites_validos() {
    let hub = Hub::start().await;
    let r1 = crear(&hub, "min-ok", "min:*", 1).await;
    assert_eq!(r1.status(), 201);
    let r2 = crear(&hub, "max-ok", "max:*", 100000).await;
    assert_eq!(r2.status(), 201);
}

#[tokio::test]
async fn edicion_rechaza_rate_fuera_de_rango() {
    let hub = Hub::start().await;
    for (i, v) in [0i64, -5, 100001].into_iter().enumerate() {
        let res = hub
            .admin_patch_key(&hub.project_a, json!({ "rate_limit_per_min": v }))
            .await;
        assert_eq!(
            res.status(),
            400,
            "update rate {v} debe rechazarse (caso {i})"
        );
    }
    let ok = hub
        .admin_patch_key(&hub.project_a, json!({ "rate_limit_per_min": 100000 }))
        .await;
    assert_eq!(ok.status(), 200);
}
