// FASE 1 - Items 9 y 10: identidad del servidor (sub del JWT), presence sin
// suplantación ("user-1" no pisada por un tercero) y eventos de sistema reservados.
mod common;

use common::{Hub, Project};
use futures_util::SinkExt;
use serde_json::{json, Value};

#[tokio::test]
async fn dos_conexiones_del_mismo_usuario_reciben_mensaje_dirigido() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt1 = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let jwt2 = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;

    let mut c1 = hub.ws_open(&jwt1, "a:chat").await.expect("conn 1");
    let mut c2 = hub.ws_open(&jwt2, "a:chat").await.expect("conn 2");
    let _c3_tcp_mark = 0;

    let res = hub
        .emit_with_key(
            &hub.key_a,
            "a:chat",
            vec!["user-1".into()],
            json!({"type": "chat", "text": "hola user-1"}),
        )
        .await;
    assert_eq!(res.status(), 200);

    let mut recibidos = 0;
    for conn in [&mut c1, &mut c2] {
        for _ in 0..5 {
            if let Some(v) = common::next_text(conn, std::time::Duration::from_secs(2)).await {
                if v["payload"]["type"] == "chat" && v["payload"]["text"] == "hola user-1" {
                    recibidos += 1;
                    break;
                }
            }
        }
    }
    assert_eq!(
        recibidos, 2,
        "AMBAS conexiones del mismo usuario deben recibirlo (recibidos={})",
        recibidos
    );
}

#[tokio::test]
async fn tercero_con_identidad_declarada_no_suplanta_ni_recibe() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt1 = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let jwt_invasor = hub
        .mint_jwt(Project::A, "user-invasor", "a:chat", "subscribe")
        .await;

    let mut c1 = hub.ws_open(&jwt1, "a:chat").await.expect("conn user-1");

    // El invasor INTENTA declarar client_id = "user-1" en el handshake.
    // Target: el servidor ignora esa declaración (identidad = sub del JWT).
    let res = hub
        .ws_open_handshake(&jwt_invasor, "a:chat", Some(json!({"client_id": "user-1"})))
        .await;
    let mut invasor = match res {
        Ok(s) => s,
        Err(c) => panic!(
            "el invasor (usuario legítimo propio) debería poder entrar sin suplantar: {}",
            c
        ),
    };

    // Mensaje dirigido a user-1: c1 debe recibirlo, el invasor NO.
    let res = hub
        .emit_with_key(
            &hub.key_a,
            "a:chat",
            vec!["user-1".into()],
            json!({"type": "chat", "text": "secreto"}),
        )
        .await;
    assert_eq!(res.status(), 200);

    let mut visto_en_c1 = false;
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut c1, std::time::Duration::from_secs(2)).await {
            if v["payload"]["text"] == "secreto" {
                visto_en_c1 = true;
                break;
            }
        }
    }
    assert!(visto_en_c1, "user-1 debe recibir su mensaje dirigido");

    let visto_en_invasor = common::next_text(&mut invasor, std::time::Duration::from_secs(2)).await;
    if let Some(v) = &visto_en_invasor {
        assert_ne!(
            v["payload"]["text"],
            Value::from("secreto"),
            "el invasor NO debe recibir mensajes dirigidos a user-1"
        );
    }
}

#[tokio::test]
async fn presence_no_pisa_identidad_de_otros() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt1 = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    hub.mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let jwt_invasor = hub
        .mint_jwt(Project::A, "user-invasor", "a:chat", "subscribe")
        .await;

    let _c1 = hub.ws_open(&jwt1, "a:chat").await.expect("c1");
    let _c1b = hub
        .ws_open(&jwt1, "a:chat")
        .await
        .expect("c1 segunda conexión");
    let res_inv = hub
        .ws_open_handshake(&jwt_invasor, "a:chat", Some(json!({"client_id": "user-1"})))
        .await;
    assert!(res_inv.is_ok(), "invasor puede entrar con SU identidad");

    // Presencia: 3 conexiones, identidad por sub: user-1 x2, user-invasor x1.
    let clients_res = hub.get_clients(&hub.key_a, "a:chat").await;
    assert_eq!(clients_res.status(), 200);
    let v: Value = clients_res.json().await.unwrap();
    let list = v["data"].as_array().expect("array");
    assert_eq!(
        list.len(),
        3,
        "deben verse las 3 conexiones (identidad = sub): {:?}",
        list
    );
    let ids: Vec<&str> = list.iter().filter_map(|c| c["id"].as_str()).collect();
    assert_eq!(
        ids.iter().filter(|i| **i == "user-1").count(),
        2,
        "user-1 x2, IDs={:?}",
        ids
    );
    assert!(
        ids.contains(&"user-invasor"),
        "invasor con su propia identidad, IDs={:?}",
        ids
    );
}

#[tokio::test]
async fn source_system_y_tipos_de_sistema_reservados() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;
    let peer_jwt = hub
        .mint_jwt(Project::A, "user-2", "a:chat", "subscribe")
        .await;
    let mut peer = hub.ws_open(&peer_jwt, "a:chat").await.expect("peer");

    let (jwt, _) = hub.signed_claims("user-1", "a:chat", "both");

    // REST: source __system__ debe rechazarse o sobrescribirse.
    let res = hub
        .client
        .post(format!("{}/channels/{}/events", hub.base, id_a))
        .header("x-api-key", &hub.key_a)
        .json(&json!({
            "source": "__system__",
            "targets": ["*"],
            "payload": {"type": "client_joined", "client_id": "suplantación"}
        }))
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    assert!(
        status == 403 || status == 400,
        "source __system__ debe rechazarse para clientes ({})",
        status
    );

    // WS: payload.type de sistema debe rechazarse o sobrescribirse.
    let (jwt_ws, _) = hub.signed_claims("user-1", "a:chat", "both");
    let mut autor = hub.ws_open(&jwt_ws, "a:chat").await.expect("autor");
    let _ = jwt;
    autor
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type": "client_joined", "x": 1}).to_string(),
        ))
        .await
        .expect("send ws");

    // Ningún receptor debe ver nada que diga ser un evento de sistema del cliente.
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut peer, std::time::Duration::from_secs(2)).await {
            let es_join_del_cliente = v["payload"].get("type").and_then(Value::as_str)
                == Some("client_joined")
                && v["source"].as_str() != Some("__system__")
                && v["payload"].get("x").is_some();
            assert!(
                !es_join_del_cliente,
                "un cliente no puede falsificar client_joined: {:?}",
                v
            );
        }
    }
}

#[tokio::test]
async fn ws_sobrescribe_source_con_identidad_real() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let autor_jwt = hub.mint_jwt(Project::A, "user-1", "a:chat", "both").await;
    let peer_jwt = hub
        .mint_jwt(Project::A, "user-2", "a:chat", "subscribe")
        .await;
    let mut peer = hub.ws_open(&peer_jwt, "a:chat").await.expect("peer");
    let mut autor = hub.ws_open(&autor_jwt, "a:chat").await.expect("autor");

    autor
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::json!({"type": "chat", "source": "evil", "text": "hola"}).to_string(),
        ))
        .await
        .expect("send");

    let mut source_visto: Option<String> = None;
    for _ in 0..6 {
        if let Some(v) = common::next_text(&mut peer, std::time::Duration::from_secs(2)).await {
            if v["payload"]["type"] == "chat" {
                source_visto = v["source"].as_str().map(|s| s.to_string());
                break;
            }
        }
    }
    assert_eq!(
        source_visto.as_deref(),
        Some("proj-a:user-1"),
        "el servidor debe imponer source=proyecto:usuario"
    );
}

#[tokio::test]
async fn rest_rechaza_payload_grande_targets_excesivos_y_profundidad() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(Project::A, "a:chat").await;

    // Payload > 64 KB
    let grande = serde_json::json!({"type": "chat", "text": "x".repeat(70 * 1024)});
    let res = hub
        .emit_with_key(&hub.key_a, &id_a, vec!["*".into()], grande)
        .await;
    assert_eq!(res.status(), 400, "payload > 64KB debe rechazarse");

    // Demasiados targets (max 50)
    let targets: Vec<String> = (0..51).map(|i| format!("user-{}", i)).collect();
    let res = hub
        .emit_with_key(
            &hub.key_a,
            &id_a,
            targets,
            serde_json::json!({"type": "chat"}),
        )
        .await;
    assert_eq!(res.status(), 400, "targets > 50 debe rechazarse");

    // Profundidad > 16
    let mut deep = serde_json::json!("fondo");
    for _ in 0..20 {
        deep = serde_json::json!([deep]);
    }
    let res = hub
        .emit_with_key(&hub.key_a, &id_a, vec!["*".into()], deep)
        .await;
    assert_eq!(
        res.status(),
        400,
        "payload demasiado profundo debe rechazarse"
    );

    // Tipo de sistema por REST
    let res = hub
        .emit_with_key(
            &hub.key_a,
            &id_a,
            vec!["*".into()],
            serde_json::json!({"type": "client_left"}),
        )
        .await;
    assert_eq!(res.status(), 400, "tipo de sistema debe rechazarse");
}

#[tokio::test]
async fn cada_conexion_tiene_connection_id_y_cerrar_una_no_afecta_a_la_otra() {
    let hub = Hub::start().await;
    hub.seed_channels().await;
    let jwt = hub
        .mint_jwt(Project::A, "user-1", "a:chat", "subscribe")
        .await;
    let mut c1 = hub.ws_open(&jwt, "a:chat").await.expect("c1");
    let mut c2 = hub.ws_open(&jwt, "a:chat").await.expect("c2");

    // Dos entradas con connection_id distintos para el mismo usuario.
    let res = hub.get_clients(&hub.key_a, "a:chat").await;
    let v: Value = res.json().await.unwrap();
    let list = v["data"].as_array().expect("array").clone();
    assert_eq!(list.len(), 2, "dos conexiones => dos entradas");
    let ids: Vec<&str> = list
        .iter()
        .filter_map(|c| c["connection_id"].as_str())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(
        ids[0] != ids[1],
        "connection_id debe ser único por conexión"
    );
    assert!(ids.iter().all(|i| !i.is_empty()));
    assert!(list.iter().all(|c| c["id"] == "user-1"), "identidad = sub");

    // Cerrar c1: c2 sigue vivo y la presencia baja a 1.
    use futures_util::SinkExt;
    c1.send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await
        .expect("close c1");
    let mut count = 2usize;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let res = hub.get_clients(&hub.key_a, "a:chat").await;
        let v: Value = res.json().await.unwrap();
        count = v["data"].as_array().map(|a| a.len()).unwrap_or(0);
        if count == 1 {
            break;
        }
    }
    assert_eq!(count, 1, "cerrar una conexión deja viva la otra");

    // c2 sigue recibiendo mensajes dirigidos al usuario.
    let res = hub
        .emit_with_key(
            &hub.key_a,
            "a:chat",
            vec!["user-1".into()],
            serde_json::json!({"type": "chat", "text": "sigue"}),
        )
        .await;
    assert_eq!(res.status(), 200);
    let mut recibido = false;
    for _ in 0..5 {
        if let Some(v) = common::next_text(&mut c2, std::time::Duration::from_secs(2)).await {
            if v["payload"]["text"] == "sigue" {
                recibido = true;
                break;
            }
        }
    }
    assert!(recibido, "la conexión restante sigue recibiendo");
}
