// Autorización central de canales: UNA sola función decide por (proyecto, canal, acción).
// Ningún endpoint ni el WS pueden saltarla ni mantener atajos paralelos.
use crate::auth::Claims;
use crate::channel_manager::ChannelManager;
use crate::keys::{channel_allowed, ApiKey};
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
    /// La key/credencial que respalda al proyecto ya no está viva (revocada/borrada).
    InvalidCredential,
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
    // La key que respaldó al JWT debe seguir viva: revocada o borrada => no vale.
    let row = sqlx::query_as::<_, (i64,)>("SELECT revoked FROM api_keys WHERE id = ?")
        .bind(&claims.project_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AuthzError::InvalidCredential)?;
    let alive = matches!(row, Some((0,)));
    if !alive {
        return Err(AuthzError::InvalidCredential);
    }

    let Some(ch) = cm.resolve_channel(channel_id_or_name) else {
        return Err(AuthzError::NotFound);
    };
    if ch.project_id != claims.project_id {
        return Err(AuthzError::NotFound);
    }

    match action {
        ChannelAction::Manage => return Err(AuthzError::Forbidden),
        ChannelAction::Publish if !claims.can_publish() => return Err(AuthzError::Forbidden),
        ChannelAction::Subscribe if !claims.can_subscribe() => return Err(AuthzError::Forbidden),
        _ => {}
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
