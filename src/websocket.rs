use crate::authz::{authorize_channel_for_jwt, AuthzError, ChannelAction};
use crate::handlers::AppState;
use crate::models::{ClientType, Event};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::HeaderMap,
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
    // Gate previo: exige JWT ANTES del upgrade para no malgastar recursos.
    // Con la API key cruda como token require_jwt ya devuelve 401 (sin puentes).
    match crate::middleware::require_jwt(&state, &headers, params.token.as_deref()).await {
        Ok(claims) => {
            info!(
                "WS pre-auth ok project={} sub={}",
                claims.project_name, claims.sub
            );
            ws.on_upgrade(move |socket| handle_socket(socket, state, claims))
        }
        Err((code, msg)) => {
            warn!("WS rechazado: {} ({})", msg, code);
            (code, msg).into_response()
        }
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, claims: crate::auth::Claims) {
    let (mut sender, mut receiver) = socket.split();

    // Handshake: { "channel": "<id o nombre>" } — la identidad NO la declara el cliente
    // (viene del JWT; en la fase de identidad se elimina client_id del protocolo).
    let channel_ref = match receiver.next().await {
        Some(Ok(msg)) => {
            if let Ok(text) = msg.to_text() {
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(obj) => match obj.get("channel").and_then(|v| v.as_str()) {
                        Some(c) if !c.trim().is_empty() => c.to_string(),
                        _ => {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"error": "falta el canal en el handshake"}).to_string(),
                                ))
                                .await;
                            return;
                        }
                    },
                    _ => {
                        let _ = sender
                            .send(Message::Text(json!({"error": "Invalid JSON"}).to_string()))
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

    // Autorización central única (proyecto dueño + rol subscribe + cobertura del claim).
    let effective_id = match authorize_channel_for_jwt(
        &state.channel_manager,
        &state.keys.pool,
        &claims,
        &channel_ref,
        ChannelAction::Subscribe,
    )
    .await
    {
        Ok(id) => id,
        Err(AuthzError::NotFound) => {
            let _ = sender
                .send(Message::Text(
                    json!({"error": "Channel not found"}).to_string(),
                ))
                .await;
            return;
        }
        Err(e) => {
            warn!(
                "WS sub={} denegado en canal {} ({:?})",
                claims.sub, channel_ref, e
            );
            let _ = sender
                .send(Message::Text(
                    json!({"error": "forbidden para este canal"}).to_string(),
                ))
                .await;
            return;
        }
    };

    // client_id sigue siendo el identificador de presencia hasta la fase de identidad.
    let client_id = claims.sub.clone();

    let tx = match state.channel_manager.subscribe(
        &effective_id,
        client_id.clone(),
        ClientType::WebSocket,
        claims.project_name.clone(),
    ) {
        Ok(tx) => tx,
        Err(e) => {
            let _ = sender
                .send(Message::Text(json!({"error": e}).to_string()))
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
    let jwt_exp = claims.exp;

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
                                if let Err(e) = sender.send(Message::Text(json_str)).await {
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
                            let _ = sender
                                .send(Message::Text(
                                    json!({"error": "forbidden: tu rol no permite publicar"}).to_string(),
                                ))
                                .await;
                            continue;
                        }
                        // Re-verificación de autorización en cada publicación (por si la
                        // key se revocó/eliminó mientras la conexión seguía abierta).
                        if authorize_channel_for_jwt(
                            &state.channel_manager,
                            &state.keys.pool,
                            &claims,
                            &effective_id,
                            ChannelAction::Publish,
                        )
                        .await
                        .is_err()
                        {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"error": "forbidden: credencial sin acceso al canal"}).to_string(),
                                ))
                                .await;
                            break;
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
                let _ = sender.send(Message::Text(json!({"error": "token expirado, reconectar"}).to_string())).await;
                break;
            }
        }
    }
    state.channel_manager.unsubscribe(&effective_id, &client_id);
}
