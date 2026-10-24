use crate::auth::{bearer_from_header, decode_jwt, Claims, KEY_PREFIX};
use crate::handlers::AppState;
use crate::keys::{has_scope, origin_allowed, ApiKey};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use dashmap::DashMap;
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

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

/// IP del cliente ya resuelta (socket + proxies de confianza).
#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

#[axum::async_trait]
impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        Ok(ClientIp(resolve_client_ip(
            peer,
            &parts.headers,
            &state.trusted_proxies,
        )))
    }
}

/// Solo si el par (IP del socket) es un proxy de confianza se leen XFF/X-Real-IP.
/// De XFF se usa el ÚLTIMO valor (el añadido por el proxy de confianza), nunca el primero.
pub fn resolve_client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    let trusted_peer = trusted.iter().any(|net| net.contains(&peer));
    if !trusted_peer {
        return peer;
    }
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.split(',').map(|s| s.trim()).rfind(|s| !s.is_empty()) {
            if let Ok(ip) = last.parse::<IpAddr>() {
                return ip;
            }
        }
    }
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer
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

/// Comparación en tiempo constante (hash de longitud fija + subtle).
pub fn secure_compare(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    bool::from(ha.as_slice().ct_eq(hb.as_slice()))
}

/// Estado del bloqueo temporal del admin por IP tras fallos de MASTER_KEY.
#[derive(Clone, Default)]
pub struct AdminGuard {
    state: Arc<DashMap<String, (u32, Option<Instant>)>>,
}

impl AdminGuard {
    pub fn new() -> Self {
        Self::default()
    }

    fn is_locked(&self, ip: &str) -> bool {
        match self.state.get(ip) {
            Some(entry) => match entry.1 {
                Some(until) => Instant::now() < until,
                None => false,
            },
            None => false,
        }
    }

    fn record_failure(&self, ip: &str, fail_max: u32, lock_secs: u64) {
        let mut entry = self.state.entry(ip.to_string()).or_insert((0, None));
        entry.0 = entry.0.saturating_add(1);
        if entry.0 >= fail_max {
            entry.1 = Some(Instant::now() + Duration::from_secs(lock_secs));
            entry.0 = 0;
        }
    }

    fn record_success(&self, ip: &str) {
        self.state.remove(ip);
    }

    pub fn cleanup(&self) {
        let now = Instant::now();
        self.state
            .retain(|_, (_, until)| until.map(|u| now < u).unwrap_or(true));
    }
}

/// Exige MASTER_KEY en el listener admin: rate limit de fallos + bloqueo temporal
/// + comparación en tiempo constante. Los errores no distinguen causa.
pub fn require_master(
    state: &AppState,
    headers: &HeaderMap,
    ip: IpAddr,
) -> Result<(), (StatusCode, String)> {
    if state.master_key.is_empty() {
        state
            .audit
            .record(None, "admin_auth_failed", "master_key_no_configurada");
        return Err((StatusCode::FORBIDDEN, "forbidden".to_string()));
    }
    let ip_key = ip.to_string();
    if state.admin_guard.is_locked(&ip_key) {
        state
            .audit
            .record(None, "admin_auth_blocked", "bloqueo_temporal");
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts".to_string(),
        ));
    }
    let got = headers
        .get("x-master-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !secure_compare(got, &state.master_key) {
        state.admin_guard.record_failure(
            &ip_key,
            state.limits.admin_fail_max,
            state.limits.admin_lock_secs,
        );
        state
            .audit
            .record(None, "admin_auth_failed", "master_key_invalida");
        return Err((StatusCode::UNAUTHORIZED, "unauthorized".to_string()));
    }
    state.admin_guard.record_success(&ip_key);
    Ok(())
}

/// Valida rate-limit por IP (ANTES de la key) + X-Api-Key + origin + scope + rate-limit de key.
pub async fn require_api_key(
    state: &AppState,
    headers: &HeaderMap,
    required_scope: &str,
    ip: IpAddr,
) -> Result<ApiKey, (StatusCode, String)> {
    // 1) Límite por IP primero: las keys inválidas también consumen cupo.
    let ip_bucket = format!("ip:{}", ip);
    if !state
        .limiter
        .check(&ip_bucket, state.limits.ip_rate_per_min)
    {
        state.audit.record(None, "rate_limited", "ip");
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit por IP".into()));
    }

    // 2) Validación de la key.
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        state.audit.record(None, "auth_failed", "falta_api_key");
        return Err((StatusCode::UNAUTHORIZED, "falta X-Api-Key".into()));
    }
    let key = match state.keys.validate(raw).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            state.audit.record(None, "auth_failed", "api_key_invalida");
            return Err((
                StatusCode::UNAUTHORIZED,
                "api key inválida, revocada o expirada".into(),
            ));
        }
        Err(e) => {
            tracing::error!("no se pudo validar la api key: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "no se pudo verificar la credencial".into(),
            ));
        }
    };

    // 3) Origen (domains) y scope.
    let origin = request_origin(headers);
    if !origin_allowed(&key, origin.as_deref()) {
        state
            .audit
            .record(Some(&key.id), "auth_failed", "origen_no_permitido");
        return Err((
            StatusCode::FORBIDDEN,
            "origen no permitido para esta key".into(),
        ));
    }
    if !required_scope.is_empty() && !has_scope(&key, required_scope) {
        state
            .audit
            .record(Some(&key.id), "auth_failed", "scope_insuficiente");
        return Err((
            StatusCode::FORBIDDEN,
            format!("scope '{}' no permitido", required_scope),
        ));
    }

    // 4) Límite por key.
    let bucket = format!("key:{}", key.id);
    if !state.limiter.check(&bucket, key.rate_limit_per_min) {
        state.audit.record(Some(&key.id), "rate_limited", "key");
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit excedido".into()));
    }
    Ok(key)
}

/// Valida JWT Bearer (o ?token=) + revocación de jti.
/// SIN puente de service-key: una API key `sc_live_...` nunca es un token de WS/Bearer.
pub async fn require_jwt(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Claims, (StatusCode, String)> {
    let token = bearer_from_header(headers, query_token)
        .ok_or((StatusCode::UNAUTHORIZED, "falta token Bearer".into()))?;

    if token.starts_with(KEY_PREFIX) {
        state
            .audit
            .record(None, "auth_failed", "api_key_como_token");
        return Err((
            StatusCode::UNAUTHORIZED,
            "api keys no son tokens: usa X-Api-Key en REST o pide un JWT en /auth/token".into(),
        ));
    }

    let claims = match decode_jwt(&state.jwt_secret, &token) {
        Ok(c) => c,
        Err(_) => {
            state.audit.record(None, "auth_failed", "jwt_invalido");
            return Err((StatusCode::UNAUTHORIZED, "jwt inválido o expirado".into()));
        }
    };
    // FAIL-CLOSED: si no se puede comprobar la revocación, se deniega (500) y se loguea.
    match state.keys.is_token_revoked(&claims.jti).await {
        Ok(true) => {
            state
                .audit
                .record(Some(&claims.project_id), "auth_failed", "jwt_revocado");
            return Err((StatusCode::UNAUTHORIZED, "token revocado".into()));
        }
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
