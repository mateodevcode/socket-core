use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub channel: String,
    pub source: String,
    pub targets: Vec<String>,
    pub payload: serde_json::Value,
    pub timestamp: i64,
}

impl Event {
    pub fn new(
        channel: String,
        source: String,
        targets: Vec<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            channel,
            source,
            targets,
            payload,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

pub const SYSTEM_SOURCE: &str = "__system__";
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_PAYLOAD_DEPTH: usize = 16;
pub const MAX_TARGETS: usize = 50;
pub const MAX_TARGET_LEN: usize = 128;
pub const SYSTEM_EVENT_TYPES: &[&str] = &["client_joined", "client_left", "missed_events"];

pub fn is_reserved_event_type(t: &str) -> bool {
    SYSTEM_EVENT_TYPES.contains(&t)
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventError {
    ReservedSource,
    ReservedType(String),
    TooManyTargets,
    TargetTooLong,
    PayloadTooLarge,
    PayloadTooDeep,
}

impl std::fmt::Display for EventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventError::ReservedSource => write!(f, "source reservado"),
            EventError::ReservedType(t) => write!(f, "tipo de evento reservado: {}", t),
            EventError::TooManyTargets => write!(f, "demasiados targets (max {})", MAX_TARGETS),
            EventError::TargetTooLong => {
                write!(f, "target demasiado largo (max {})", MAX_TARGET_LEN)
            }
            EventError::PayloadTooLarge => write!(
                f,
                "payload demasiado grande (max {} bytes)",
                MAX_PAYLOAD_BYTES
            ),
            EventError::PayloadTooDeep => write!(
                f,
                "profundidad de payload excedida (max {})",
                MAX_PAYLOAD_DEPTH
            ),
        }
    }
}

impl std::error::Error for EventError {}

/// Valida reglas de salida comunes a REST y WS.
pub fn validate_outbound(
    source: &str,
    targets: &[String],
    payload: &serde_json::Value,
) -> Result<(), EventError> {
    if source == SYSTEM_SOURCE {
        return Err(EventError::ReservedSource);
    }
    if let Some(t) = payload.get("type").and_then(|v| v.as_str()) {
        if is_reserved_event_type(t) {
            return Err(EventError::ReservedType(t.to_string()));
        }
    }
    if targets.len() > MAX_TARGETS {
        return Err(EventError::TooManyTargets);
    }
    for t in targets {
        if t.len() > MAX_TARGET_LEN {
            return Err(EventError::TargetTooLong);
        }
    }
    let bytes = serde_json::to_vec(payload).map_err(|_| EventError::PayloadTooLarge)?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(EventError::PayloadTooLarge);
    }
    if depth_of(payload) > MAX_PAYLOAD_DEPTH {
        return Err(EventError::PayloadTooDeep);
    }
    Ok(())
}

fn depth_of(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Object(map) => 1 + map.values().map(depth_of).max().unwrap_or(0),
        serde_json::Value::Array(arr) => 1 + arr.iter().map(depth_of).max().unwrap_or(0),
        _ => 1,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub clients_count: usize,
}

impl Channel {
    pub fn new(name: String, project_id: String, description: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            project_id,
            description,
            created_at: chrono::Utc::now().timestamp(),
            clients_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Identidad del usuario final: el `sub` del JWT (no la elige el cliente).
    pub id: String,
    /// Identificador de ESTA conexión, generado por el servidor.
    /// Un usuario puede tener varias conexiones simultáneas.
    pub connection_id: String,
    /// Nombre de dispositivo opcional saneado proveniente del handshake.
    #[serde(default)]
    pub device: Option<String>,
    pub client_type: ClientType,
    /// Proyecto dueño de la conexión (del JWT). Para ver por proyecto.
    #[serde(default)]
    pub project: String,
    pub connected_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum ClientType {
    WebSocket,
    Webhook(String),
    Internal,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmitEventRequest {
    pub source: String,
    pub targets: Vec<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ServerStats {
    pub channels: usize,
    pub total_clients: usize,
}

// ---- Auth / Admin ----

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    /// Identidad del usuario final (ej: user-123). Si se omite se genera uno.
    pub user_id: Option<String>,
    /// Canal al que se le da acceso, o "*" para todo el prefijo del proyecto.
    pub channel: Option<String>,
    /// "publish" | "subscribe" | "both"
    pub role: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub expires_in: i64,
    pub channel: String,
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub name: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default = "default_prefix")]
    pub channel_prefix: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    #[serde(default = "default_rate")]
    pub rate_limit_per_min: i64,
    /// Días de vigencia. Omitir = sin expiración.
    pub expires_in_days: Option<i64>,
}

fn default_prefix() -> String {
    "*".to_string()
}
fn default_scopes() -> Vec<String> {
    vec!["publish".into(), "subscribe".into()]
}
fn default_rate() -> i64 {
    120
}

#[derive(Debug, Serialize)]
pub struct CreateKeyResponse {
    pub id: String,
    pub name: String,
    /// ¡Mostrar una sola vez! No se vuelve a recuperar.
    pub api_key: String,
    pub domains: Vec<String>,
    pub channel_prefix: String,
    pub scopes: Vec<String>,
    pub rate_limit_per_min: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyPublic {
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

#[derive(Debug, Deserialize)]
pub struct RevokeTokenRequest {
    pub jti: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateKeyRequest {
    pub name: Option<String>,
    pub domains: Option<Vec<String>>,
    pub channel_prefix: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub rate_limit_per_min: Option<i64>,
    /// Días de vigencia desde hoy. null/omitido = no toca; 0 = sin expiración.
    pub expires_in_days: Option<i64>,
}
