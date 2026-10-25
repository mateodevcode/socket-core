use crate::auth::{bearer_from_header, decode_jwt, Claims, KEY_PREFIX};
use crate::handlers::AppState;
use crate::keys::{has_scope, origin_allowed, ApiKey};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use dashmap::DashMap;
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
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
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(conn) = parts.extensions.get::<ConnectInfo<SocketAddr>>() else {
            tracing::error!(
                "petición sin ConnectInfo: el servidor no se está sirviendo con into_make_service_with_connect_info"
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        };
        Ok(ClientIp(resolve_client_ip(
            conn.0.ip(),
            &parts.headers,
            &state.trusted_proxies,
        )))
    }
}

/// Solo si el par (IP del socket) es un proxy de confianza se leen XFF/X-Real-IP.
/// De XFF se usa el ÚLTIMO valor (el añadido por el proxy de confianza), nunca el primero.
pub fn resolve_client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    // Normaliza ::ffff:a.b.c.d a IPv4 para comparar y agrupar correctamente.
    let peer = peer.to_canonical();
    let trusted_peer = trusted.iter().any(|net| net.contains(&peer));
    if !trusted_peer {
        return peer;
    }
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.split(',').map(|s| s.trim()).rfind(|s| !s.is_empty()) {
            if let Ok(ip) = last.parse::<IpAddr>() {
                return ip.to_canonical();
            }
        }
    }
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
    {
        return ip.to_canonical();
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
#[derive(Clone, Copy)]
struct AdminFail {
    count: u32,
    locked_until: Option<Instant>,
    last: Instant,
}

#[derive(Clone, Default)]
pub struct AdminGuard {
    state: Arc<DashMap<String, AdminFail>>,
}

impl AdminGuard {
    pub fn new() -> Self {
        Self::default()
    }

    fn is_locked(&self, ip: &str) -> bool {
        match self.state.get(ip) {
            Some(entry) => match entry.locked_until {
                Some(until) => Instant::now() < until,
                None => false,
            },
            None => false,
        }
    }

    fn record_failure(&self, ip: &str, fail_max: u32, lock_secs: u64) {
        let now = Instant::now();
        let mut entry = self.state.entry(ip.to_string()).or_insert(AdminFail {
            count: 0,
            locked_until: None,
            last: now,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last = now;
        if entry.count >= fail_max {
            entry.locked_until = Some(now + Duration::from_secs(lock_secs));
            entry.count = 0;
        }
    }

    /// Número de fallos acumulados sin bloqueo (para tests/observabilidad).
    pub fn failure_count(&self, ip: &str) -> u32 {
        self.state.get(ip).map(|e| e.count).unwrap_or(0)
    }

    /// Purga contadores sin bloqueo más viejos que `ttl`; mantiene bloqueos activos.
    pub fn cleanup_with_ttl(&self, ttl: Duration) {
        let now = Instant::now();
        self.state.retain(|_, e| match e.locked_until {
            Some(u) => now < u,
            None => now.duration_since(e.last) <= ttl,
        });
    }

    fn record_success(&self, ip: &str) {
        self.state.remove(ip);
    }

    pub fn cleanup(&self) {
        self.cleanup_with_ttl(Duration::from_secs(crate::limits::ADMIN_FAIL_TTL_SECS));
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
        state
            .audit
            .record(None, "rate_limited", &format!("ip={} motivo=ip", ip));
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limit por IP".into()));
    }

    // 2) Validación de la key.
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        state.audit.record(
            None,
            "auth_failed",
            &format!("ip={} motivo=falta_api_key", ip),
        );
        return Err((StatusCode::UNAUTHORIZED, "falta X-Api-Key".into()));
    }
    let key = match state.keys.validate(raw).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            state.audit.record(
                None,
                "auth_failed",
                &format!("ip={} motivo=api_key_invalida", ip),
            );
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
        state.audit.record(
            Some(&key.id),
            "auth_failed",
            &format!("ip={} motivo=origen_no_permitido", ip),
        );
        return Err((
            StatusCode::FORBIDDEN,
            "origen no permitido para esta key".into(),
        ));
    }
    if !required_scope.is_empty() && !has_scope(&key, required_scope) {
        state.audit.record(
            Some(&key.id),
            "auth_failed",
            &format!("ip={} motivo=scope_insuficiente", ip),
        );
        return Err((
            StatusCode::FORBIDDEN,
            format!("scope '{}' no permitido", required_scope),
        ));
    }

    // 4) Límite por key.
    let bucket = format!("key:{}", key.id);
    if !state.limiter.check(&bucket, key.rate_limit_per_min) {
        state.audit.record(
            Some(&key.id),
            "rate_limited",
            &format!("ip={} motivo=key", ip),
        );
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
    ip: IpAddr,
) -> Result<Claims, (StatusCode, String)> {
    let token = bearer_from_header(headers, query_token)
        .ok_or((StatusCode::UNAUTHORIZED, "falta token Bearer".into()))?;

    if token.starts_with(KEY_PREFIX) {
        state.audit.record(
            None,
            "auth_failed",
            &format!("ip={} motivo=api_key_como_token", ip),
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            "api keys no son tokens: usa X-Api-Key en REST o pide un JWT en /auth/token".into(),
        ));
    }

    let claims = match decode_jwt(&state.jwt_secret, &token) {
        Ok(c) => c,
        Err(_) => {
            state.audit.record(
                None,
                "auth_failed",
                &format!("ip={} motivo=jwt_invalido", ip),
            );
            return Err((StatusCode::UNAUTHORIZED, "jwt inválido o expirado".into()));
        }
    };
    // FAIL-CLOSED: si no se puede comprobar la revocación, se deniega (500) y se loguea.
    match state.keys.is_token_revoked(&claims.jti).await {
        Ok(true) => {
            state.audit.record(
                Some(&claims.project_id),
                "auth_failed",
                &format!("ip={} motivo=jwt_revocado", ip),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn normaliza_ipv4_mapeada_en_ipv6() {
        let peer: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let trusted: Vec<IpNet> = vec!["127.0.0.1/32".parse().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("::ffff:10.1.2.3"),
        );
        assert_eq!(
            resolve_client_ip(peer, &headers, &trusted),
            "10.1.2.3".parse::<IpAddr>().unwrap(),
            "XFF mapeado debe normalizarse y confiar en el peer canónico"
        );
        // Sin proxy de confianza, se devuelve el peer (también canónico).
        let sin: Vec<IpNet> = vec![];
        assert_eq!(
            resolve_client_ip(peer, &headers, &sin),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }
}

#[cfg(test)]
mod admin_guard_tests {
    use super::*;

    #[test]
    fn cleanup_purga_contadores_sin_bloqueo_por_ttl() {
        let g = AdminGuard::new();
        g.record_failure("1.2.3.4", 5, 60);
        assert_eq!(g.failure_count("1.2.3.4"), 1);
        g.cleanup_with_ttl(Duration::ZERO);
        assert_eq!(
            g.failure_count("1.2.3.4"),
            0,
            "un contador sin bloqueo debe purgarse al superar el TTL"
        );

        // Un bloqueo activo NO se purga por TTL 0.
        for _ in 0..5 {
            g.record_failure("5.6.7.8", 5, 60);
        }
        g.cleanup_with_ttl(Duration::ZERO);
        assert!(g.is_locked("5.6.7.8"), "el bloqueo activo debe mantenerse");
    }
}
