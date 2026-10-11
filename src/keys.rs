use crate::auth::{generate_raw_key, hash_key};
use chrono::Utc;
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub key_hash: String,
    pub domains: String,
    pub channel_prefix: String,
    pub scopes: String,
    pub rate_limit_per_min: i64,
    pub expires_at: Option<i64>,
    pub revoked: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub domains: Vec<String>,
    pub channel_prefixes: Vec<String>,
    pub scopes: Vec<String>,
    pub rate_limit_per_min: i64,
    pub expires_at: Option<i64>,
    pub revoked: bool,
    pub created_at: i64,
}

impl From<ApiKeyRow> for ApiKey {
    fn from(r: ApiKeyRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            domains: split_csv(&r.domains),
            channel_prefixes: split_csv(&r.channel_prefix),
            scopes: split_csv(&r.scopes),
            rate_limit_per_min: r.rate_limit_per_min,
            expires_at: r.expires_at,
            revoked: r.revoked != 0,
            created_at: r.created_at,
        }
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

#[derive(Clone)]
pub struct KeyStore {
    pub pool: SqlitePool,
}

impl KeyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn count(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM api_keys")
            .fetch_one(&self.pool)
            .await
            .map(|r| r.0)
            .unwrap_or(0)
    }

    /// Crea key y devuelve (ApiKey público, raw_key secreta una sola vez).
    pub async fn create(
        &self,
        name: String,
        domains: Vec<String>,
        channel_prefix: String,
        scopes: Vec<String>,
        rate_limit_per_min: i64,
        expires_at: Option<i64>,
    ) -> Result<(ApiKey, String), String> {
        let raw = generate_raw_key();
        let hash = hash_key(&raw);
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let domains_s = if domains.is_empty() {
            "*".to_string()
        } else {
            domains.join(",")
        };
        let scopes_s = if scopes.is_empty() {
            "publish,subscribe".to_string()
        } else {
            scopes.join(",")
        };
        let prefix_s = if channel_prefix.trim().is_empty() {
            "*".to_string()
        } else {
            channel_prefix
        };

        sqlx::query(
            "INSERT INTO api_keys (id,name,key_hash,domains,channel_prefix,scopes,rate_limit_per_min,expires_at,revoked,created_at)
             VALUES (?,?,?,?,?,?,?,?,0,?)",
        )
        .bind(&id)
        .bind(&name)
        .bind(&hash)
        .bind(&domains_s)
        .bind(&prefix_s)
        .bind(&scopes_s)
        .bind(rate_limit_per_min)
        .bind(expires_at)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

        let key = ApiKey {
            id,
            name,
            domains: split_csv(&domains_s),
            channel_prefixes: split_csv(&prefix_s),
            scopes: split_csv(&scopes_s),
            rate_limit_per_min,
            expires_at,
            revoked: false,
            created_at: now,
        };
        Ok((key, raw))
    }

    pub async fn list(&self) -> Vec<ApiKey> {
        sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(ApiKey::from)
            .collect()
    }

    pub async fn revoke(&self, id: &str) -> bool {
        sqlx::query("UPDATE api_keys SET revoked = 1 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    pub async fn delete(&self, id: &str) -> bool {
        sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    /// Valida key cruda: hash, revocada, expirada.
    pub async fn validate(&self, raw_key: &str) -> Option<ApiKey> {
        let hash = hash_key(raw_key);
        let row = sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys WHERE key_hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        let key = ApiKey::from(row);
        if key.revoked {
            return None;
        }
        if let Some(exp) = key.expires_at {
            if Utc::now().timestamp() > exp {
                return None;
            }
        }
        Some(key)
    }

    pub async fn is_token_revoked(&self, jti: &str) -> bool {
        sqlx::query_as::<_, (String,)>("SELECT jti FROM revoked_tokens WHERE jti = ?")
            .bind(jti)
            .fetch_optional(&self.pool)
            .await
            .map(|r| r.is_some())
            .unwrap_or(false)
    }

    pub async fn revoke_token(&self, jti: &str, exp: i64) -> bool {
        sqlx::query("INSERT OR IGNORE INTO revoked_tokens (jti, exp) VALUES (?,?)")
            .bind(jti)
            .bind(exp)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }
}

// ---- Policy helpers (puros, testeables) ----

/// Origin/Referer permitido para esta key.
/// domains puede contener "*", exactos "https://app.com" o wildcard "*.midominio.com".
pub fn origin_allowed(key: &ApiKey, origin: Option<&str>) -> bool {
    if key.domains.iter().any(|d| d == "*") {
        return true;
    }
    let Some(mut o) = origin.map(|s| s.to_string()) else {
        return true; // backend-to-backend sin Origin: se permite (ya exige key)
    };
    o = o.trim_end_matches('/').to_lowercase();
    // Si viene "https://app.com/path", reducimos a esquema+host para comparar
    let origin_host = host_of(&o);
    for d in &key.domains {
        let dd = d.trim().trim_end_matches('/').to_lowercase();
        if dd == "*" {
            return true;
        }
        if o == dd || o.starts_with(&format!("{}/", dd)) {
            return true;
        }
        // wildcard subdominio
        if let Some(suffix) = dd.strip_prefix("*.") {
            if origin_host == suffix || origin_host.ends_with(&format!(".{}", suffix)) {
                return true;
            }
        }
        // comparar solo host si el dominio configurado no trae esquema
        if !dd.contains("://") && (origin_host == dd || origin_host.ends_with(&format!(".{}", dd))) {
            return true;
        }
    }
    false
}

fn host_of(url_or_origin: &str) -> String {
    let s = url_or_origin
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    s.split('/').next().unwrap_or(s).split(':').next().unwrap_or(s).to_string()
}

/// Canal permitido: prefixes ["*"] o ["monitoreo:*","jarvis:*"] o exactos.
pub fn channel_allowed(key: &ApiKey, channel: &str) -> bool {
    for p in &key.channel_prefixes {
        if p == "*" {
            return true;
        }
        if p.ends_with(":*") {
            let prefix = &p[..p.len() - 2];
            if channel == prefix || channel.starts_with(&format!("{}:", prefix)) {
                return true;
            }
        } else if p == channel {
            return true;
        }
    }
    false
}

pub fn has_scope(key: &ApiKey, scope: &str) -> bool {
    key.scopes.iter().any(|s| s == scope || s == "admin" || s == "*")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(domains: &str, prefix: &str, scopes: &str) -> ApiKey {
        ApiKey {
            id: "x".into(),
            name: "t".into(),
            domains: split_csv(domains),
            channel_prefixes: split_csv(prefix),
            scopes: split_csv(scopes),
            rate_limit_per_min: 60,
            expires_at: None,
            revoked: false,
            created_at: 0,
        }
    }

    #[test]
    fn origins() {
        assert!(origin_allowed(&k("*", "*", "*"), Some("https://evil.com")));
        assert!(origin_allowed(&k("*", "*", "*"), None));
        let key = k("https://panel.midominio.com", "*", "*");
        assert!(origin_allowed(&key, Some("https://panel.midominio.com")));
        assert!(!origin_allowed(&key, Some("https://evil.com")));
        assert!(origin_allowed(&key, None)); // backend sin Origin
        let wild = k("*.midominio.com", "*", "*");
        assert!(origin_allowed(&wild, Some("https://app.midominio.com")));
        assert!(!origin_allowed(&wild, Some("https://otro.com")));
    }

    #[test]
    fn channels() {
        let key = k("*", "monitoreo:*", "publish,subscribe");
        assert!(channel_allowed(&key, "monitoreo:alertas"));
        assert!(!channel_allowed(&key, "jarvis:x"));
        assert!(channel_allowed(&k("*", "*", "*"), "cualquiera"));
        assert!(!has_scope(&key, "manage"));
        assert!(has_scope(&key, "publish"));
    }
}
