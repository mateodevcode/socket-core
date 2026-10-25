// Helpers de integración: hub temporal con DB SQLite temporal, dos proyectos (a:*, b:*),
// server en puerto aleatorio y clientes HTTP/WS.
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use socket_core::auth::{create_jwt, Claims};
use socket_core::channel_manager::ChannelManager;
use socket_core::handlers::AppState;
use socket_core::keys::KeyStore;
use socket_core::middleware::RateLimiter;
use socket_core::{config, db};
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const JWT_SECRET: &str = "test-jwt-secret-32-chars-minimum!!!!";
pub const MASTER_KEY: &str = "test-master-key-32-chars-minimum!!!!!!";
pub const HANDSHAKE_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub enum Project {
    A,
    B,
}

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Hub {
    pub base: String,
    pub admin_base: String,
    pub client: Client,
    pub state: AppState,
    pub key_a: String,
    pub key_b: String,
    pub project_a: String,
    pub project_b: String,
    _db: TempDir,
}

impl Hub {
    /// Levanta un hub completo con proyectos A (prefijo "a:*") y B (prefijo "b:*").
    pub async fn start() -> Self {
        Self::start_with_limits(socket_core::limits::Limits::default()).await
    }

    pub async fn start_with_limits(limits: socket_core::limits::Limits) -> Self {
        Self::start_with_options(limits, Vec::new()).await
    }

    /// Igual que `start` pero con límites y proxies de confianza personalizados.
    pub async fn start_with_options(
        limits: socket_core::limits::Limits,
        trusted_proxies: Vec<ipnet::IpNet>,
    ) -> Self {
        let db_dir = TempDir::new().expect("temp dir");
        let db_path = db_dir.path().join("test.db");
        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let pool = db::init_pool(&db_url).await.expect("temp db pool");
        let control = socket_core::control::ControlBus::new();
        let keys = KeyStore::new_with_control(pool, control.clone());

        let (ka, raw_a) = keys
            .create(
                "proj-a".into(),
                vec!["https://app-a.example".into()],
                "a:*".into(),
                vec!["publish".into(), "subscribe".into(), "manage".into()],
                0, // rate limit desactivado salvo tests específicos
                None,
            )
            .await
            .expect("key A");
        let (kb, raw_b) = keys
            .create(
                "proj-b".into(),
                vec!["https://app-b.example".into()],
                "b:*".into(),
                vec!["publish".into(), "subscribe".into(), "manage".into()],
                0,
                None,
            )
            .await
            .expect("key B");

        let state = AppState {
            channel_manager: ChannelManager::new(),
            keys,
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_secs: 600,
            master_key: MASTER_KEY.into(),
            limiter: RateLimiter::new(),
            control,
            registry: socket_core::limits::ConnectionRegistry::new(),
            limits,
            publish_limits: socket_core::limits::ProjectPublishLimiters::new(),
            dev_mode: true,
            trusted_proxies,
            audit: socket_core::audit::AuditLog::new(),
            admin_guard: socket_core::middleware::AdminGuard::new(),
            allowed_origins: vec![],
        };

        let addr = socket_core::start_test_server(state.clone()).await;
        let admin_addr = socket_core::start_test_admin_server(state.clone()).await;
        let base = format!("http://{}", addr);
        let admin_base = format!("http://{}", admin_addr);

        Self {
            base,
            admin_base,
            client: Client::builder().build().expect("client"),
            state,
            key_a: raw_a,
            key_b: raw_b,
            project_a: ka.id,
            project_b: kb.id,
            _db: db_dir,
        }
    }

    /// Crea canales "a:chat" y "b:chat" vía REST. Devuelve (id_a, id_b).
    pub async fn seed_channels(&self) -> (String, String) {
        let a = self.create_channel_ok(Project::A, "a:chat").await;
        let b = self.create_channel_ok(Project::B, "b:chat").await;
        (a, b)
    }

    pub fn key_for(&self, p: Project) -> &str {
        match p {
            Project::A => &self.key_a,
            Project::B => &self.key_b,
        }
    }

    /// Mint de JWT vía REST /auth/token (la ruta real de producción).
    pub async fn mint_jwt(&self, p: Project, sub: &str, channel: &str, role: &str) -> String {
        let res = self
            .client
            .post(format!("{}/auth/token", self.base))
            .header("x-api-key", self.key_for(p))
            .json(&json!({"user_id": sub, "channel": channel, "role": role}))
            .send()
            .await
            .expect("auth/token");
        assert_eq!(res.status(), 200, "mint_jwt {} falló", role);
        let v: Value = res.json().await.expect("json auth/token");
        v["data"]["token"].as_str().expect("token").to_string()
    }

    /// JWT forjado directamente (para tokens expirados u otros casos imposibles vía REST).
    pub fn custom_jwt(&self, sub: &str, channel: &str, role: &str, exp: i64) -> String {
        create_jwt(
            JWT_SECRET,
            exp - chrono::Utc::now().timestamp(),
            &self.project_a,
            "proj-a",
            sub,
            channel,
            role,
        )
        .map(|(t, _)| t)
        .expect("custom jwt")
    }

    /// Claims firmados (con jti real devuelto junto al token).
    pub fn signed_claims(&self, sub: &str, channel: &str, role: &str) -> (String, String) {
        create_jwt(
            JWT_SECRET,
            600,
            &self.project_a,
            "proj-a",
            sub,
            channel,
            role,
        )
        .expect("jwt")
    }

    /// Crea canal con la key del proyecto vía REST.
    pub async fn create_channel_with_name(&self, p: Project, name: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/channels", self.base))
            .header("x-api-key", self.key_for(p))
            .json(&json!({"name": name, "description": "test"}))
            .send()
            .await
            .expect("POST /channels")
    }

    pub async fn create_channel_ok(&self, p: Project, name: &str) -> String {
        let res = self.create_channel_with_name(p, name).await;
        assert_eq!(res.status(), 201, "crear canal {} debió funcionar", name);
        let v: Value = res.json().await.expect("json");
        v["data"]["id"].as_str().expect("id").to_string()
    }

    /// Emite evento por REST con API key.
    pub async fn emit_with_key(
        &self,
        key: &str,
        channel: &str,
        targets: Vec<String>,
        payload: Value,
    ) -> reqwest::Response {
        self.client
            .post(format!("{}/channels/{}/events", self.base.clone(), channel))
            .header("x-api-key", key)
            .json(&json!({"source": "backend", "targets": targets, "payload": payload}))
            .send()
            .await
            .expect("POST events")
    }

    /// Emite evento por REST con Authorization Bearer (JWT u otra cosa).
    pub async fn emit_with_bearer(
        &self,
        bearer: &str,
        channel: &str,
        targets: Vec<String>,
        payload: Value,
    ) -> reqwest::Response {
        self.client
            .post(format!("{}/channels/{}/events", self.base.clone(), channel))
            .header("Authorization", format!("Bearer {}", bearer))
            .json(&json!({"source": "user", "targets": targets, "payload": payload}))
            .send()
            .await
            .expect("POST events bearer")
    }

    /// GET /channels/:id/clients con una key.
    pub async fn get_clients(&self, key: &str, channel: &str) -> reqwest::Response {
        self.client
            .get(format!(
                "{}/channels/{}/clients",
                self.base.clone(),
                channel
            ))
            .header("x-api-key", key)
            .send()
            .await
            .expect("GET clients")
    }

    /// Conecta WS. Ok(stream) si el upgrade funcionó; Err(status) si fue rechazado.
    pub async fn ws_connect_raw(&self, token: &str) -> Result<WsStream, u16> {
        let ws_base = self.base.replacen("http://", "ws://", 1);
        let url = format!("{}/ws?token={}", ws_base, encode_query(token));
        match connect_async(url).await {
            Ok((stream, _)) => Ok(stream),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
            Err(e) => panic!("error WS inesperado no-HTTP: {}", e),
        }
    }

    /// Conexión completa: upgrade + handshake de canal.
    /// Ok(stream) si entró; Err(status) si el upgrade fue rechazado con código HTTP;
    /// Err(1) si el servidor aceptó el upgrade pero cerró/errores en el handshake;
    /// Err(0) si no llegó respuesta en HANDSHAKE_CLOSE_TIMEOUT.
    pub async fn ws_open(&self, token: &str, channel: &str) -> Result<WsStream, u16> {
        self.ws_open_handshake(token, channel, None).await
    }

    /// Crea una key vía /admin/keys (master). Devuelve la respuesta cruda.
    pub async fn admin_create_key(
        &self,
        name: &str,
        prefix: &str,
        scopes: &[&str],
    ) -> reqwest::Response {
        self.client
            .post(format!("{}/admin/keys", self.admin_base))
            .header("x-master-key", MASTER_KEY)
            .json(&json!({"name": name, "channel_prefix": prefix, "scopes": scopes}))
            .send()
            .await
            .expect("POST /admin/keys")
    }

    /// PATCH de key vía /admin/keys/:id (master).
    pub async fn admin_patch_key(&self, id: &str, body: Value) -> reqwest::Response {
        self.client
            .patch(format!("{}/admin/keys/{}", self.admin_base, id))
            .header("x-master-key", MASTER_KEY)
            .json(&body)
            .send()
            .await
            .expect("PATCH /admin/keys/:id")
    }

    /// POST /auth/token usando una API key cruda arbitraria.
    pub async fn mint_with_key(&self, raw_key: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/auth/token", self.base))
            .header("x-api-key", raw_key)
            .json(&body)
            .send()
            .await
            .expect("POST /auth/token (key cruda)")
    }

    /// POST /auth/token con cuerpo arbitrario.
    pub async fn mint_request(&self, p: Project, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/auth/token", self.base))
            .header("x-api-key", self.key_for(p))
            .json(&body)
            .send()
            .await
            .expect("POST /auth/token")
    }

    /// Simula un fallo de la tabla de canales (la DB de keys sigue funcionando).
    pub async fn drop_channels_table(&self) {
        sqlx::query("DROP TABLE channels")
            .execute(&self.state.keys.pool)
            .await
            .expect("DROP TABLE channels");
    }

    /// Igual que ws_open pero permite campos extra en el handshake (p.ej. client_id malicioso).
    pub async fn ws_open_handshake(
        &self,
        token: &str,
        channel: &str,
        mut extra_fields: Option<Value>,
    ) -> Result<WsStream, u16> {
        let ws_base = self.base.replacen("http://", "ws://", 1);
        let url = format!("{}/ws?token={}", ws_base, encode_query(token));
        let (mut stream, _) = match connect_async(url).await {
            Ok(r) => r,
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                return Err(resp.status().as_u16())
            }
            Err(e) => panic!("error WS inesperado no-HTTP: {}", e),
        };
        let mut hs = json!({"channel": channel});
        if let Some(e) = extra_fields.take() {
            for (k, v) in e.as_object().expect("obj") {
                hs[k] = v.clone();
            }
        }
        if stream.send(Message::Text(hs.to_string())).await.is_err() {
            return Err(1);
        }
        match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).map_err(|_| 1u16)?;
                let es_error = v.get("error").is_some()
                    || v.get("type").and_then(|x| x.as_str()) == Some("error");
                if es_error {
                    Err(1)
                } else {
                    Ok(stream)
                }
            }
            Ok(Some(Ok(_))) => Ok(stream),
            Ok(Some(Err(_))) | Ok(None) => Err(1),
            Err(_) => Err(0),
        }
    }
}

/// Lee mensajes text del stream hasta conseguir JSON válido; None si se cerró (o timeout).
pub async fn next_text(stream: &mut WsStream, timeout: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let msg = tokio::time::timeout_at(deadline, stream.next()).await;
        match msg {
            Err(_) => return None, // timeout
            Ok(None) => return None,
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    return Some(v);
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => return None,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) => return None,
        }
    }
    None
}

/// Espera (hasta timeout) que la conexión cierre. true si cerró.
pub async fn waits_closed(stream: &mut WsStream, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let msg = tokio::time::timeout_at(deadline, stream.next()).await;
        match msg {
            Err(_) => return false,
            Ok(None) | Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// JWT firmado con expiración pasada (forjado directo).
pub fn expired_jwt(hub: &Hub, sub: &str, channel: &str, role: &str) -> String {
    let claims = Claims {
        sub: sub.into(),
        project_id: hub.project_a.clone(),
        project_name: "proj-a".into(),
        channel: channel.into(),
        role: role.into(),
        jti: uuid::Uuid::new_v4().to_string(),
        iss: socket_core::auth::JWT_ISSUER.to_string(),
        aud: socket_core::auth::JWT_AUDIENCE.to_string(),
        iat: chrono::Utc::now().timestamp() - 7200,
        exp: chrono::Utc::now().timestamp() - 3600, // 1h expirado
    };
    let header = jsonwebtoken::Header::default();
    jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(hub.state.jwt_secret.as_bytes()),
    )
    .expect("encode expired")
}

/// JWT firmado y su jti real.
pub type SignedJwt = (String, String);

/// Codifica un valor para query string (JWT puede traer +/=/).
pub fn encode_query(v: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(v.len());
    for b in v.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(*b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

#[allow(unused)]
fn cfg_marker() {
    let _ = config::DEFAULT_JWT_SECRET;
}
