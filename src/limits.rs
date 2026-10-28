// Límites de recursos (constantes + estructuras auxiliares testeables).
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tamaño máximo de un frame/mensaje WS (y de un payload lógico).
pub const WS_MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Tiempo máximo para completar el handshake WS.
pub const WS_HANDSHAKE_TIMEOUT_SECS: u64 = 8;
/// Máximo de conexiones WS simultáneas por proyecto.
pub const WS_MAX_CONNECTIONS_PER_PROJECT: usize = 50;
/// Máximo de conexiones WS simultáneas por IP.
pub const WS_MAX_CONNECTIONS_PER_IP: usize = 20;
/// Máximo de publicaciones por conexión en una ventana de 1 segundo.
pub const WS_PUBLISH_PER_SEC: u32 = 50;
/// Máximo de publicaciones AGREGADAS por proyecto en una ventana de 1 segundo.
pub const WS_PUBLISH_PER_PROJECT_PER_SEC: u32 = 500;
/// Intervalo de ping al cliente.
pub const WS_PING_INTERVAL_SECS: u64 = 30;
/// Sin actividad entrante durante este tiempo => conexión muerta.
pub const WS_IDLE_TIMEOUT_SECS: u64 = 120;
/// Cada cuánto revalida la conexión sus credenciales (revocación externa).
pub const WS_AUTH_RECHECK_SECS: u64 = 2;
/// Limpieza periódica de tablas/estructuras efímeras.
pub const CLEANUP_INTERVAL_SECS: u64 = 300;
/// Entradas del rate limiter más viejas que esto se purgan.
pub const RATE_LIMITER_TTL_SECS: u64 = 120;
/// Contadores de fallos de admin sin bloqueo expiran pasado este TTL.
pub const ADMIN_FAIL_TTL_SECS: u64 = 900;

use dashmap::DashMap;

/// Límites configurables del hub (defaults seguros; tests pueden reducirlos).
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_message_bytes: usize,
    pub handshake_timeout_secs: u64,
    pub max_conns_project: usize,
    pub max_conns_ip: usize,
    pub publish_per_sec_conn: u32,
    pub publish_per_sec_project: u32,
    /// Cada cuánto revalida credenciales cada WS (watchdog).
    pub auth_recheck_secs: u64,
    pub ping_interval_secs: u64,
    pub idle_timeout_secs: u64,
    /// Peticiones por minuto por IP (se aplica ANTES de validar la key).
    pub ip_rate_per_min: i64,
    /// Fallos de MASTER_KEY antes de bloquear temporalmente.
    pub admin_fail_max: u32,
    /// Duración del bloqueo del listener admin.
    pub admin_lock_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: WS_MAX_MESSAGE_BYTES,
            handshake_timeout_secs: WS_HANDSHAKE_TIMEOUT_SECS,
            max_conns_project: WS_MAX_CONNECTIONS_PER_PROJECT,
            max_conns_ip: WS_MAX_CONNECTIONS_PER_IP,
            publish_per_sec_conn: WS_PUBLISH_PER_SEC,
            publish_per_sec_project: WS_PUBLISH_PER_PROJECT_PER_SEC,
            auth_recheck_secs: 10,
            ping_interval_secs: WS_PING_INTERVAL_SECS,
            idle_timeout_secs: WS_IDLE_TIMEOUT_SECS,
            ip_rate_per_min: 600,
            admin_fail_max: 5,
            admin_lock_secs: 60,
        }
    }
}

/// Contadores de conexiones activas por proyecto y por IP.
#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    by_project: Arc<DashMap<String, usize>>,
    by_ip: Arc<DashMap<String, usize>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserva un slot ANTES del upgrade. false si se excede algún límite.
    pub fn try_register(
        &self,
        project_id: &str,
        ip: &str,
        max_project: usize,
        max_ip: usize,
    ) -> bool {
        {
            let mut p = self.by_project.entry(project_id.to_string()).or_insert(0);
            if *p >= max_project {
                return false;
            }
            *p += 1;
        }
        {
            let mut i = self.by_ip.entry(ip.to_string()).or_insert(0);
            if *i >= max_ip {
                // revertir el contador de proyecto
                if let Some(mut p) = self.by_project.get_mut(project_id) {
                    *p = p.saturating_sub(1);
                }
                return false;
            }
            *i += 1;
        }
        true
    }

    pub fn unregister(&self, project_id: &str, ip: &str) {
        if let Some(mut p) = self.by_project.get_mut(project_id) {
            *p = p.saturating_sub(1);
        }
        if let Some(mut i) = self.by_ip.get_mut(ip) {
            *i = i.saturating_sub(1);
        }
    }

    pub fn project_count(&self, project_id: &str) -> usize {
        self.by_project.get(project_id).map(|v| *v).unwrap_or(0)
    }

    pub fn ip_count(&self, ip: &str) -> usize {
        self.by_ip.get(ip).map(|v| *v).unwrap_or(0)
    }

    /// Limpieza de claves con contador 0 (evita crecer sin límite).
    pub fn cleanup(&self) {
        self.by_project.retain(|_, v| *v > 0);
        self.by_ip.retain(|_, v| *v > 0);
    }
}

/// Ventana fija de 1 segundo para el rate limit de publicación.
pub struct PublishWindow {
    window_start: Instant,
    count: u32,
    limit: u32,
}

impl PublishWindow {
    pub fn new(limit: u32) -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
            limit,
        }
    }

    /// true si la publicación está permitida; false si se excedió el límite.
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.limit
    }

    pub fn is_stale(&self) -> bool {
        self.window_start.elapsed() > Duration::from_secs(RATE_LIMITER_TTL_SECS)
    }
}

impl Default for PublishWindow {
    fn default() -> Self {
        Self::new(WS_PUBLISH_PER_SEC)
    }
}

/// Ventanas de publicación AGREGADA por proyecto (compartidas entre conexiones).
#[derive(Clone, Default)]
pub struct ProjectPublishLimiters {
    inner: Arc<DashMap<String, Arc<std::sync::Mutex<PublishWindow>>>>,
}

impl ProjectPublishLimiters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&self, project_id: &str, limit: u32) -> bool {
        let window: Arc<std::sync::Mutex<PublishWindow>> = {
            let entry = self
                .inner
                .entry(project_id.to_string())
                .or_insert_with(|| Arc::new(std::sync::Mutex::new(PublishWindow::new(limit))));
            Arc::clone(&entry)
        };
        let allowed = match window.lock() {
            Ok(mut w) => w.allow(),
            Err(_) => false, // mutex envenenado => denegar (fail-closed)
        };
        allowed
    }

    pub fn cleanup(&self) {
        self.inner.retain(|_, w| match w.lock() {
            Ok(win) => !win.is_stale(),
            Err(_) => false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_limits_and_cleanup() {
        let reg = ConnectionRegistry::new();
        assert!(reg.try_register("p1", "ip1", 50, 20));
        assert_eq!(reg.project_count("p1"), 1);
        reg.unregister("p1", "ip1");
        assert_eq!(reg.project_count("p1"), 0);
        assert_eq!(reg.ip_count("ip1"), 0);
        reg.cleanup();
        assert_eq!(reg.project_count("p1"), 0);
    }

    #[test]
    fn publish_window_caps_per_second() {
        let mut w = PublishWindow::new(5);
        for _ in 0..5 {
            assert!(w.allow());
        }
        assert!(!w.allow(), "debe cortar al superar el límite por segundo");
    }

    #[test]
    fn project_publish_limiter_agrega_entre_llamadas() {
        let l = ProjectPublishLimiters::new();
        assert!(l.allow("p", 3));
        assert!(l.allow("p", 3));
        assert!(l.allow("p", 3));
        assert!(!l.allow("p", 3), "el límite agregado debe aplicar");
        assert!(l.allow("otro", 3), "otro proyecto tiene su propia ventana");
    }
}
