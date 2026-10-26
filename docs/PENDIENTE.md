# Pendientes

## R10 — Rotación de API keys (documentado, NO implementado)

Objetivo: rotar una key sin ventana de indisponibilidad y sin reutilizar el
prefijo de canal de otra key.

### Alcance

- `POST /admin/keys/:id/rotate` (solo listener admin, `X-Master-Key`):
  - Genera una key nueva y devuelve la cruda **una sola vez**.
  - Conserva `name`, `domains`, `channel_prefix`, `scopes`, `rate_limit_per_min`
    y `expires_at` de la key original.
  - En una sola transacción: inserta la nueva, revoca la anterior y deja el
    prefijo libre para la nueva (sin que el chequeo de solape se compare consigo
    mismo).
  - Si algo falla, no cambia ninguna de las dos.
  - La key antigua deja de autenticar de inmediato (REST y WS/JWT ya emitidos
    siguen su ciclo normal de revocación).
- `scripts/alta_cliente.py --rotar`: pide la rotación y escribe el paquete nuevo
  en `0600`; nunca imprime la key ni la pasa por argv.

### Tests exigidos antes de implementar

- Rotación correcta: 200 con key nueva; la vieja responde 401 en REST y en
  `/auth/token`; la nueva conserva scopes, dominios, prefijo y límites.
- La respuesta no registra la key en logs/auditoría y el fichero queda `0600`.
- Filtraciones: no se puede rotar una key inexistente (404), revocada o expirada
  sin definir el comportamiento esperado; no se libera el prefijo antes de tiempo.
- Fallo de DB a mitad: la key vieja sigue válida y no aparece una nueva.
- Dos rotaciones concurrentes: solo una gana; la otra no deja keys huérfanas.
- CLI: `--rotar` sin MASTER_KEY no intenta nada y no escribe ficheros.
