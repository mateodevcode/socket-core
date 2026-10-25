// Fase 8.4 — endurecimiento de administración de keys/canales.
mod common;

use common::{Hub, MASTER_KEY};
use serde_json::{json, Value};

async fn post_key(hub: &Hub, body: Value) -> reqwest::Response {
    hub.client
        .post(format!("{}/admin/keys", hub.admin_base))
        .header("x-master-key", MASTER_KEY)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn patch_key(hub: &Hub, id: &str, body: Value) -> reqwest::Response {
    hub.client
        .patch(format!("{}/admin/keys/{}", hub.admin_base, id))
        .header("x-master-key", MASTER_KEY)
        .json(&body)
        .send()
        .await
        .unwrap()
}

// ---------- P1: borrar key borra canales (memoria+DB) y libera prefijo ----------

#[tokio::test]
async fn borrar_key_borra_canales_y_libera_prefijo() {
    let hub = Hub::start().await;
    // Proyecto desechable con canal.
    let k = post_key(
        &hub,
        json!({"name":"cascade","channel_prefix":"cascade:*","scopes":["publish","subscribe","manage"]}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    let raw = k["data"]["api_key"].as_str().unwrap().to_string();
    let id = k["data"]["id"].as_str().unwrap().to_string();
    let ch = hub
        .client
        .post(format!("{}/channels", hub.base))
        .header("x-api-key", &raw)
        .json(&json!({"name":"cascade:chat"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ch.status(), 201);

    let del = hub
        .client
        .delete(format!("{}/admin/keys/{}", hub.admin_base, id))
        .header("x-master-key", MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200);

    // El canal desaparece de la DB.
    let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channels WHERE project_id = ?")
        .bind(&id)
        .fetch_one(&hub.state.keys.pool)
        .await
        .unwrap();
    assert_eq!(n.0, 0, "no deben quedar canales de la key borrada");
    let huerfanos: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM channels WHERE project_id NOT IN (SELECT id FROM api_keys)",
    )
    .fetch_one(&hub.state.keys.pool)
    .await
    .unwrap();
    assert_eq!(huerfanos.0, 0, "no debe haber canales huérfanos");

    // Un proyecto nuevo con el mismo prefijo puede crear el mismo canal y no ve viejos.
    let k2 = post_key(
        &hub,
        json!({"name":"cascade2","channel_prefix":"cascade:*","scopes":["publish","subscribe","manage"]}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(k2["success"], true, "el prefijo quedó libre: {:?}", k2);
    let raw2 = k2["data"]["api_key"].as_str().unwrap().to_string();
    let ch2 = hub
        .client
        .post(format!("{}/channels", hub.base))
        .header("x-api-key", &raw2)
        .json(&json!({"name":"cascade:chat"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ch2.status(), 201, "el nombre de canal quedó libre");
    let list = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", &raw2)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);

    // Auditoría con recuento.
    let entries = hub.state.audit.snapshot();
    assert!(
        entries
            .iter()
            .any(|e| e.contains("key_delete") && e.contains("canales=1")),
        "auditoría debe indicar canales borrados: {:?}",
        entries
    );
}

// ---------- P2: nombre duplicado => 409 ----------

#[tokio::test]
async fn alta_key_nombre_duplicado_da_409() {
    let hub = Hub::start().await;
    let res = post_key(
        &hub,
        json!({"name":"proj-a","channel_prefix":"dup:*","scopes":["publish"]}),
    )
    .await;
    assert_eq!(res.status(), 409);
    let body = res.text().await.unwrap();
    assert!(
        !body.contains("UNIQUE") && !body.contains("sqlite"),
        "sin detalles de DB: {}",
        body
    );
}

#[tokio::test]
async fn edicion_key_nombre_duplicado_da_409() {
    let hub = Hub::start().await;
    let res = patch_key(&hub, &hub.project_b, json!({"name":"proj-a"})).await;
    assert_eq!(res.status(), 409);
}

// ---------- P3: expires_in_days ----------

#[tokio::test]
async fn expires_invalido_en_alta() {
    let hub = Hub::start().await;
    for (i, v) in [-1i64, 0, 3651, i64::MAX].into_iter().enumerate() {
        let res = post_key(
            &hub,
            json!({
                "name": format!("exp-bad-{i}"),
                "channel_prefix": format!("expb{i}:*"),
                "scopes": ["publish"],
                "expires_in_days": v
            }),
        )
        .await;
        assert_eq!(res.status(), 400, "expires_in_days={v} debe rechazarse");
    }
    let ok = post_key(
        &hub,
        json!({"name":"exp-ok","channel_prefix":"expo:*","scopes":["publish"],"expires_in_days":3650}),
    )
    .await;
    assert_eq!(ok.status(), 201);
}

#[tokio::test]
async fn expires_invalido_en_edicion_y_cero_quita_expiracion() {
    let hub = Hub::start().await;
    for v in [-1i64, 3651, i64::MAX] {
        let res = patch_key(&hub, &hub.project_a, json!({"expires_in_days": v})).await;
        assert_eq!(res.status(), 400, "edición expires={v} debe rechazarse");
    }
    let ok = patch_key(&hub, &hub.project_a, json!({"expires_in_days": 0})).await;
    assert_eq!(ok.status(), 200, "0 en edición = quitar expiración");
    let ok = patch_key(&hub, &hub.project_a, json!({"expires_in_days": 3650})).await;
    assert_eq!(ok.status(), 200);
}

// ---------- P4: scopes / prefijo / dominios ----------

#[tokio::test]
async fn scopes_invalidos_rechazados() {
    let hub = Hub::start().await;
    for (i, scopes) in [
        vec!["publish", "admin"],
        vec!["*"],
        vec!["publish", "publish"],
        vec!["otro"],
    ]
    .into_iter()
    .enumerate()
    {
        let res = post_key(
            &hub,
            json!({
                "name": format!("scope-bad-{i}"),
                "channel_prefix": format!("sb{i}:*"),
                "scopes": scopes
            }),
        )
        .await;
        assert_eq!(res.status(), 400, "scopes {:?} deben rechazarse", scopes);
    }
    let ok = post_key(
        &hub,
        json!({"name":"scope-ok","channel_prefix":"sok:*","scopes":["publish","subscribe","manage"]}),
    )
    .await;
    assert_eq!(ok.status(), 201);
    let bad_update = patch_key(&hub, &hub.project_a, json!({"scopes":["publish"]})).await;
    assert_eq!(bad_update.status(), 200, "scopes válidos en update");
    let bad_update = patch_key(&hub, &hub.project_a, json!({"scopes":["admin"]})).await;
    assert_eq!(bad_update.status(), 400);
}

#[tokio::test]
async fn channel_prefix_invalido_rechazado() {
    let hub = Hub::start().await;
    for (i, p) in ["mal prefijo:*", "a::x", ":x", "con espacio"]
        .into_iter()
        .enumerate()
    {
        let res = post_key(
            &hub,
            json!({"name":format!("pfx-bad-{i}"),"channel_prefix":p,"scopes":["publish"]}),
        )
        .await;
        assert_eq!(res.status(), 400, "prefijo {:?} debe rechazarse", p);
    }
    let ok = post_key(
        &hub,
        json!({"name":"pfx-ok","channel_prefix":"ok:*","scopes":["publish"]}),
    )
    .await;
    assert_eq!(ok.status(), 201);
}

#[tokio::test]
async fn domains_invalidos_rechazados() {
    let hub = Hub::start().await;
    for (i, d) in [
        vec!["panel.example.com"],
        vec!["ftp://panel.example.com"],
        vec!["https://"],
    ]
    .into_iter()
    .enumerate()
    {
        let res = post_key(
            &hub,
            json!({
                "name": format!("dom-bad-{i}"),
                "channel_prefix": format!("db{i}:*"),
                "scopes": ["publish"],
                "domains": d
            }),
        )
        .await;
        assert_eq!(res.status(), 400, "domains {:?} deben rechazarse", d);
    }
    let ok = post_key(
        &hub,
        json!({
            "name":"dom-ok",
            "channel_prefix":"dok:*",
            "scopes":["publish"],
            "domains":["https://panel.example.com","*.example.com","*"]
        }),
    )
    .await;
    assert_eq!(ok.status(), 201);
}

// ---------- P6: alta concurrente mismo prefijo ----------

#[tokio::test]
async fn alta_concurrente_mismo_prefijo_solo_una_gana() {
    let hub = Hub::start().await;
    let mut handles = Vec::new();
    for i in 0..8 {
        let client = hub.client.clone();
        let admin = hub.admin_base.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(format!("{}/admin/keys", admin))
                .header("x-master-key", MASTER_KEY)
                .json(&json!({
                    "name": format!("race-{i}"),
                    "channel_prefix": "race:*",
                    "scopes": ["publish"]
                }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut creadas = 0;
    for h in handles {
        if h.await.unwrap() == 201 {
            creadas += 1;
        }
    }
    assert_eq!(
        creadas, 1,
        "solo una alta concurrente puede ganar el prefijo"
    );
}

// ---------- Q8: SQLite BUSY => 503 + Retry-After ----------

#[tokio::test]
async fn alta_con_db_bloqueada_da_503_con_retry_after() {
    let hub = Hub::start().await;
    // Tomamos el lock de escritura en otra conexión del pool.
    let mut conn = hub.state.keys.pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .unwrap();

    let res = post_key(
        &hub,
        json!({"name":"busy-key","channel_prefix":"busy:*","scopes":["publish"]}),
    )
    .await;
    let tiene_retry = res.headers().get("retry-after").is_some();
    let status = res.status().as_u16();
    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;

    assert_eq!(status, 503, "DB bloqueada debe responder 503, no 500");
    assert!(tiene_retry, "503 debe incluir Retry-After");
}
