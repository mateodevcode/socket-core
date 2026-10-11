use crate::auth::{bearer_from_header, decode_jwt, Claims, KEY_PREFIX};
use crate::handlers::AppState;
use crate::keys::{channel_allowed, has_scope, origin_allowed, ApiKey};
use axum::http::{HeaderMap, StatusCode};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let key = state
        .keys
        .validate(raw)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "api key inválida, revocada o expirada".into()))?;

    let origin = request_origin(headers);
    if !origin_allowed(&key, origin.as_deref()) {
        return Err((StatusCode::FORBIDDEN, "origen no permitido para esta key".into()));
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

/// Valida JWT Bearer (o ?token=) + revocación + canal/rol.
/// Si el token empieza con sc_live_ se trata como API key de servicio (puente backend).
pub async fn require_jwt(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Claims, (StatusCode, String)> {
    let token = bearer_from_header(headers, query_token)
        .ok_or((StatusCode::UNAUTHORIZED, "falta token Bearer".into()))?;

    // Puente servicio-a-servicio: permite usar la API key cruda como token WS
    if token.starts_with(KEY_PREFIX) {
        if let Some(key) = state.keys.validate(&token).await {
            let origin = request_origin(headers);
            if !origin_allowed(&key, origin.as_deref()) {
                return Err((StatusCode::FORBIDDEN, "origen no permitido".into()));
            }
            return Ok(Claims {
                sub: format!("svc:{}", key.name),
                project_id: key.id,
                project_name: key.name,
                channel: "*".into(),
                role: "both".into(),
                jti: "svc".into(),
                iat: 0,
                exp: i64::MAX,
            });
        }
        return Err((StatusCode::UNAUTHORIZED, "service key inválida".into()));
    }

    let claims = decode_jwt(&state.jwt_secret, &token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "jwt inválido o expirado".into()))?;
    if claims.jti != "svc" && state.keys.is_token_revoked(&claims.jti).await {
        return Err((StatusCode::UNAUTHORIZED, "token revocado".into()));
    }
    Ok(claims)
}

/// El JWT autoriza este canal si coincide o si es "*" dentro del prefijo de su proyecto.
/// Para validar prefijo necesitamos la key original; si el claim viene de una key
/// eliminada, igual limitamos por channel_prefix guardado en el token? No lo guardamos,
/// así que re-validamos: el canal del claim debe respetar el prefijo de la key viva.
/// Si la key ya no existe, solo permitimos si el token es reciente (defensa en profundidad
/// la hace la expiración corta + revocación).
pub async fn jwt_can_access_channel(
    state: &AppState,
    claims: &Claims,
    target_channel_id_or_name: &str,
    channel_name_lookup: Option<&str>,
) -> bool {
    // Puente servicio: acceso total (ya validó key)
    if claims.jti == "svc" {
        return true;
    }
    // Si el JWT está acotado a un canal concreto, debe coincidir (por id o nombre)
    if claims.channel != "*" {
        if claims.channel == target_channel_id_or_name {
            return true;
        }
        if let Some(n) = channel_name_lookup {
            if claims.channel == n || claims.channel == target_channel_id_or_name {
                return true;
            }
        }
        // Soporte "prefijo:*" dentro del propio JWT
        if claims.channel.ends_with(":*") {
            let p = &claims.channel[..claims.channel.len() - 2];
            if target_channel_id_or_name.starts_with(p)
                || channel_name_lookup.map(|n| n.starts_with(p)).unwrap_or(false)
            {
                return true;
            }
        }
        return false;
    }
    // JWT wildcard: verificar contra el prefijo vivo de la key
    let row = sqlx::query_as::<_, (String,)>("SELECT channel_prefix FROM api_keys WHERE id = ?")
        .bind(&claims.project_id)
        .fetch_optional(&state.keys.pool)
        .await
        .ok()
        .flatten();
    let Some((prefix_s,)) = row else { return false };
    let tmp = crate::keys::ApiKey {
        id: claims.project_id.clone(),
        name: claims.project_name.clone(),
        domains: vec!["*".into()],
        channel_prefixes: prefix_s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        scopes: vec![],
        rate_limit_per_min: 0,
        expires_at: None,
        revoked: false,
        created_at: 0,
    };
    channel_allowed(&tmp, target_channel_id_or_name)
        || channel_name_lookup.map(|n| channel_allowed(&tmp, n)).unwrap_or(false)
}
