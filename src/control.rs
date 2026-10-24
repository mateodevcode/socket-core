// Canal de control interno: permite cortar conexiones WS de inmediato cuando
// se revoca/edita/borra una key, se revoca un jti, se borra un canal o el
// servidor se apaga. Es un broadcast interno (no expuesto por red).
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// Cierra todas las conexiones del proyecto (key revocada/borrada/editada).
    CloseProject(String),
    /// Cierra las conexiones cuyo JWT tiene este jti.
    CloseJti(String),
    /// Cierra las conexiones suscritas a este canal (canal borrado).
    CloseChannel(String),
    /// Apagado ordenado: cierra todos los WS.
    Shutdown,
}

#[derive(Clone)]
pub struct ControlBus {
    tx: broadcast::Sender<ControlEvent>,
}

impl ControlBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }

    pub fn send(&self, event: ControlEvent) {
        // Si no hay receptores no pasa nada: el evento es best-effort + watchdog.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ControlEvent> {
        self.tx.subscribe()
    }
}

impl Default for ControlBus {
    fn default() -> Self {
        Self::new()
    }
}
