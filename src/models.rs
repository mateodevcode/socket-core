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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub clients_count: usize,
}

impl Channel {
    pub fn new(name: String, description: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            created_at: chrono::Utc::now().timestamp(),
            clients_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: String,
    pub client_type: ClientType,
    /// Proyecto dueño de la conexión (del JWT / service key). Para ver por proyecto.
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
