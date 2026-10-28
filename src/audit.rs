// Registro de auditoría sin secretos. Cada entrada lleva proyecto, acción y motivo.
// Se escribe en el log (target "audit") y se guarda en un ring buffer para tests.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_ENTRIES: usize = 1024;

#[derive(Clone)]
pub struct AuditLog {
    entries: Arc<Mutex<VecDeque<String>>>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_ENTRIES))),
        }
    }

    /// Registra un evento. NUNCA pasar secretos (keys/JWT) como outcome.
    pub fn record(&self, project_id: Option<&str>, action: &str, outcome: &str) {
        let line = format!(
            "project={} action={} outcome={}",
            project_id.unwrap_or("-"),
            action,
            outcome
        );
        tracing::info!(target: "audit", "{}", line);
        if let Ok(mut buf) = self.entries.lock() {
            if buf.len() >= MAX_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(line);
        }
    }

    /// Copia de las entradas (para tests/observabilidad).
    pub fn snapshot(&self) -> Vec<String> {
        self.entries
            .lock()
            .map(|b| b.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}
