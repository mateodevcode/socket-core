use crate::authz::{authorize_channel_for_jwt, AuthzError, ChannelAction};
use crate::handlers::AppState;
use crate::models::{validate_outbound, ClientType, Event, MAX_PAYLOAD_BYTES, SYSTEM_SOURCE};
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

/// Sanea un nombre de dispositivo opcional: trim, máx 64 caracteres, sin controles.
fn sanitize_device(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let clean: String = raw.chars().filter(|c| !c.is_control()).take(64).collect();
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, claims: crate::auth::Claims) {
    let (mut sender, mut receiver) = socket.split();

    // Handshake: { "channel": "<id o nombre>", "device": "<opcional>" }.
    // La identidad NO la declara el cliente: es el `sub` del JWT.
    let (channel_ref, device) = match receiver.next().await {
        Some(Ok(msg)) => {
            let Some(text) = msg.to_text().ok() else {
                return;
            };
            let obj: serde_json::Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => {
                    let _ = sender
                        .send(Message::Text(json!({"error": "Invalid JSON"}).to_string()))
                        .await;
                    return;
                }
            };
            match obj.get("channel").and_then(|v| v.as_str()) {
                Some(c) if !c.trim().is_empty() => (
                    c.to_string(),
                    sanitize_device(obj.get("device").and_then(|v| v.as_str())),
                ),
                _ => {
                    let _ = sender
                        .send(Message::Text(
                            json!({"error": "falta el canal en el handshake"}).to_string(),
                        ))
                        .await;
                    return;
                }
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

    // Identidad del usuario = sub del JWT. connection_id lo genera el servidor.
    let user_id = claims.sub.clone();
    let connection_id = uuid::Uuid::new_v4().to_string();

    let tx = match state.channel_manager.subscribe(
        &effective_id,
        user_id.clone(),
        connection_id.clone(),
        device.clone(),
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
        "WS user {} (conn {}) proj={} -> {}",
        user_id, connection_id, claims.project_name, effective_id
    );
    let mut rx = tx.subscribe();

    // Presencia inicial: solo dentro del canal/proyecto (no se expone fuera).
    if let Some(clients) = state.channel_manager.get_clients(&effective_id) {
        let join_event = Event::new(
            effective_id.clone(),
            SYSTEM_SOURCE.to_string(),
            vec!["*".to_string()],
            json!({
                "type": "client_joined",
                "user": user_id,
                "connection_id": connection_id,
                "clients": clients,
                "total_clients": clients.len()
            }),
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
                        // Los targets se refieren a USUARIOS (sub), nunca a un id declarado.
                        let should_receive = event.targets.iter().any(|t| t == "*" || t == &user_id);
                        if should_receive {
                            if let Ok(json_str) = serde_json::to_string(&event) {
                                if let Err(e) = sender.send(Message::Text(json_str)).await {
                                    error!("Failed to send to client: {}", e);
                                    break;
                                }
                            }
                        } else {
                            debug!("Event for {:?} received but not for user {}", event.targets, user_id);
                        }
                    }
                    Err(_) => { debug!("Channel broadcast closed"); break; }
                }
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Tope de frame/payload (64 KB). Cierra la conexión si lo supera.
                        if text.len() > MAX_PAYLOAD_BYTES {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"type": "error", "code": "message_too_large"}).to_string(),
                                ))
                                .await;
                            break;
                        }
                        // Solo roles publish pueden emitir por WS.
                        if !claims.can_publish() {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"type": "error", "code": "forbidden", "detail": "tu rol no permite publicar"}).to_string(),
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
                                    json!({"type": "error", "code": "forbidden", "detail": "credencial sin acceso al canal"}).to_string(),
                                ))
                                .await;
                            break;
                        }
                        let payload: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = sender
                                    .send(Message::Text(
                                        json!({"type": "error", "code": "invalid_payload"}).to_string(),
                                    ))
                                    .await;
                                continue;
                            }
                        };
                        // Targets opcionales del payload (a usuarios); por defecto "*".
                        let mut targets = vec!["*".to_string()];
                        if let Some(arr) = payload.get("targets").and_then(|v| v.as_array()) {
                            let parsed: Option<Vec<String>> = arr
                                .iter()
                                .map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            if let Some(t) = parsed {
                                targets = t;
                            } else {
                                let _ = sender
                                    .send(Message::Text(
                                        json!({"type": "error", "code": "invalid_payload", "detail": "targets debe ser lista de strings"}).to_string(),
                                    ))
                                    .await;
                                continue;
                            }
                        }
                        // El servidor impone source y bloquea tipos de sistema.
                        let source = format!("{}:{}", claims.project_name, claims.sub);
                        if let Err(e) = validate_outbound(&source, &targets, &payload) {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"type": "error", "code": "invalid_payload", "detail": e.to_string()}).to_string(),
                                ))
                                .await;
                            continue;
                        }
                        let event = Event::new(effective_id.clone(), source, targets, payload);
                        if let Err(e) = state.channel_manager.emit_event(&effective_id, event) {
                            error!("Error emitiendo evento: {}", e);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!("Client {} (conn {}) disconnected", user_id, connection_id);
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
                info!("JWT expirado para {}, cerrando WS (re-auth requerida)", user_id);
                let _ = sender.send(Message::Text(json!({"type": "error", "code": "token_expired", "detail": "reconecta con un token nuevo"}).to_string())).await;
                break;
            }
        }
    }
    state
        .channel_manager
        .unsubscribe(&effective_id, &connection_id);
}
