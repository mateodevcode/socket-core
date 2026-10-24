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

use dashmap::DashMap;

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
    pub fn try_register(&self, project_id: &str, ip: &str) -> bool {
        {
            let mut p = self.by_project.entry(project_id.to_string()).or_insert(0);
            if *p >= WS_MAX_CONNECTIONS_PER_PROJECT {
                return false;
            }
            *p += 1;
        }
        {
            let mut i = self.by_ip.entry(ip.to_string()).or_insert(0);
            if *i >= WS_MAX_CONNECTIONS_PER_IP {
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

/// Ventana fija de 1 segundo para el rate limit de publicación por conexión.
pub struct PublishWindow {
    window_start: Instant,
    count: u32,
}

impl PublishWindow {
    pub fn new() -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
        }
    }

    /// true si la publicación está permitida; false si debe cerrarse la conexión.
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= WS_PUBLISH_PER_SEC
    }
}

impl Default for PublishWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_limits_and_cleanup() {
        let reg = ConnectionRegistry::new();
        assert!(reg.try_register("p1", "ip1"));
        assert_eq!(reg.project_count("p1"), 1);
        reg.unregister("p1", "ip1");
        assert_eq!(reg.project_count("p1"), 0);
        assert_eq!(reg.ip_count("ip1"), 0);
        reg.cleanup();
        assert_eq!(reg.project_count("p1"), 0);
    }

    #[test]
    fn publish_window_caps_per_second() {
        let mut w = PublishWindow::new();
        for _ in 0..WS_PUBLISH_PER_SEC {
            assert!(w.allow());
        }
        assert!(!w.allow(), "debe cortar al superar el límite por segundo");
    }
}
