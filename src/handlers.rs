use crate::auth::create_jwt;
use crate::authz::{
    authorize_channel_for_jwt, authorize_channel_for_key, AuthzError, ChannelAction,
};
use crate::channel_manager::ChannelManager;
use crate::middleware::{client_ip, require_api_key, require_jwt, require_master, RateLimiter};
use crate::models::*;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Clone)]
pub struct AppState {
    pub channel_manager: ChannelManager,
    pub keys: crate::keys::KeyStore,
    pub jwt_secret: String,
    pub jwt_ttl_secs: i64,
    pub master_key: String,
    pub limiter: RateLimiter,
}

pub async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "socket-core"}))
}

/// Convierte un AuthzError en Response (404/403/401, sin detalles internos).
fn authz_err(e: AuthzError) -> axum::response::Response {
    let (code, msg) = e.status();
    err(code, msg)
}

/// Detecta si la petición trae X-Api-Key (rama exclusiva en emit).
fn has_api_key(headers: &HeaderMap) -> bool {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

// ---- REST protegida ----

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    let (channels, clients) = state.channel_manager.stats_for(&key.id);
    Json(ApiResponse::ok(ServerStats {
        channels,
        total_clients: clients,
    }))
    .into_response()
}

pub async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateChannelRequest>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "manage").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    if req.name.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "name requerido");
    }
    // El nombre SIEMPRE quedará bajo el prefijo del propio proyecto: si no es así,
    // fuera (aunque la colisión sea con otro proyecto).
    if !crate::keys::channel_allowed(&key, &req.name) {
        return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
    }
    match state
        .channel_manager
        .create_channel(key.id.clone(), req.name, req.description)
    {
        Ok(channel) => {
            // Persistencia: el canal sobrevive a reinicios (los clientes no).
            state
                .channel_manager
                .persist(&state.keys.pool, &channel)
                .await;
            (StatusCode::CREATED, Json(ApiResponse::ok(channel))).into_response()
        }
        Err(_) => err(StatusCode::BAD_REQUEST, "channel name already exists"),
    }
}

pub async fn list_channels(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    Json(ApiResponse::ok(
        state.channel_manager.list_channels_for(&key.id),
    ))
    .into_response()
}

pub async fn get_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    match authorize_channel_for_key(&state.channel_manager, &key, &id, ChannelAction::View) {
        Ok(id_canon) => {
            let channel = state.channel_manager.get_channel(&id_canon);
            Json(ApiResponse::ok(channel)).into_response()
        }
        Err(e) => authz_err(e),
    }
}

pub async fn get_channel_clients(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    match authorize_channel_for_key(&state.channel_manager, &key, &id, ChannelAction::View) {
        Ok(id_canon) => {
            let clients = state.channel_manager.get_clients(&id_canon);
            Json(ApiResponse::ok(clients)).into_response()
        }
        Err(e) => authz_err(e),
    }
}

pub async fn delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "manage").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    let target_id = match authorize_channel_for_key(
        &state.channel_manager,
        &key,
        &id_or_name,
        ChannelAction::Manage,
    ) {
        Ok(t) => t,
        Err(e) => return authz_err(e),
    };
    if state.channel_manager.remove_channel(&target_id) {
        let _ = sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(&target_id)
            .execute(&state.keys.pool)
            .await;
        Json(ApiResponse::ok(json!({"deleted": true}))).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "Channel not found")
    }
}

pub async fn emit_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<EmitEventRequest>,
) -> impl IntoResponse {
    // Ramas EXCLUSIVAS: si hay X-Api-Key, esa rama manda y su error es definitivo.
    // Si no la hay, solo JWT. Sin fallback cruzado.
    let effective_id = if has_api_key(&headers) {
        let key = match require_api_key(&state, &headers, "publish").await {
            Ok(k) => k,
            Err(v) => return err(v.0, v.1),
        };
        match authorize_channel_for_key(&state.channel_manager, &key, &id, ChannelAction::Publish) {
            Ok(cid) => cid,
            Err(e) => return authz_err(e),
        }
    } else {
        let claims = match require_jwt(&state, &headers, None).await {
            Ok(c) => c,
            Err(v) => return err(v.0, v.1),
        };
        match authorize_channel_for_jwt(
            &state.channel_manager,
            &state.keys.pool,
            &claims,
            &id,
            ChannelAction::Publish,
        )
        .await
        {
            Ok(cid) => cid,
            Err(e) => return authz_err(e),
        }
    };

    // Rate extra por IP emisora (fase de red ajustará la confianza en XFF)
    let ip_bucket = format!("emit:{}", client_ip(&headers));
    if !state.limiter.check(&ip_bucket, 600) {
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit por IP");
    }

    let source = if req.source.trim().is_empty() {
        "unknown".to_string()
    } else {
        req.source
    };
    let event = Event::new(effective_id.clone(), source, req.targets, req.payload);
    match state.channel_manager.emit_event(&effective_id, event) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::ok(json!({"success": true}))),
        )
            .into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "Channel not found"),
    }
}

// ---- Auth: emitir JWT ----

pub async fn mint_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TokenRequest>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "").await {
        Ok(k) => k,
        Err(v) => return err(v.0, v.1),
    };
    let channel = req.channel.unwrap_or_else(|| "*".into());
    let role = req.role.unwrap_or_else(|| "both".into());
    if !["publish", "subscribe", "both"].contains(&role.as_str()) {
        return err(
            StatusCode::BAD_REQUEST,
            "role debe ser publish|subscribe|both",
        );
    }
    // El canal pedido debe estar dentro del prefijo del proyecto (salvo "*")
    if channel != "*" && !crate::keys::channel_allowed(&key, &channel) {
        return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
    }
    // El scope de la key debe cubrir el rol pedido
    if role.contains("publish") && !crate::keys::has_scope(&key, "publish") {
        return err(StatusCode::FORBIDDEN, "key sin scope publish");
    }
    if role.contains("subscribe") && !crate::keys::has_scope(&key, "subscribe") {
        return err(StatusCode::FORBIDDEN, "key sin scope subscribe");
    }

    let user_id = req
        .user_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    match create_jwt(
        &state.jwt_secret,
        state.jwt_ttl_secs,
        &key.id,
        &key.name,
        &user_id,
        &channel,
        &role,
    ) {
        Ok((token, _)) => Json(ApiResponse::ok(TokenResponse {
            token,
            expires_in: state.jwt_ttl_secs,
            channel,
            role,
        }))
        .into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no se pudo emitir el token",
        ),
    }
}

// ---- Admin (MASTER_KEY) ----

pub async fn admin_list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    let keys = state.keys.list().await;
    let pub_keys: Vec<ApiKeyPublic> = keys
        .into_iter()
        .map(|k| ApiKeyPublic {
            id: k.id,
            name: k.name,
            domains: k.domains,
            channel_prefixes: k.channel_prefixes,
            scopes: k.scopes,
            rate_limit_per_min: k.rate_limit_per_min,
            expires_at: k.expires_at,
            revoked: k.revoked,
            created_at: k.created_at,
        })
        .collect();
    Json(ApiResponse::ok(pub_keys)).into_response()
}

pub async fn admin_create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    if req.name.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "name requerido");
    }
    let expires_at = req
        .expires_in_days
        .map(|d| chrono::Utc::now().timestamp() + d * 86400);
    match state
        .keys
        .create(
            req.name,
            req.domains,
            req.channel_prefix,
            req.scopes,
            req.rate_limit_per_min,
            expires_at,
        )
        .await
    {
        Ok((k, raw)) => (
            StatusCode::CREATED,
            Json(ApiResponse::ok(CreateKeyResponse {
                id: k.id,
                name: k.name,
                api_key: raw,
                domains: k.domains,
                channel_prefix: k.channel_prefixes.join(","),
                scopes: k.scopes,
                rate_limit_per_min: k.rate_limit_per_min,
                expires_at: k.expires_at,
            })),
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn admin_revoke_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    if state.keys.revoke(&id).await {
        Json(ApiResponse::ok(json!({"revoked": true}))).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "key no encontrada")
    }
}

pub async fn admin_update_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateKeyRequest>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    match state
        .keys
        .update(
            &id,
            req.name,
            req.domains,
            req.channel_prefix,
            req.scopes,
            req.rate_limit_per_min,
            req.expires_in_days,
        )
        .await
    {
        Ok(k) => Json(ApiResponse::ok(ApiKeyPublic {
            id: k.id,
            name: k.name,
            domains: k.domains,
            channel_prefixes: k.channel_prefixes,
            scopes: k.scopes,
            rate_limit_per_min: k.rate_limit_per_min,
            expires_at: k.expires_at,
            revoked: k.revoked,
            created_at: k.created_at,
        }))
        .into_response(),
        Err(e) => {
            let code = if e == "key no encontrada" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            err(code, e)
        }
    }
}

pub async fn admin_delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    if state.keys.delete(&id).await {
        Json(ApiResponse::ok(json!({"deleted": true}))).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "key no encontrada")
    }
}

#[derive(Debug, Deserialize)]
pub struct RevokeQuery {
    pub jti: Option<String>,
}

pub async fn admin_revoke_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RevokeQuery>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_master(&headers, &state.master_key) {
        return err(c, m);
    }
    let jti = q.jti.or_else(|| {
        body.get("jti")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    let Some(jti) = jti else {
        return err(StatusCode::BAD_REQUEST, "jti requerido");
    };
    // exp aproximada: ahora + ttl (la limpieza la purga después)
    let exp = chrono::Utc::now().timestamp() + state.jwt_ttl_secs;
    state.keys.revoke_token(&jti, exp).await;
    Json(ApiResponse::ok(json!({"revoked": true, "jti": jti}))).into_response()
}

fn err(code: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    (
        code,
        Json(ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(msg.into()),
        }),
    )
        .into_response()
}
