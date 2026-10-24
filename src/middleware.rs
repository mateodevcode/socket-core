use crate::auth::{bearer_from_header, decode_jwt, Claims, KEY_PREFIX};
use crate::handlers::AppState;
use crate::keys::{has_scope, origin_allowed, ApiKey};
use axum::http::{HeaderMap, StatusCode};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Rate-limit fijo por ventana de 60s, clave = api_key_id o IP.
#[derive(Clone)]
pub struct RateLimiter {
    hits: Arc<DashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            hits: Arc::new(DashMap::new()),
        }
    }

    /// Purga entradas viejas (llamada por la tarea periódica de limpieza).
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.hits.retain(|_, (_, last)| {
            now.duration_since(*last) <= Duration::from_secs(crate::limits::RATE_LIMITER_TTL_SECS)
        });
    }

    /// Devuelve true si aún está dentro del límite.
    pub fn check(&self, bucket: &str, limit_per_min: i64) -> bool {
        if limit_per_min <= 0 {
            return true; // 0 = sin límite
        }
        let now = Instant::now();
        let mut entry = self.hits.entry(bucket.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) > Duration::from_secs(60) {
            entry.0 = 1;
            entry.1 = now;
            return true;
        }
        entry.0 += 1;
        entry.0 as i64 <= limit_per_min
    }
}

pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn request_origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

/// Exige MASTER_KEY (admin). Comparación simple; master via ENV, solo backend.
pub fn require_master(headers: &HeaderMap, master_key: &str) -> Result<(), (StatusCode, String)> {
    if master_key.is_empty() {
        return Err((StatusCode::FORBIDDEN, "MASTER_KEY no configurado".into()));
    }
    let got = headers
        .get("x-master-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if got != master_key {
        return Err((StatusCode::UNAUTHORIZED, "master key inválida".into()));
    }
    Ok(())
}

/// Valida X-Api-Key + origin + scope + rate-limit. Retorna la key.
pub async fn require_api_key(
    state: &AppState,
    headers: &HeaderMap,
    required_scope: &str,
) -> Result<ApiKey, (StatusCode, String)> {
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "falta X-Api-Key".into()));
    }
    let key = match state.keys.validate(raw).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                "api key inválida, revocada o expirada".into(),
            ))
        }
        Err(e) => {
            tracing::error!("no se pudo validar la api key: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "no se pudo verificar la credencial".into(),
            ));
        }
    };

    let origin = request_origin(headers);
    if !origin_allowed(&key, origin.as_deref()) {
        return Err((
            StatusCode::FORBIDDEN,
            "origen no permitido para esta key".into(),
        ));
    }
    if !required_scope.is_empty() && !has_scope(&key, required_scope) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("scope '{}' no permitido", required_scope),
        ));
    }
    let bucket = format!("key:{}", key.id);
    if !state.limiter.check(&bucket, key.rate_limit_per_min) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit excedido".into()));
    }
    // Rate global por IP como segunda barrera (600/min)
    let ip_bucket = format!("ip:{}", client_ip(headers));
    if !state.limiter.check(&ip_bucket, 600) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit por IP".into()));
    }
    Ok(key)
}

/// Valida JWT Bearer (o ?token=) + revocación de jti.
/// SIN puente de service-key: una API key `sc_live_...` nunca es un token de WS/Bearer.
/// Los backends usan REST con `X-Api-Key`; los frontends usan JWT.
pub async fn require_jwt(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Claims, (StatusCode, String)> {
    let token = bearer_from_header(headers, query_token)
        .ok_or((StatusCode::UNAUTHORIZED, "falta token Bearer".into()))?;

    if token.starts_with(KEY_PREFIX) {
        // Defensa explícita: una API key cruda no debe usarse como JWT/Bearer/WS.
        return Err((
            StatusCode::UNAUTHORIZED,
            "api keys no son tokens: usa X-Api-Key en REST o pide un JWT en /auth/token".into(),
        ));
    }

    let claims = decode_jwt(&state.jwt_secret, &token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "jwt inválido o expirado".into()))?;
    // FAIL-CLOSED: si no se puede comprobar la revocación, se deniega (500) y se loguea.
    match state.keys.is_token_revoked(&claims.jti).await {
        Ok(true) => return Err((StatusCode::UNAUTHORIZED, "token revocado".into())),
        Ok(false) => {}
        Err(e) => {
            tracing::error!("no se pudo comprobar revocación del jti: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "no se pudo verificar la credencial".into(),
            ));
        }
    }
    Ok(claims)
}
