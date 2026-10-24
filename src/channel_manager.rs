use crate::models::{Channel, ClientInfo, ClientType, Event};
use dashmap::DashMap;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info};

pub type ChannelBroadcast = broadcast::Sender<Event>;

#[derive(Clone)]
pub struct ChannelManager {
    channels: Arc<DashMap<String, ChannelData>>,
}

struct ChannelData {
    channel: Channel,
    tx: broadcast::Sender<Event>,
    clients: DashMap<String, ClientInfo>,
}

impl ChannelManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
        }
    }

    pub fn create_channel(
        &self,
        project_id: String,
        name: String,
        description: Option<String>,
    ) -> Result<Channel, String> {
        if self
            .channels
            .iter()
            .any(|entry| entry.value().channel.name == name)
        {
            return Err(format!("Channel '{}' already exists", name));
        }

        let channel = Channel::new(name, project_id, description);
        let (tx, _) = broadcast::channel(1000);

        self.channels.insert(
            channel.id.clone(),
            ChannelData {
                channel: channel.clone(),
                tx,
                clients: DashMap::new(),
            },
        );

        info!(
            "Channel created: {} (project={})",
            channel.id, channel.project_id
        );
        Ok(channel)
    }

    /// Resuelve un canal por id O por nombre (los clientes usan el nombre, los
    /// backends el id). Es la única puerta de entrada para referenciar canales.
    pub fn resolve_channel(&self, channel_id_or_name: &str) -> Option<Channel> {
        if let Some(v) = self.channels.get(channel_id_or_name) {
            return Some(v.value().channel.clone());
        }
        self.channels
            .iter()
            .find(|e| e.value().channel.name == channel_id_or_name)
            .map(|e| e.value().channel.clone())
    }

    pub fn get_channel(&self, channel_id: &str) -> Option<Channel> {
        self.channels
            .get(channel_id)
            .map(|entry| entry.value().channel.clone())
    }

    /// Canales VISIBLES para un proyecto (dueño). Los ajenos ni aparecen.
    pub fn list_channels_for(&self, project_id: &str) -> Vec<Channel> {
        self.channels
            .iter()
            .map(|entry| entry.value().channel.clone())
            .filter(|c| c.project_id == project_id)
            .collect()
    }

    pub fn list_channels(&self) -> Vec<Channel> {
        self.channels
            .iter()
            .map(|entry| entry.value().channel.clone())
            .collect()
    }

    /// Suscribe una CONEXIÓN (identificada por connection_id generado por el servidor).
    /// `user_id` es la identidad del usuario (sub del JWT); puede repetirse.
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &self,
        channel_id: &str,
        user_id: String,
        connection_id: String,
        device: Option<String>,
        client_type: ClientType,
        project: String,
    ) -> Result<ChannelBroadcast, String> {
        let mut channel_data = self
            .channels
            .get_mut(channel_id)
            .ok_or("Channel not found")?;

        let client_info = ClientInfo {
            id: user_id.clone(),
            connection_id: connection_id.clone(),
            device,
            client_type,
            project,
            connected_at: chrono::Utc::now().timestamp(),
        };

        channel_data
            .clients
            .insert(connection_id.clone(), client_info);
        channel_data.channel.clients_count = channel_data.clients.len();

        info!(
            "Client {} (conn {}) subscribed to channel {}",
            user_id, connection_id, channel_id
        );
        Ok(channel_data.tx.clone())
    }

    /// Elimina UNA conexión (connection_id) y notifica la salida.
    pub fn unsubscribe(&self, channel_id: &str, connection_id: &str) {
        let mut left: Option<(String, Vec<ClientInfo>, usize)> = None;
        if let Some(mut channel_data) = self.channels.get_mut(channel_id) {
            let user = channel_data
                .clients
                .remove(connection_id)
                .map(|(_, info)| info.id)
                .unwrap_or_default();
            channel_data.channel.clients_count = channel_data.clients.len();

            info!(
                "Client {} (conn {}) unsubscribed from channel {}",
                user, connection_id, channel_id
            );

            let clients: Vec<ClientInfo> = channel_data
                .clients
                .iter()
                .map(|entry| entry.value().clone())
                .collect();
            left = Some((user, clients, channel_data.clients.len()));
        }
        if let Some((user, clients, total)) = left {
            if let Some(channel_data) = self.channels.get(channel_id) {
                let leave_event = Event::new(
                    channel_id.to_string(),
                    crate::models::SYSTEM_SOURCE.to_string(),
                    vec!["*".to_string()],
                    json!({
                        "type": "client_left",
                        "user": user,
                        "connection_id": connection_id,
                        "clients": clients,
                        "total_clients": total
                    }),
                );
                let _ = channel_data.tx.send(leave_event);
            }
        }
    }

    pub fn emit_event(&self, channel_id: &str, event: Event) -> Result<(), String> {
        let channel_data = self.channels.get(channel_id).ok_or("Channel not found")?;
        debug!("Emitting event {} to channel {}", event.id, channel_id);
        let _ = channel_data.tx.send(event);
        Ok(())
    }

    pub fn get_clients(&self, channel_id: &str) -> Option<Vec<ClientInfo>> {
        self.channels.get(channel_id).map(|entry| {
            entry
                .clients
                .iter()
                .map(|client| client.value().clone())
                .collect()
        })
    }

    pub fn stats(&self) -> (usize, usize) {
        let channels = self.channels.len();
        let total_clients: usize = self
            .channels
            .iter()
            .map(|entry| entry.value().clients.len())
            .sum();
        (channels, total_clients)
    }

    /// Stats VISIBLES para un proyecto: cuenta solo sus canales y sus clientes.
    pub fn stats_for(&self, project_id: &str) -> (usize, usize) {
        let mut channels = 0usize;
        let mut total_clients = 0usize;
        for entry in self.channels.iter() {
            if entry.value().channel.project_id == project_id {
                channels += 1;
                total_clients += entry.value().clients.len();
            }
        }
        (channels, total_clients)
    }

    /// Elimina el canal de memoria. Los suscritos activos se desconectan solos
    /// (el broadcast se cierra al caer el último Sender).
    pub fn remove_channel(&self, channel_id: &str) -> bool {
        self.channels.remove(channel_id).is_some()
    }

    /// Guarda/actualiza el registro del canal en SQLite (los clientes no se persisten).
    pub async fn persist(&self, pool: &sqlx::SqlitePool, ch: &Channel) {
        let _ = sqlx::query(
            "INSERT INTO channels (id, name, project_id, description, created_at) VALUES (?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, project_id=excluded.project_id, description=excluded.description",
        )
        .bind(&ch.id)
        .bind(&ch.name)
        .bind(&ch.project_id)
        .bind(&ch.description)
        .bind(ch.created_at)
        .execute(pool)
        .await;
    }

    /// Recarga en memoria los canales persistidos (con broadcast y clientes frescos).
    /// Los IDs se conservan, así que los JWT atados a un canal siguen valiendo tras reinicios.
    pub async fn load_persisted(&self, pool: &sqlx::SqlitePool) -> usize {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>, i64)>(
            "SELECT id, name, project_id, description, created_at FROM channels",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        let mut n = 0;
        for (id, name, project_id, description, created_at) in rows {
            if self.channels.contains_key(&id) {
                continue;
            }
            let (tx, _) = broadcast::channel(1000);
            self.channels.insert(
                id.clone(),
                ChannelData {
                    channel: Channel {
                        id,
                        name,
                        project_id,
                        description,
                        created_at,
                        clients_count: 0,
                    },
                    tx,
                    clients: DashMap::new(),
                },
            );
            n += 1;
        }
        if n > 0 {
            info!("Canales restaurados desde SQLite: {}", n);
        }
        n
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}
