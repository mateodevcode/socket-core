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

        let channel = Channel::new(name, description);
        let (tx, _) = broadcast::channel(1000);

        self.channels.insert(
            channel.id.clone(),
            ChannelData {
                channel: channel.clone(),
                tx,
                clients: DashMap::new(),
            },
        );

        info!("Channel created: {}", channel.id);
        Ok(channel)
    }

    pub fn get_channel(&self, channel_id: &str) -> Option<Channel> {
        self.channels
            .get(channel_id)
            .map(|entry| entry.value().channel.clone())
    }

    pub fn list_channels(&self) -> Vec<Channel> {
        self.channels
            .iter()
            .map(|entry| entry.value().channel.clone())
            .collect()
    }

    pub fn subscribe(
        &self,
        channel_id: &str,
        client_id: String,
        client_type: ClientType,
        project: String,
    ) -> Result<ChannelBroadcast, String> {
        let mut channel_data = self
            .channels
            .get_mut(channel_id)
            .ok_or("Channel not found")?;

        let client_info = ClientInfo {
            id: client_id.clone(),
            client_type,
            project,
            connected_at: chrono::Utc::now().timestamp(),
        };

        channel_data.clients.insert(client_id.clone(), client_info);
        channel_data.channel.clients_count = channel_data.clients.len();

        info!(
            "Client {} subscribed to channel {}",
            client_id, channel_id
        );
        Ok(channel_data.tx.clone())
    }

    pub fn unsubscribe(&self, channel_id: &str, client_id: &str) {
        if let Some(mut channel_data) = self.channels.get_mut(channel_id) {
            channel_data.clients.remove(client_id);
            channel_data.channel.clients_count = channel_data.clients.len();

            info!(
                "Client {} unsubscribed from channel {}",
                client_id, channel_id
            );

            let clients_list: Vec<String> = channel_data
                .clients
                .iter()
                .map(|entry| entry.key().clone())
                .collect();

            let leave_event = Event::new(
                channel_id.to_string(),
                "__system__".to_string(),
                vec!["*".to_string()],
                json!({
                    "type": "client_left",
                    "client_id": client_id,
                    "clients": clients_list,
                    "total_clients": channel_data.clients.len()
                }),
            );
            let _ = channel_data.tx.send(leave_event);
        }
    }

    pub fn emit_event(&self, channel_id: &str, event: Event) -> Result<(), String> {
        let channel_data = self
            .channels
            .get(channel_id)
            .ok_or("Channel not found")?;
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

    /// Guarda/actualiza el registro del canal en SQLite (los clientes no se persisten).
    pub async fn persist(&self, pool: &sqlx::SqlitePool, ch: &Channel) {
        let _ = sqlx::query(
            "INSERT INTO channels (id, name, description, created_at) VALUES (?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description",
        )
        .bind(&ch.id)
        .bind(&ch.name)
        .bind(&ch.description)
        .bind(ch.created_at)
        .execute(pool)
        .await;
    }

    /// Recarga en memoria los canales persistidos (con broadcast y clientes frescos).
    /// Los IDs se conservan, así que los JWT atados a un canal siguen valiendo tras reinicios.
    pub async fn load_persisted(&self, pool: &sqlx::SqlitePool) -> usize {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            "SELECT id, name, description, created_at FROM channels",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        let mut n = 0;
        for (id, name, description, created_at) in rows {
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