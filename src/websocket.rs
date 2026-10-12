use crate::handlers::AppState;
use crate::middleware::{jwt_can_access_channel, request_origin};
use crate::models::{ClientType, Event};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, error, info, warn};

#[derive(Debug, Deserialize)]
pub struct WsParams {
    pub token: Option<String>,
}

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<WsParams>,
) -> impl IntoResponse {
    // Gate previo: exige token (JWT o service key) ANTES del upgrade para no malgastar recursos.
    // El chequeo fino de canal se hace dentro del handshake (necesita channel del primer mensaje).
    match crate::middleware::require_jwt(&state, &headers, params.token.as_deref()).await {
        Ok(claims) => {
            let origin = request_origin(&headers);
            info!(
                "WS pre-auth ok project={} sub={} origin={:?}",
                claims.project_name, claims.sub, origin
            );
            ws.on_upgrade(move |socket| handle_socket(socket, state, headers, claims))
        }
        Err((code, msg)) => {
            warn!("WS rechazado: {} ({})", msg, code);
            (code, msg).into_response()
        }
    }
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    headers: HeaderMap,
    claims: crate::auth::Claims,
) {
    let (mut sender, mut receiver) = socket.split();
    let _ = headers; // reservado para auditoría futura (IP ya validada en pre-auth)

    // Handshake: { "channel": "...", "client_id": "..." }
    let (channel_id, client_id) = match receiver.next().await {
        Some(Ok(msg)) => {
            if let Ok(text) = msg.to_text() {
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(obj) => {
                        let channel = obj.get("channel").and_then(|v| v.as_str());
                        let client = obj.get("client_id").and_then(|v| v.as_str());
                        match (channel, client) {
                            (Some(c), Some(cl)) => (c.to_string(), cl.to_string()),
                            _ => {
                                let _ = sender
                                    .send(Message::Text(
                                        json!({"error": "Missing channel or client_id"})
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                                return;
                            }
                        }
                    }
                    _ => {
                        let _ = sender
                            .send(Message::Text(
                                json!({"error": "Invalid JSON"}).to_string().into(),
                            ))
                            .await;
                        return;
                    }
                }
            } else {
                return;
            }
        }
        _ => return,
    };

    // Autorización fina: el JWT debe cubrir este canal (por id o nombre).
    // Resolvemos nombre si el cliente mandó un ID: el manager guarda id->Channel{name}.
    let name_lookup = state
        .channel_manager
        .get_channel(&channel_id)
        .map(|c| c.name.clone());
    // También soportar que el cliente mande el NOMBRE en vez del id: buscar id por nombre.
    let effective_id = if state.channel_manager.get_channel(&channel_id).is_some() {
        channel_id.clone()
    } else {
        // buscar por nombre
        let found = state
            .channel_manager
            .list_channels()
            .into_iter()
            .find(|c| c.name == channel_id)
            .map(|c| c.id);
        match found {
            Some(id) => id,
            None => {
                let _ = sender
                    .send(Message::Text(
                        json!({"error": "Channel not found"}).to_string().into(),
                    ))
                    .await;
                return;
            }
        }
    };
    let name_for_check = name_lookup.clone().unwrap_or_else(|| channel_id.clone());
    if !jwt_can_access_channel(&state, &claims, &effective_id, Some(&name_for_check)).await
        && !jwt_can_access_channel(&state, &claims, &channel_id, Some(&name_for_check)).await
    {
        warn!(
            "WS sub={} sin acceso al canal {}",
            claims.sub, channel_id
        );
        let _ = sender
            .send(Message::Text(
                json!({"error": "No autorizado para este canal"}).to_string().into(),
            ))
            .await;
        return;
    }

    // Solo roles con subscribe pueden abrir WS
    if !claims.can_subscribe() {
        let _ = sender
            .send(Message::Text(
                json!({"error": "JWT sin rol subscribe"}).to_string().into(),
            ))
            .await;
        return;
    }

    let tx = match state.channel_manager.subscribe(
        &effective_id,
        client_id.clone(),
        ClientType::WebSocket,
        claims.project_name.clone(),
    ) {
        Ok(tx) => tx,
        Err(e) => {
            let _ = sender
                .send(Message::Text(json!({"error": e}).to_string().into()))
                .await;
            return;
        }
    };

    info!(
        "WS client {} (sub={} proj={}) -> {}",
        client_id, claims.sub, claims.project_name, effective_id
    );
    let mut rx = tx.subscribe();

    if let Some(clients) = state.channel_manager.get_clients(&effective_id) {
        let clients_list: Vec<String> = clients.iter().map(|c| c.id.clone()).collect();
        let join_event = Event::new(
            effective_id.clone(),
            "__system__".to_string(),
            vec!["*".to_string()],
            json!({ "type": "client_joined", "client_id": client_id, "clients": clients_list, "total_clients": clients.len() }),
        );
        let _ = tx.send(join_event);
    }

    // Si el JWT expira a mitad de conexión, la cortamos (re-autenticación periódica).
    let jwt_exp = if claims.jti == "svc" {
        i64::MAX
    } else {
        claims.exp
    };

    loop {
        // Timeout hasta expiración del JWT para forzar re-auth
        let now = chrono::Utc::now().timestamp();
        let secs_left = (jwt_exp - now).max(0) as u64;
        let timeout_dur = std::time::Duration::from_secs(secs_left.min(3600));

        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let should_receive = event.targets.contains(&"*".to_string()) || event.targets.contains(&client_id);
                        if should_receive {
                            if let Ok(json_str) = serde_json::to_string(&event) {
                                if let Err(e) = sender.send(Message::Text(json_str.into())).await {
                                    error!("Failed to send to client: {}", e);
                                    break;
                                }
                            }
                        } else {
                            debug!("Event for {:?} received but not for client {}", event.targets, client_id);
                        }
                    }
                    Err(_) => { debug!("Channel broadcast closed"); break; }
                }
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Solo roles publish pueden emitir por WS
                        if !claims.can_publish() {
                            continue;
                        }
                        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
                            let event = Event::new(
                                effective_id.clone(),
                                client_id.clone(),
                                vec!["*".to_string()],
                                payload,
                            );
                            if let Err(e) = state.channel_manager.emit_event(&effective_id, event) {
                                error!("Error emitiendo evento: {}", e);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!("Client {} disconnected", client_id);
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(timeout_dur), if jwt_exp != i64::MAX => {
                info!("JWT expirado para {}, cerrando WS (re-auth requerida)", client_id);
                let _ = sender.send(Message::Text(json!({"error": "token expirado, reconectar"}).to_string().into())).await;
                break;
            }
        }
    }
    state.channel_manager.unsubscribe(&effective_id, &client_id);
    let _ = StatusCode::OK;
}
