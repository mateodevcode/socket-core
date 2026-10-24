use crate::auth::{generate_raw_key, hash_key};
use crate::control::{ControlBus, ControlEvent};
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
    pub control: ControlBus,
}

impl KeyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            control: ControlBus::new(),
        }
    }

    pub fn new_with_control(pool: SqlitePool, control: ControlBus) -> Self {
        Self { pool, control }
    }

    /// Número de keys registradas. Fail-closed: propaga el error de DB.
    pub async fn count(&self) -> Result<i64, sqlx::Error> {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM api_keys")
            .fetch_one(&self.pool)
            .await
            .map(|r| r.0)
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

    pub async fn list(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        let rows =
            sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys ORDER BY created_at DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(ApiKey::from).collect())
    }

    /// Revoca una key. Cierra sus WS abiertos al instante (bus de control).
    pub async fn revoke(&self, id: &str) -> Result<bool, sqlx::Error> {
        let affected = sqlx::query("UPDATE api_keys SET revoked = 1 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0;
        if affected {
            self.control
                .send(ControlEvent::CloseProject(id.to_string()));
        }
        Ok(affected)
    }

    /// Borra una key. Cierra sus WS abiertos al instante (bus de control).
    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let affected = sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0;
        if affected {
            self.control
                .send(ControlEvent::CloseProject(id.to_string()));
        }
        Ok(affected)
    }

    /// Edita solo los campos presentes. Devuelve la key actualizada o error.
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        id: &str,
        name: Option<String>,
        domains: Option<Vec<String>>,
        channel_prefix: Option<String>,
        scopes: Option<Vec<String>>,
        rate_limit_per_min: Option<i64>,
        expires_in_days: Option<i64>,
    ) -> Result<ApiKey, String> {
        if name.is_none()
            && domains.is_none()
            && channel_prefix.is_none()
            && scopes.is_none()
            && rate_limit_per_min.is_none()
            && expires_in_days.is_none()
        {
            return Err("nada que actualizar".into());
        }
        if let Some(n) = &name {
            if n.trim().is_empty() {
                return Err("name vacío".into());
            }
        }
        if let Some(r) = rate_limit_per_min {
            if r < 0 {
                return Err("rate_limit_per_min inválido".into());
            }
        }
        let expires_at = expires_in_days.map(|d| {
            if d <= 0 {
                None
            } else {
                Some(chrono::Utc::now().timestamp() + d * 86400)
            }
        });
        // Si cambian prefijo o scopes, las conexiones abiertas deben reevaluarse:
        // se cierran (los clientes reconectan con credenciales vigentes).
        let security_changed = channel_prefix.is_some() || scopes.is_some();
        // expires_in_days: Some(None) = quitar expiración; None = no tocar.
        // Se resuelve con CASE para no necesitar SQL dinámico.
        let touch_exp = expires_in_days.is_some();
        let exp_val = expires_at.flatten();
        // Normalizar vacíos como en create (un [] no debe bloquearlo todo).
        let domains_s = domains.map(|v| {
            let j = v
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            if j.is_empty() {
                "*".to_string()
            } else {
                j
            }
        });
        let prefix_s = channel_prefix.map(|s| {
            if s.trim().is_empty() {
                "*".to_string()
            } else {
                s
            }
        });
        let scopes_s = scopes.map(|v| {
            let j = v
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            if j.is_empty() {
                "publish,subscribe".to_string()
            } else {
                j
            }
        });

        let res = sqlx::query(
            "UPDATE api_keys SET
                name = COALESCE(?, name),
                domains = COALESCE(?, domains),
                channel_prefix = COALESCE(?, channel_prefix),
                scopes = COALESCE(?, scopes),
                rate_limit_per_min = COALESCE(?, rate_limit_per_min),
                expires_at = CASE WHEN ? THEN ? ELSE expires_at END
             WHERE id = ?",
        )
        .bind(name)
        .bind(domains_s)
        .bind(prefix_s)
        .bind(scopes_s)
        .bind(rate_limit_per_min)
        .bind(touch_exp)
        .bind(exp_val)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
        if res.rows_affected() == 0 {
            return Err("key no encontrada".into());
        }
        if security_changed {
            self.control
                .send(ControlEvent::CloseProject(id.to_string()));
        }
        match self.by_id(id).await {
            Ok(Some(k)) => Ok(k),
            Ok(None) => Err("key no encontrada".into()),
            Err(e) => {
                tracing::error!("update: error leyendo key {}: {}", id, e);
                Err("internal error".into())
            }
        }
    }

    async fn by_id(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        Ok(
            sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .map(ApiKey::from),
        )
    }

    /// Valida key cruda: hash, revocada, expirada.
    /// FAIL-CLOSED: los errores de DB se propagan (el llamante responde 500, no 401).
    pub async fn validate(&self, raw_key: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let hash = hash_key(raw_key);
        let row = sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys WHERE key_hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let key = ApiKey::from(row);
        if key.revoked {
            return Ok(None);
        }
        if let Some(exp) = key.expires_at {
            if Utc::now().timestamp() > exp {
                return Ok(None);
            }
        }
        Ok(Some(key))
    }

    /// Comprueba si el prefijo indicado solapa con alguna otra key.
    /// "*" solapa con todo; "a:*" solapa con "a:*", "a:b:*", "a", etc.
    pub async fn prefix_conflicts(
        &self,
        prefix_s: &str,
        exclude_id: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let wanted = prefix_bases(prefix_s);
        if wanted.is_empty() {
            return Ok(false);
        }
        let rows = sqlx::query_as::<_, (String, String)>("SELECT id, channel_prefix FROM api_keys")
            .fetch_all(&self.pool)
            .await?;
        for (id, existing) in rows {
            if Some(id.as_str()) == exclude_id {
                continue;
            }
            for b in prefix_bases(&existing) {
                for w in &wanted {
                    if bases_overlap(w, &b) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Comprueba que el proyecto (key) sigue existiendo, activo y vigente.
    /// Fail-closed: propaga errores de DB para que el llamante deniegue.
    pub async fn is_project_active(&self, id: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query_as::<_, (i64, Option<i64>)>(
            "SELECT revoked, expires_at FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((revoked, expires_at)) = row else {
            return Ok(false);
        };
        if revoked != 0 {
            return Ok(false);
        }
        if let Some(exp) = expires_at {
            if Utc::now().timestamp() > exp {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// FAIL-CLOSED: si la DB no responde, el error se propaga y la autenticación
    /// se deniega (500) en vez de asumir "no revocado".
    pub async fn is_token_revoked(&self, jti: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query_as::<_, (String,)>("SELECT jti FROM revoked_tokens WHERE jti = ?")
            .bind(jti)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Revoca un jti y cierra las conexiones WS que lo usan.
    pub async fn revoke_token(&self, jti: &str, exp: i64) -> Result<bool, sqlx::Error> {
        let inserted = sqlx::query("INSERT OR IGNORE INTO revoked_tokens (jti, exp) VALUES (?,?)")
            .bind(jti)
            .bind(exp)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0;
        if inserted {
            self.control.send(ControlEvent::CloseJti(jti.to_string()));
        }
        Ok(inserted)
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
        if !dd.contains("://") && (origin_host == dd || origin_host.ends_with(&format!(".{}", dd)))
        {
            return true;
        }
    }
    false
}

fn host_of(url_or_origin: &str) -> String {
    let s = url_or_origin
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    s.split('/')
        .next()
        .unwrap_or(s)
        .split(':')
        .next()
        .unwrap_or(s)
        .to_string()
}

fn prefix_bases(prefix_s: &str) -> Vec<String> {
    prefix_s
        .split(',')
        .map(|p| p.trim().trim_end_matches(":*").trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

fn bases_overlap(a: &str, b: &str) -> bool {
    if a == "*" || b == "*" {
        return true;
    }
    if a == b {
        return true;
    }
    let a_ns = format!("{}:", a);
    let b_ns = format!("{}:", b);
    b.starts_with(&a_ns) || a.starts_with(&b_ns)
}

/// Canal permitido: prefixes ["*"] o ["a:*","jarvis:*"] o exactos.
/// Estricto: "a:*" NO acepta "a" (sin segmento), "a:" (segmento vacío),
/// "a2:x" (otro prefijo) ni "A:x" (mayúsculas).
pub fn channel_allowed(key: &ApiKey, channel: &str) -> bool {
    for p in &key.channel_prefixes {
        if p == "*" {
            return true;
        }
        if p.ends_with(":*") {
            let prefix = &p[..p.len() - 2];
            if prefix.is_empty() {
                continue; // ":*" no es un prefijo válido
            }
            if let Some(suffix) = channel.strip_prefix(prefix) {
                // debe seguir el separador ':' y un segmento no vacío
                if let Some(sfx) = suffix.strip_prefix(':') {
                    if !sfx.is_empty() {
                        return true;
                    }
                }
            }
        } else if p == channel {
            return true;
        }
    }
    false
}

pub fn has_scope(key: &ApiKey, scope: &str) -> bool {
    key.scopes
        .iter()
        .any(|s| s == scope || s == "admin" || s == "*")
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

    #[test]
    fn policies_estrictas_de_prefijo() {
        let key = k("*", "a:*", "publish,subscribe");
        assert!(channel_allowed(&key, "a:x"));
        assert!(!channel_allowed(&key, "a2:x")); // otro prefijo
        assert!(!channel_allowed(&key, "a")); // sin segmento
        assert!(!channel_allowed(&key, "A:x")); // mayúsculas
        assert!(!channel_allowed(&key, "a:")); // segmento vacío
        assert!(!channel_allowed(&key, "x:a")); // prefijo al final
        assert!(!channel_allowed(&key, "aa:x")); // prefijo pegado
    }
}
