// FASE 4.1 - Tests PRIMERO para los hallazgos pendientes:
// 1) scope/rol exacto en mint, 2) scopes vivos en authz, 3) solape de prefijos,
// 4) validación de sub/nombre/description, 5) propagación de errores DB + creación
// atómica, 6) límite agregado de publish por proyecto + watchdog configurable.
mod common;

use common::{Hub, Project};
use futures_util::SinkExt;
use serde_json::{json, Value};
use std::time::Duration;

async fn raw_key_of(res: reqwest::Response) -> String {
    let v: Value = res.json().await.expect("json key");
    v["data"]["api_key"].as_str().expect("api_key").to_string()
}

// ---------- 1) mint_token: rol exacto con scopes ----------

#[tokio::test]
async fn mint_role_both_rechazado_con_scope_solo_subscribe() {
    let hub = Hub::start().await;
    let raw = raw_key_of(
        hub.admin_create_key("sub-only", "c:*", &["subscribe"])
            .await,
    )
    .await;
    let res = hub
        .mint_with_key(
            &raw,
            json!({"user_id": "u1", "channel": "c:x", "role": "both"}),
        )
        .await;
    assert_eq!(
        res.status(),
        403,
        "'both' exige AMBOS scopes; con solo subscribe debe dar 403"
    );
}

#[tokio::test]
async fn mint_role_publish_rechazado_con_scope_solo_subscribe() {
    let hub = Hub::start().await;
    let raw = raw_key_of(
        hub.admin_create_key("sub-only2", "d:*", &["subscribe"])
            .await,
    )
    .await;
    let res = hub
        .mint_with_key(
            &raw,
            json!({"user_id": "u1", "channel": "d:x", "role": "publish"}),
        )
        .await;
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn mint_role_subscribe_ok_con_scope_solo_subscribe() {
    let hub = Hub::start().await;
    let raw = raw_key_of(
        hub.admin_create_key("sub-only3", "e:*", &["subscribe"])
            .await,
    )
    .await;
    let res = hub
        .mint_with_key(
            &raw,
            json!({"user_id": "u1", "channel": "e:x", "role": "subscribe"}),
        )
        .await;
    assert_eq!(res.status(), 200);
}

// ---------- 2) scopes vivos en authorize_channel_for_jwt ----------

#[tokio::test]
async fn jwt_viejo_no_publica_tras_quitar_scope_publish() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;
    let jwt = hub.mint_jwt(Project::A, "user-1", "a:chat", "both").await;

    // Se recorta la key a solo subscribe (los JWT viejos deben morir para publish).
    let res = hub
        .admin_patch_key(&hub.project_a, json!({"scopes": ["subscribe"]}))
        .await;
    assert_eq!(res.status(), 200, "PATCH de scopes debe funcionar");

    let res = hub
        .emit_with_bearer(&jwt, &id_a, vec!["*".into()], json!({"x": 1}))
        .await;
    assert_eq!(
        res.status(),
        403,
        "sin scope publish vivo, el JWT viejo no publica"
    );
}

#[tokio::test]
async fn ws_publicacion_denegada_tras_quitar_scope_publish() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub.mint_jwt(Project::A, "user-1", "a:chat", "both").await;

    let res = hub
        .admin_patch_key(&hub.project_a, json!({"scopes": ["subscribe"]}))
        .await;
    assert_eq!(res.status(), 200);

    // Conexión de solo lectura con el JWT viejo (subscribe sigue permitido).
    let mut ws = hub
        .ws_open(&jwt, "a:chat")
        .await
        .expect("puede suscribirse: conserva scope subscribe");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type": "chat", "text": "no debería pasar"}).to_string(),
    ))
    .await
    .expect("send");

    let mut denegado = false;
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut ws, Duration::from_secs(2)).await {
            let code = v["code"].as_str().unwrap_or("");
            if v["type"] == "error" && (code == "forbidden" || code == "credential_revoked") {
                denegado = true;
                break;
            }
        }
    }
    if !denegado {
        denegado = common::waits_closed(&mut ws, Duration::from_secs(2)).await;
    }
    assert!(
        denegado,
        "publicar sin scope publish vivo debe denegarse y cerrar/informar"
    );
}

// ---------- 3) solape de prefijos ----------

#[tokio::test]
async fn admin_create_key_con_prefijo_solapado_da_400() {
    let hub = Hub::start().await;
    // A ya usa "a:*"
    let res = hub.admin_create_key("dup-a", "a:*", &["publish"]).await;
    assert_eq!(res.status(), 400, "prefijo a:* solapa con la key existente");
    let body = res.text().await.unwrap();
    assert!(
        body.contains("proj-a"),
        "el error debe indicar con qué key solapa: {}",
        body
    );
}

#[tokio::test]
async fn admin_create_key_wildcard_solapado_da_400() {
    let hub = Hub::start().await;
    let res = hub.admin_create_key("dup-all", "*", &["publish"]).await;
    assert_eq!(
        res.status(),
        400,
        "'*' solapa con cualquier prefijo existente"
    );
}

#[tokio::test]
async fn admin_update_key_con_prefijo_solapado_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .admin_patch_key(&hub.project_a, json!({"channel_prefix": "b:*"}))
        .await;
    assert_eq!(res.status(), 400, "mover A a b:* solapa con B");
    let body = res.text().await.unwrap();
    assert!(
        body.contains("proj-b"),
        "el error debe indicar con qué key solapa: {}",
        body
    );
}

#[tokio::test]
async fn prefijo_de_key_revocada_sigue_ocupado_hasta_borrarla() {
    let hub = Hub::start().await;
    // Creamos una key aparte con su propio prefijo.
    let res = hub.admin_create_key("rev-pref", "z:*", &["publish"]).await;
    assert_eq!(res.status(), 201);
    let body: Value = res.json().await.unwrap();
    let id = body["data"]["id"].as_str().unwrap().to_string();

    // Revocarla: el prefijo sigue ocupado (el solape cuenta revocadas).
    let res = hub
        .client
        .post(format!("{}/admin/keys/{}/revoke", hub.admin_base, id))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = hub.admin_create_key("nueva-z", "z:*", &["publish"]).await;
    assert_eq!(res.status(), 400);
    let body = res.text().await.unwrap();
    assert!(
        body.contains("rev-pref"),
        "debe nombrar la key que ocupa el prefijo: {}",
        body
    );
    assert!(
        body.to_lowercase().contains("revocada"),
        "debe avisar de que revocadas cuentan: {}",
        body
    );

    // Borrar la key revocada libera el prefijo.
    let res = hub
        .client
        .delete(format!("{}/admin/keys/{}", hub.admin_base, id))
        .header("x-master-key", common::MASTER_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = hub.admin_create_key("nueva-z2", "z:*", &["publish"]).await;
    assert_eq!(
        res.status(),
        201,
        "tras borrar la key, el prefijo queda libre"
    );
}

#[tokio::test]
async fn admin_update_key_con_su_propio_prefijo_ok() {
    let hub = Hub::start().await;
    let res = hub
        .admin_patch_key(&hub.project_a, json!({"channel_prefix": "a:*"}))
        .await;
    assert_eq!(res.status(), 200, "repetir su propio prefijo no es solape");
}

// ---------- 4) validación de sub / canal / description ----------

#[tokio::test]
async fn mint_sin_user_id_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .mint_request(Project::A, json!({"channel": "a:chat", "role": "both"}))
        .await;
    assert_eq!(res.status(), 400, "sub (user_id) es obligatorio");
}

#[tokio::test]
async fn mint_sub_con_asterisco_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .mint_request(
            Project::A,
            json!({"user_id": "user*", "channel": "a:chat", "role": "both"}),
        )
        .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn mint_sub_con_dos_puntos_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .mint_request(
            Project::A,
            json!({"user_id": "user:1", "channel": "a:chat", "role": "both"}),
        )
        .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn mint_sub_demasiado_largo_da_400() {
    let hub = Hub::start().await;
    let sub = "u".repeat(129);
    let res = hub
        .mint_request(
            Project::A,
            json!({"user_id": sub, "channel": "a:chat", "role": "both"}),
        )
        .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn mint_sub_con_charset_invalido_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .mint_request(
            Project::A,
            json!({"user_id": "user name", "channel": "a:chat", "role": "both"}),
        )
        .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn crear_canal_con_nombre_uuid_da_400() {
    let hub = Hub::start().await;
    let uuid = uuid::Uuid::new_v4().to_string();
    let res = hub.create_channel_with_name(Project::A, &uuid).await;
    assert_eq!(res.status(), 400, "un UUID no es un nombre de canal válido");
}

#[tokio::test]
async fn crear_canal_nombre_demasiado_largo_da_400() {
    let hub = Hub::start().await;
    let name = format!("a:{}", "x".repeat(127)); // 129 chars
    let res = hub.create_channel_with_name(Project::A, &name).await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn crear_canal_con_caracteres_invalidos_da_400() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "a:ch at").await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn crear_canal_con_segmento_vacio_da_400() {
    let hub = Hub::start().await;
    for name in ["a:", "a::x", ":a", "a:"] {
        let res = hub.create_channel_with_name(Project::A, name).await;
        assert_eq!(res.status(), 400, "nombre inválido: {}", name);
    }
}

#[tokio::test]
async fn crear_canal_con_description_larga_da_400() {
    let hub = Hub::start().await;
    let res = hub
        .client
        .post(format!("{}/channels", hub.base))
        .header("x-api-key", &hub.key_a)
        .json(&json!({"name": "a:desc", "description": "d".repeat(501)}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

// ---------- 5) propagación de errores de DB ----------

#[tokio::test]
async fn api_key_con_db_caida_da_500_no_401() {
    let hub = Hub::start().await;
    hub.state.keys.pool.close().await;
    let res = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500, "DB caída al validar key => 500, no 401");
}

#[tokio::test]
async fn persist_db_caida_da_500_y_no_queda_canal() {
    let hub = Hub::start().await;
    hub.drop_channels_table().await;
    let res = hub.create_channel_with_name(Project::A, "a:nuevo").await;
    assert_eq!(
        res.status(),
        500,
        "si no se puede persistir, no se crea (500)"
    );

    // No debe quedar en memoria (rollback).
    let res = hub
        .client
        .get(format!("{}/channels", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    let v: Value = res.json().await.unwrap();
    let names: Vec<&str> = v["data"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"a:nuevo"),
        "el canal no debe quedar tras fallo: {:?}",
        names
    );
}

#[tokio::test]
async fn delete_channel_db_caida_da_500_y_sigue_el_canal() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    hub.drop_channels_table().await;
    let res = hub
        .client
        .delete(format!("{}/channels/a:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500, "si la DB falla al borrar, 500");
    let res = hub
        .client
        .get(format!("{}/channels/a:chat", hub.base))
        .header("x-api-key", &hub.key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        200,
        "el canal sigue operativo tras fallar el borrado"
    );
}

#[tokio::test]
async fn load_persisted_propaga_error_de_db() {
    let hub = Hub::start().await;
    hub.drop_channels_table().await;
    let res = hub
        .state
        .channel_manager
        .load_persisted(&hub.state.keys.pool)
        .await;
    assert!(
        res.is_err(),
        "load_persisted debe propagar el error (no unwrap_or_default)"
    );
}

#[tokio::test]
async fn crear_canal_con_conflicto_en_db_da_500() {
    let hub = Hub::start().await;
    // Fila en DB que no está en memoria (simula otra instancia/estado inconsistente).
    sqlx::query(
        "INSERT INTO channels (id, name, project_id, description, created_at) VALUES (?,?,?,?,?)",
    )
    .bind("pre-existente")
    .bind("a:conflicto")
    .bind(&hub.project_a)
    .bind(Option::<String>::None)
    .bind(chrono::Utc::now().timestamp())
    .execute(&hub.state.keys.pool)
    .await
    .unwrap();

    let res = hub
        .create_channel_with_name(Project::A, "a:conflicto")
        .await;
    assert_eq!(
        res.status(),
        500,
        "conflicto de unicidad en DB => 500 (no 201 mentiroso)"
    );
}

#[tokio::test]
async fn creacion_concurrente_del_mismo_nombre_solo_uno_gana() {
    let hub = Hub::start().await;
    let mut handles = Vec::new();
    for _ in 0..16 {
        let client = hub.client.clone();
        let base = hub.base.clone();
        let key = hub.key_a.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(format!("{}/channels", base))
                .header("x-api-key", key)
                .json(&json!({"name": "a:race"}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut creados = 0;
    for h in handles {
        if h.await.unwrap() == 201 {
            creados += 1;
        }
    }
    assert_eq!(
        creados, 1,
        "la creación debe ser atómica: exactamente un 201"
    );
}

// ---------- 6) límite agregado por proyecto + watchdog ----------

#[tokio::test]
async fn publish_agregado_por_proyecto_se_limita() {
    let limits = socket_core::limits::Limits {
        publish_per_sec_conn: 100,
        publish_per_sec_project: 3,
        ..Default::default()
    };
    let hub = Hub::start_with_limits(limits).await;
    hub.seed_channels().await;

    let jwt1 = hub.mint_jwt(Project::A, "user-1", "a:chat", "both").await;
    let jwt2 = hub.mint_jwt(Project::A, "user-2", "a:chat", "both").await;
    let mut c1 = hub.ws_open(&jwt1, "a:chat").await.expect("c1");
    let mut c2 = hub.ws_open(&jwt2, "a:chat").await.expect("c2");

    // 2 + 2 publicaciones: la cuarta excede el límite AGREGADO (3/s) del proyecto.
    let mut cerrada = false;
    for (conn, n) in [(&mut c1, 2u32), (&mut c2, 2u32)] {
        for i in 0..n {
            let msg = json!({"type": "chat", "seq": i}).to_string();
            if conn
                .send(tokio_tungstenite::tungstenite::Message::Text(msg))
                .await
                .is_err()
            {
                cerrada = true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Al menos una de las conexiones debe recibir rate_limited o cerrarse.
    let mut visto_rate_limited = false;
    for conn in [&mut c1, &mut c2] {
        for _ in 0..6 {
            if let Some(v) = common::next_text(conn, Duration::from_secs(1)).await {
                if v["code"].as_str() == Some("rate_limited") {
                    visto_rate_limited = true;
                    break;
                }
            }
        }
    }
    let cerrada = cerrada
        || common::waits_closed(&mut c1, Duration::from_secs(2)).await
        || common::waits_closed(&mut c2, Duration::from_secs(2)).await;
    assert!(
        visto_rate_limited || cerrada,
        "el límite AGREGADO por proyecto debe cortar (rate_limited o cierre)"
    );
}

#[test]
fn watchdog_por_defecto_es_10_segundos() {
    assert_eq!(socket_core::limits::Limits::default().auth_recheck_secs, 10);
}

#[tokio::test]
async fn watchdog_configurable_cierra_por_cambio_directo_en_db() {
    let limits = socket_core::limits::Limits {
        auth_recheck_secs: 1,
        ..Default::default()
    };
    let hub = Hub::start_with_limits(limits).await;
    hub.seed_channels().await;
    let (jwt, jti) = hub.signed_claims("user-1", "a:chat", "both");
    let mut ws = hub.ws_open(&jwt, "a:chat").await.expect("conn");

    // Cambio directo en DB (sin bus de control): solo el watchdog puede detectarlo.
    sqlx::query("INSERT OR IGNORE INTO revoked_tokens (jti, exp) VALUES (?, ?)")
        .bind(&jti)
        .bind(chrono::Utc::now().timestamp() + 600)
        .execute(&hub.state.keys.pool)
        .await
        .unwrap();

    let cerrada = common::waits_closed(&mut ws, Duration::from_secs(4)).await;
    assert!(
        cerrada,
        "el watchdog (configurado a 1s) debe cerrar la conexión"
    );
}
