use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const KEY_PREFIX: &str = "sc_live_";

/// Claims del JWT de corta vida emitido por socket-core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,          // user_id / client_id
    pub project_id: String,   // api_keys.id
    pub project_name: String, // api_keys.name
    pub channel: String,      // canal autorizado o "*" dentro del prefijo
    pub role: String,         // "publish" | "subscribe" | "both"
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
}

impl Claims {
    pub fn can_publish(&self) -> bool {
        self.role == "publish" || self.role == "both" || self.role == "admin"
    }
    pub fn can_subscribe(&self) -> bool {
        self.role == "subscribe" || self.role == "both" || self.role == "admin"
    }
}

pub fn hash_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    hex_encode(h.finalize().as_slice())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Genera una API key cruda (solo se muestra UNA vez, se guarda el hash).
pub fn generate_raw_key() -> String {
    use rand::distributions::{Alphanumeric, DistString};
    let mut rng = rand::thread_rng();
    let suffix = Alphanumeric.sample_string(&mut rng, 40);
    format!("{}{}", KEY_PREFIX, suffix)
}

pub fn create_jwt(
    secret: &str,
    ttl_secs: i64,
    project_id: &str,
    project_name: &str,
    user_id: &str,
    channel: &str,
    role: &str,
) -> Result<(String, String), jsonwebtoken::errors::Error> {
    let now = Utc::now().timestamp();
    let jti = Uuid::new_v4().to_string();
    let claims = Claims {
        sub: user_id.to_string(),
        project_id: project_id.to_string(),
        project_name: project_name.to_string(),
        channel: channel.to_string(),
        role: role.to_string(),
        jti: jti.clone(),
        iat: now,
        exp: now + ttl_secs,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, jti))
}

pub fn decode_jwt(secret: &str, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::default();
    validation.validate_exp = true;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}

/// Extrae token de `Authorization: Bearer <t>` o query `?token=<t>`.
pub fn bearer_from_header(
    headers: &axum::http::HeaderMap,
    query_token: Option<&str>,
) -> Option<String> {
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(t) = s.strip_prefix("Bearer ") {
                return Some(t.trim().to_string());
            }
        }
    }
    // WS / REST también aceptan X-Access-Token como alternativa al query
    if let Some(t) = headers.get("x-access-token") {
        if let Ok(s) = t.to_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    query_token.map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_roundtrip_and_roles() {
        let (tok, _) =
            create_jwt("secret-test", 600, "p1", "demo", "u1", "demo:a", "both").unwrap();
        let c = decode_jwt("secret-test", &tok).unwrap();
        assert!(c.can_publish() && c.can_subscribe());
        let (tok2, _) =
            create_jwt("secret-test", 600, "p1", "demo", "u1", "*", "subscribe").unwrap();
        let c2 = decode_jwt("secret-test", &tok2).unwrap();
        assert!(!c2.can_publish() && c2.can_subscribe());
        assert!(decode_jwt("otra-secret", &tok).is_err());
    }

    #[test]
    fn key_format_and_hash() {
        let raw = generate_raw_key();
        assert!(raw.starts_with(KEY_PREFIX));
        assert_eq!(hash_key(&raw).len(), 64);
    }
}
