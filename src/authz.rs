// Autorización central de canales: UNA sola función decide por (proyecto, canal, acción).
// Ningún endpoint ni el WS pueden saltarla ni mantener atajos paralelos.
use crate::auth::Claims;
use crate::channel_manager::ChannelManager;
use crate::keys::{channel_allowed, has_scope, ApiKey};
use axum::http::StatusCode;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelAction {
    /// Ver canal, clientes y metadata (lectura).
    View,
    /// Crear/borrar el canal.
    Manage,
    /// Emitir eventos.
    Publish,
    /// Suscribirse por WS.
    Subscribe,
}

/// Error de autorización SIN filtrar detalles (uso interno):
/// NotFound-intencional para canales que no existen O no son del proyecto.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AuthzError {
    NotFound,
    Forbidden,
    /// La key/credencial que respalda al proyecto ya no está viva (revocada/borrada/expirada).
    InvalidCredential,
    /// Fallo interno al comprobar la credencial (DB). NUNCA se convierte en acceso.
    Internal,
}

impl AuthzError {
    pub fn status(&self) -> (StatusCode, &'static str) {
        match self {
            // Slot ajeno e inexistente responden igual: mismo 404 (no filtra existencia).
            AuthzError::NotFound => (StatusCode::NOT_FOUND, "Channel not found"),
            AuthzError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            AuthzError::InvalidCredential => {
                (StatusCode::UNAUTHORIZED, "credential invalid or revoked")
            }
            AuthzError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }
}

/// Rama con API key: dueño real (project_id) del canal. `channel_allowed` es
/// defensa en profundidad, nunca una fuente de verdad alternativa.
pub fn authorize_channel_for_key(
    cm: &ChannelManager,
    key: &ApiKey,
    channel_id_or_name: &str,
    action: ChannelAction,
) -> Result<String, AuthzError> {
    let Some(ch) = cm.resolve_channel(channel_id_or_name) else {
        return Err(AuthzError::NotFound);
    };
    if ch.project_id != key.id {
        return Err(AuthzError::NotFound);
    }
    if !channel_allowed(key, &ch.name) {
        return Err(AuthzError::Forbidden);
    }
    check_action_for_key(key, action)?;
    Ok(ch.id)
}

fn check_action_for_key(key: &ApiKey, action: ChannelAction) -> Result<(), AuthzError> {
    let (scope, required) = match action {
        ChannelAction::View => ("", false),
        ChannelAction::Manage => ("manage", true),
        ChannelAction::Publish => ("publish", true),
        ChannelAction::Subscribe => ("subscribe", true),
    };
    if required && !crate::keys::has_scope(key, scope) {
        return Err(AuthzError::Forbidden);
    }
    Ok(())
}

/// Rama con JWT: comprueba (1) key viva que respalda el token, (2) proyecto dueño,
/// (3) rol del claim, (4) scope de canal del claim.
pub async fn authorize_channel_for_jwt(
    cm: &ChannelManager,
    pool: &SqlitePool,
    claims: &Claims,
    channel_id_or_name: &str,
    action: ChannelAction,
) -> Result<String, AuthzError> {
    // La key que respaldó al JWT debe seguir viva: revocada, borrada o expirada => no vale.
    let row = sqlx::query_as::<_, (i64, Option<i64>, String, String)>(
        "SELECT revoked, expires_at, channel_prefix, scopes FROM api_keys WHERE id = ?",
    )
    .bind(&claims.project_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!("authz: error consultando key {}: {}", claims.project_id, e);
        AuthzError::Internal
    })?;
    let (revoked, expires_at, live_prefix, live_scopes) =
        row.ok_or(AuthzError::InvalidCredential)?;
    if revoked != 0 {
        return Err(AuthzError::InvalidCredential);
    }
    if let Some(exp) = expires_at {
        if chrono::Utc::now().timestamp() > exp {
            return Err(AuthzError::InvalidCredential);
        }
    }

    let Some(ch) = cm.resolve_channel(channel_id_or_name) else {
        return Err(AuthzError::NotFound);
    };
    if ch.project_id != claims.project_id {
        return Err(AuthzError::NotFound);
    }

    // Validación contra el PREFIJO VIVO de la key (si se recortó, los JWT viejos mueren).
    let live_key = ApiKey {
        id: claims.project_id.clone(),
        name: claims.project_name.clone(),
        domains: vec!["*".into()],
        channel_prefixes: live_prefix
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        scopes: live_scopes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        rate_limit_per_min: 0,
        expires_at: None,
        revoked: false,
        created_at: 0,
    };
    if !channel_allowed(&live_key, &ch.name) {
        return Err(AuthzError::Forbidden);
    }

    // Rol del claim + SCOPES VIVOS de la key (retirar un scope invalida JWT viejos).
    match action {
        ChannelAction::Manage => return Err(AuthzError::Forbidden),
        ChannelAction::Publish => {
            if !claims.can_publish() || !has_scope(&live_key, "publish") {
                return Err(AuthzError::Forbidden);
            }
        }
        ChannelAction::Subscribe => {
            if !claims.can_subscribe() || !has_scope(&live_key, "subscribe") {
                return Err(AuthzError::Forbidden);
            }
        }
        ChannelAction::View => {}
    }

    if !claim_channel_covers(claims, &ch.name) {
        return Err(AuthzError::Forbidden);
    }
    Ok(ch.id)
}

/// Cobertura del canal por el claim: exacto, "*" (la vida de la key ya se comprobó)
/// o la forma "prefijo:*".
fn claim_channel_covers(claims: &Claims, channel_name: &str) -> bool {
    if claims.channel == "*" {
        return true;
    }
    if claims.channel == channel_name {
        return true;
    }
    if let Some(prefix) = claims.channel.strip_suffix(":*") {
        return !prefix.is_empty() && channel_name.split(':').next() == Some(prefix);
    }
    false
}
