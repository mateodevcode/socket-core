use crate::authz::{authorize_channel_for_jwt, AuthzError, ChannelAction};
use crate::control::ControlEvent;
use crate::handlers::AppState;
use crate::limits::{ConnectionRegistry, PublishWindow};
use crate::middleware::ClientIp;
use crate::models::{validate_outbound, ClientType, Event, SYSTEM_SOURCE};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

#[derive(Debug, Deserialize)]
pub struct WsParams {
    pub token: Option<String>,
}

/// Libera el slot del registro al caer la conexión (incluye salidas tempranas).
struct ConnGuard {
    registry: ConnectionRegistry,
    project_id: String,
    ip: String,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.project_id, &self.ip);
    }
}

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    ip: ClientIp,
    Query(params): Query<WsParams>,
) -> impl IntoResponse {
    // Gate previo: exige JWT ANTES del upgrade para no malgastar recursos.
    let claims =
        match crate::middleware::require_jwt(&state, &headers, params.token.as_deref()).await {
            Ok(c) => c,
            Err((code, msg)) => {
                warn!("WS rechazado: {} ({})", msg, code);
                return (code, msg).into_response();
            }
        };

    // El proyecto debe seguir existiendo, activo y vigente (fail-closed).
    match state.keys.is_project_active(&claims.project_id).await {
        Ok(true) => {}
        Ok(false) => {
            warn!(
                "WS rechazado: proyecto {} inactivo/revocado",
                claims.project_id
            );
            return (StatusCode::UNAUTHORIZED, "credential invalid or revoked").into_response();
        }
        Err(e) => {
            error!(
                "WS: no se pudo comprobar el proyecto {}: {}",
                claims.project_id, e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "no se pudo verificar la credencial",
            )
                .into_response();
        }
    }

    // Límite de conexiones por proyecto y por IP (slot reservado antes del upgrade).
    let ip = ip.0;
    let ip_key = ip.to_string();
    if !state.registry.try_register(
        &claims.project_id,
        &ip_key,
        state.limits.max_conns_project,
        state.limits.max_conns_ip,
    ) {
        warn!(
            "WS rechazado por límite de conexiones (proyecto {} ip {})",
            claims.project_id, ip
        );
        return (StatusCode::TOO_MANY_REQUESTS, "too many connections").into_response();
    }
    let guard = ConnGuard {
        registry: state.registry.clone(),
        project_id: claims.project_id.clone(),
        ip: ip_key,
    };

    info!(
        "WS pre-auth ok project={} sub={}",
        claims.project_name, claims.sub
    );

    ws.max_message_size(state.limits.max_message_bytes)
        .max_frame_size(state.limits.max_message_bytes)
        .on_upgrade(move |socket| handle_socket(socket, state, claims, guard))
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

async fn close_with(sender: &mut SplitSink<WebSocket, Message>, code: &str, detail: &str) {
    let _ = sender
        .send(Message::Text(
            json!({"type": "error", "code": code, "detail": detail}).to_string(),
        ))
        .await;
    let _ = sender.send(Message::Close(None)).await;
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    claims: crate::auth::Claims,
    _guard: ConnGuard,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut control_rx = state.control.subscribe();

    // Handshake con timeout: { "channel": "<id o nombre>", "device": "<opcional>" }.
    // La identidad NO la declara el cliente: es el `sub` del JWT.
    let first = tokio::time::timeout(
        Duration::from_secs(state.limits.handshake_timeout_secs),
        receiver.next(),
    )
    .await;
    let (channel_ref, device) = match first {
        Err(_) => {
            warn!("WS sub={} sin handshake a tiempo", claims.sub);
            close_with(&mut sender, "handshake_timeout", "envía {channel} a tiempo").await;
            return;
        }
        Ok(None) | Ok(Some(Err(_))) => return,
        Ok(Some(Ok(Message::Close(_)))) => return,
        Ok(Some(Ok(msg))) => {
            let Some(text) = msg.to_text().ok() else {
                close_with(&mut sender, "invalid_payload", "se esperaba texto JSON").await;
                return;
            };
            let obj: serde_json::Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => {
                    close_with(&mut sender, "invalid_payload", "JSON inválido").await;
                    return;
                }
            };
            match obj.get("channel").and_then(|v| v.as_str()) {
                Some(c) if !c.trim().is_empty() => (
                    c.to_string(),
                    sanitize_device(obj.get("device").and_then(|v| v.as_str())),
                ),
                _ => {
                    close_with(&mut sender, "invalid_payload", "falta el canal").await;
                    return;
                }
            }
        }
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
            close_with(&mut sender, "not_found", "canal no encontrado").await;
            return;
        }
        Err(e) => {
            warn!(
                "WS sub={} denegado en canal {} ({:?})",
                claims.sub, channel_ref, e
            );
            close_with(&mut sender, "forbidden", "sin acceso a este canal").await;
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
        device,
        ClientType::WebSocket,
        claims.project_name.clone(),
    ) {
        Ok(tx) => tx,
        Err(_) => {
            close_with(&mut sender, "not_found", "canal no encontrado").await;
            return;
        }
    };

    info!(
        "WS user {} (conn {}) proj={} -> {}",
        user_id, connection_id, claims.project_name, effective_id
    );
    let mut rx = tx.subscribe();

    // Presencia mínima: solo la conexión afectada; la lista completa va por
    // GET /channels/:id/clients (con authz), nunca en broadcasts.
    let total_clients = state
        .channel_manager
        .get_clients(&effective_id)
        .map(|c| c.len())
        .unwrap_or(0);
    let join_event = Event::new(
        effective_id.clone(),
        SYSTEM_SOURCE.to_string(),
        vec!["*".to_string()],
        json!({
            "type": "client_joined",
            "user": user_id,
            "connection_id": connection_id,
            "total_clients": total_clients
        }),
    );
    let _ = tx.send(join_event);

    let jwt_exp = claims.exp;

    // Vigilantes periódicos de la conexión.
    let mut auth_tick = tokio::time::interval(Duration::from_secs(state.limits.auth_recheck_secs));
    auth_tick.tick().await; // consume el tick inmediato
    let mut ping_tick = tokio::time::interval(Duration::from_secs(state.limits.ping_interval_secs));
    ping_tick.tick().await;
    let mut last_inbound = Instant::now();
    let mut publish_window = PublishWindow::new(state.limits.publish_per_sec_conn);

    loop {
        let now = chrono::Utc::now().timestamp();
        let secs_left = (jwt_exp - now).max(0) as u64;
        let timeout_dur = Duration::from_secs(secs_left.min(3600));

        tokio::select! {
            // 1) Órdenes internas (revocación/borrado/apagado).
            ctrl = control_rx.recv() => {
                match ctrl {
                    Ok(ControlEvent::CloseProject(pid)) if pid == claims.project_id => {
                        info!("cerrando WS de proyecto revocado/borrado: {}", pid);
                        close_with(&mut sender, "credential_revoked", "credencial revocada").await;
                        break;
                    }
                    Ok(ControlEvent::CloseJti(jti)) if jti == claims.jti => {
                        info!("cerrando WS de jti revocado: {}", jti);
                        close_with(&mut sender, "token_revoked", "token revocado").await;
                        break;
                    }
                    Ok(ControlEvent::CloseChannel(cid)) if cid == effective_id => {
                        info!("cerrando WS de canal borrado: {}", cid);
                        close_with(&mut sender, "channel_deleted", "canal borrado").await;
                        break;
                    }
                    Ok(ControlEvent::Shutdown) => {
                        info!("apagado del servidor: cerrando WS {}", connection_id);
                        close_with(&mut sender, "server_shutdown", "servidor apagándose").await;
                        break;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        // El bus no puede perder órdenes críticas: revalida por watchdog.
                    }
                    Err(RecvError::Closed) => {}
                }
            }

            // 2) Revalidación periódica de credenciales (fail-closed, pero distinguiendo
            //    revocación real de un fallo interno de DB: este último es reintentable).
            _ = auth_tick.tick() => {
                enum Estado { Ok, Revocado, FalloInterno }
                let estado = match state.keys.is_token_revoked(&claims.jti).await {
                    Ok(true) => Estado::Revocado,
                    Ok(false) => match authorize_channel_for_jwt(
                        &state.channel_manager,
                        &state.keys.pool,
                        &claims,
                        &effective_id,
                        ChannelAction::Subscribe,
                    )
                    .await
                    {
                        Ok(_) => Estado::Ok,
                        Err(AuthzError::Internal) => Estado::FalloInterno,
                        Err(_) => Estado::Revocado,
                    },
                    Err(e) => {
                        tracing::error!(
                            "WS {}: error de DB al revalidar el token: {}",
                            connection_id,
                            e
                        );
                        Estado::FalloInterno
                    }
                };
                match estado {
                    Estado::Ok => {}
                    Estado::Revocado => {
                        info!("WS {} sin credenciales válidas en revalidación", connection_id);
                        close_with(&mut sender, "credential_revoked", "credencial ya no es válida").await;
                        break;
                    }
                    Estado::FalloInterno => {
                        warn!("WS {}: DB no disponible; cierre reintentable", connection_id);
                        close_with(&mut sender, "server_error", "error interno temporal").await;
                        break;
                    }
                }
            }

            // 3) Ping/pong e idle timeout.
            _ = ping_tick.tick() => {
                if last_inbound.elapsed() > Duration::from_secs(state.limits.idle_timeout_secs) {
                    info!("WS {} inactivo demasiado tiempo", connection_id);
                    close_with(&mut sender, "idle_timeout", "sin actividad").await;
                    break;
                }
                if sender.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }

            // 4) Eventos del canal.
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let should_receive =
                            event.targets.iter().any(|t| t == "*" || t == &user_id);
                        if should_receive {
                            if let Ok(json_str) = serde_json::to_string(&event) {
                                if let Err(e) = sender.send(Message::Text(json_str)).await {
                                    error!("Failed to send to client: {}", e);
                                    break;
                                }
                            }
                        } else {
                            debug!("Event for {:?} not for user {}", event.targets, user_id);
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Cliente lento: avisar en vez de cerrar en silencio.
                        warn!("WS {} lagged: {} eventos perdidos", connection_id, n);
                        let _ = sender
                            .send(Message::Text(
                                json!({"type": "missed_events", "missed": n}).to_string(),
                            ))
                            .await;
                    }
                    Err(RecvError::Closed) => {
                        debug!("Channel broadcast closed");
                        break;
                    }
                }
            }

            // 5) Mensajes del cliente.
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        last_inbound = Instant::now();
                        if text.len() > state.limits.max_message_bytes {
                            close_with(&mut sender, "message_too_large", "máximo 64 KB").await;
                            break;
                        }
                        if !publish_window.allow()
                            || !state
                                .publish_limits
                                .allow(&claims.project_id, state.limits.publish_per_sec_project)
                        {
                            close_with(&mut sender, "rate_limited", "demasiadas publicaciones por segundo").await;
                            break;
                        }
                        if !claims.can_publish() {
                            let _ = sender
                                .send(Message::Text(
                                    json!({"type": "error", "code": "forbidden", "detail": "tu rol no permite publicar"}).to_string(),
                                ))
                                .await;
                            continue;
                        }
                        match authorize_channel_for_jwt(
                            &state.channel_manager,
                            &state.keys.pool,
                            &claims,
                            &effective_id,
                            ChannelAction::Publish,
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(AuthzError::Internal) => {
                                warn!(
                                    "WS {}: DB no disponible al autorizar publish; cierre reintentable",
                                    connection_id
                                );
                                close_with(&mut sender, "server_error", "error interno temporal").await;
                                break;
                            }
                            Err(_) => {
                                close_with(&mut sender, "credential_revoked", "credencial sin acceso al canal").await;
                                break;
                            }
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
                        let mut targets = vec!["*".to_string()];
                        if let Some(arr) = payload.get("targets").and_then(|v| v.as_array()) {
                            let parsed: Option<Vec<String>> =
                                arr.iter().map(|v| v.as_str().map(|s| s.to_string())).collect();
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
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                        last_inbound = Instant::now();
                    }
                    Some(Ok(Message::Binary(_))) => {
                        last_inbound = Instant::now();
                        let _ = sender
                            .send(Message::Text(
                                json!({"type": "error", "code": "invalid_payload", "detail": "solo texto JSON"}).to_string(),
                            ))
                            .await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!("Client {} (conn {}) disconnected", user_id, connection_id);
                        break;
                    }
                    Some(Err(e)) => {
                        debug!("WebSocket error en {}: {}", connection_id, e);
                        break;
                    }
                }
            }

            // 6) Expiración del JWT a mitad de conexión.
            _ = tokio::time::sleep(timeout_dur) => {
                info!("JWT expirado para {}, cerrando WS (re-auth requerida)", user_id);
                close_with(&mut sender, "token_expired", "reconecta con un token nuevo").await;
                break;
            }
        }
    }

    state
        .channel_manager
        .unsubscribe(&effective_id, &connection_id);
}
