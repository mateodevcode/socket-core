use crate::auth::create_jwt;
use crate::channel_manager::ChannelManager;
use crate::keys::{channel_allowed, ApiKey};
use crate::middleware::{client_ip, require_api_key, require_master, RateLimiter};
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

// ---- REST protegida ----

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err((code, msg)) = require_api_key(&state, &headers, "").await {
        return err(code, msg);
    }
    let (channels, clients) = state.channel_manager.stats();
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
        Err((c, m)) => return err(c, m),
    };
    if req.name.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "name requerido");
    }
    if !channel_allowed(&key, &req.name) {
        return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
    }
    match state
        .channel_manager
        .create_channel(req.name, req.description)
    {
        Ok(channel) => {
            // Persistencia: el canal sobrevive a reinicios (los clientes no).
            state
                .channel_manager
                .persist(&state.keys.pool, &channel)
                .await;
            (StatusCode::CREATED, Json(ApiResponse::ok(channel))).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn list_channels(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err((c, m)) = require_api_key(&state, &headers, "").await {
        return err(c, m);
    }
    Json(ApiResponse::ok(state.channel_manager.list_channels())).into_response()
}

pub async fn get_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_api_key(&state, &headers, "").await {
        return err(c, m);
    }
    match state.channel_manager.get_channel(&id) {
        Some(channel) => Json(ApiResponse::ok(channel)).into_response(),
        None => err(StatusCode::NOT_FOUND, "Channel not found"),
    }
}

pub async fn get_channel_clients(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err((c, m)) = require_api_key(&state, &headers, "").await {
        return err(c, m);
    }
    match state.channel_manager.get_clients(&id) {
        Some(clients) => Json(ApiResponse::ok(clients)).into_response(),
        None => err(StatusCode::NOT_FOUND, "Channel not found"),
    }
}

pub async fn delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let key = match require_api_key(&state, &headers, "manage").await {
        Ok(k) => k,
        Err((c, m)) => return err(c, m),
    };
    // Acepta id o nombre.
    let target_id = if state.channel_manager.get_channel(&id_or_name).is_some() {
        id_or_name.clone()
    } else {
        match state
            .channel_manager
            .list_channels()
            .into_iter()
            .find(|c| c.name == id_or_name)
        {
            Some(c) => c.id,
            None => return err(StatusCode::NOT_FOUND, "Channel not found"),
        }
    };
    let name_lookup = state
        .channel_manager
        .get_channel(&target_id)
        .map(|c| c.name);
    let allowed = channel_allowed(&key, &target_id)
        || name_lookup
            .as_deref()
            .map(|n| channel_allowed(&key, n))
            .unwrap_or(false);
    if !allowed {
        return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
    }
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
    // Servicio-a-servicio: exige API key con scope publish.
    // (El JWT de navegador NO puede emitir directo salvo rol publish; el WS sí lo valida.)
    let mut key_opt: Option<ApiKey> = None;
    let mut jwt_ok = false;

    if let Ok(k) = require_api_key(&state, &headers, "publish").await {
        // Verificar que el canal objetivo esté en su prefijo (por id o nombre)
        let name_lookup = state
            .channel_manager
            .get_channel(&id)
            .map(|c| c.name.clone());
        let allowed = channel_allowed(&k, &id)
            || name_lookup
                .as_deref()
                .map(|n| channel_allowed(&k, n))
                .unwrap_or(false);
        if !allowed {
            return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
        }
        key_opt = Some(k);
    } else {
        // Fallback: JWT Bearer con rol publish
        match crate::middleware::require_jwt(&state, &headers, None).await {
            Ok(claims) => {
                if !claims.can_publish() {
                    return err(StatusCode::FORBIDDEN, "jwt sin rol publish");
                }
                let name_lookup = state
                    .channel_manager
                    .get_channel(&id)
                    .map(|c| c.name.clone());
                if !crate::middleware::jwt_can_access_channel(
                    &state,
                    &claims,
                    &id,
                    name_lookup.as_deref(),
                )
                .await
                {
                    return err(StatusCode::FORBIDDEN, "jwt no autoriza este canal");
                }
                jwt_ok = true;
            }
            Err((c, m)) => {
                return err(
                    c,
                    format!("requiere X-Api-Key (publish) o JWT publish: {}", m),
                )
            }
        }
    }
    let _ = (key_opt, jwt_ok);

    // Rate extra por IP emisora
    let ip_bucket = format!("emit:{}", client_ip(&headers));
    if !state.limiter.check(&ip_bucket, 600) {
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit por IP");
    }

    let source = if req.source.trim().is_empty() {
        "unknown".to_string()
    } else {
        req.source
    };
    let event = Event::new(id.clone(), source, req.targets, req.payload);
    match state.channel_manager.emit_event(&id, event) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::ok(json!({"success": true}))),
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
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
        Err((c, m)) => return err(c, m),
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
    if channel != "*" && !channel_allowed(&key, &channel) {
        return err(StatusCode::FORBIDDEN, "canal fuera de tu prefijo");
    }
    // El scope de la key debe cubrir el rol pedido
    if role.contains("publish") && !crate::keys::has_scope(&key, "publish") {
        return err(StatusCode::FORBIDDEN, "key sin scope publish");
    }
    if role.contains("subscribe") && !crate::keys::has_scope(&key, "subscribe") {
        // "both" contiene ambos substrings; chequeo simple:
        if role != "publish" && !crate::keys::has_scope(&key, "subscribe") {
            return err(StatusCode::FORBIDDEN, "key sin scope subscribe");
        }
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
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("jwt error: {}", e),
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
