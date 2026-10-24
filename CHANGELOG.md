# CHANGELOG

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/).

## [Unreleased] — hardening (rama `security-hardening`)

### FASE 2 — Aislamiento de proyectos

**Aislamiento total entre proyectos** ahora es propiedad real, no un accidente de producción.

#### Cambios incompatibles

1. **Eliminado el puente service-key.** Una API key (`sc_live_...`) ya **no** es válida
   como token Bearer ni como token de WebSocket (`?token=`).
   - Antes: un backend podía abrir el WS con su API key cruda y tenía poder absoluto
     (`jti="svc"`, rol `both`, todos los canales).
   - Ahora: devuelve **401**. Los backends usan REST + `X-Api-Key`; los frontends
     usan JWT de corta vida emitido por `/auth/token`.
   - Migración: si algún componente abría WS con la API key, cámbialo a:
     1) `POST /auth/token` con `X-Api-Key` para pedir un JWT, 2) WS con ese JWT.

2. **Canales con dueño (`project_id`).** La tabla `channels` tiene columna `project_id`
   (migración idempotente automática al arrancar; se reparte dueño por prefijo del nombre).
   - Listados (`/channels`, `/stats`), lectura (`/channels/:id`, `/clients`) y borrados
     responden **404** para canales de otro proyecto (antes exponían todo el mundo).
   - **Riesgo de migración:** canales pre-existentes cuyo nombre no calza con el prefijo
     de ninguna key quedan huérfanos (`project_id=''`, inaccesibles). Si te pasa:
     ajusta `channel_prefix` de una key o renombra el canal manualmente en SQLite.

3. **`emit_event` ya no hace fallback cruzado.** Si la petición trae `X-Api-Key`, esa
   rama decide exclusivamente (y su error es definitivo). Sin X-Api-Key, solo JWT.
   - Bajo rate limit, la respuesta correcta ahora es **429** (antes la rama JWT
     fallback lo enmascaraba como 401).

4. **Revocación de key en vivo:** los JWT emitidos por una key revocada o eliminada
   dejan de servir de inmediato (401), aunque la firma y la expiración sigan bien.

5. **Identidad básica por `sub`:** el WS ya no usa un `client_id` elegido por el
   cliente para presencia/targets; usa el `sub` del JWT (fase de identidad continúa
   en la siguiente fase, con `connection_id` por conexión).
   - Migración: apuntar targets al identificador de usuario que el backend pone en
     `user_id` al pedir el JWT.

6. **Política de prefijos estricta.** Con prefijo `a:*`:
   - `a:x` válido; `a2:x`, `a`, `A:x`, `a:` e `x:a` **rechazados** (antes `a` y `a:`
     pasaban por un parseo laxo del prefijo).

7. **Identidad del WS:** el `client_id` declarado en el handshake se ignora (la
   identidad es el `sub` del JWT). Frontends que elegían `client_id` propio ya no
   controlan su identidad de presencia.


### FASE 3 — Identidad y mensajes

#### Cambios incompatibles

1. **La identidad la fija el backend, no el cliente.** El `client_id` del handshake WS
   se ignora por completo. La identidad de usuario es el `sub` del JWT (lo pone el
   backend del proyecto al pedir el token) y cada conexión recibe un `connection_id`
   propio generado por el servidor.
   - Antes: el cliente elegía `client_id` y con él se definían targets y presencia.
   - Ahora: targets y presencia usan `sub`; un usuario puede tener N conexiones
     simultáneas sin pisarse. Cerrar una no afecta a las demás.
   - Migración: en el backend, fija `user_id` (→ `sub`) al pedir `/auth/token`; en el
     frontend, envía solo `{"channel": "...", "device": "opcional"}` en el handshake.

2. **`source` de publicaciones WS lo impone el servidor**: `"<proyecto>:<sub>"`.
   Ya no se puede falsificar el origen de un evento desde el cliente.

3. **Eventos de sistema reservados.** `source="__system__"` (REST → 400) y los tipos
   `client_joined`, `client_left`, `missed_events` (REST → 400; WS → error
   `invalid_payload`) only los emite el servidor.

4. **Límites de payload**: 64 KB por payload/frame WS, profundidad JSON ≤ 16,
   targets ≤ 50 y ≤ 128 caracteres cada uno. Los WS que superen 64 KB se cierran con
   `{"type":"error","code":"message_too_large"}`.

5. **Envelope de error WS estable**: `{"type":"error","code":"...","detail":"..."}` con
   códigos `forbidden`, `invalid_payload`, `message_too_large`, `token_expired`.
   Los clientes que miraban el campo `error` deben leer `type`/`code`.

6. **Forma de presencia/sistema**: los eventos `client_joined`/`client_left` llevan
   `user`, `connection_id`, `clients` (array de objetos) y `total_clients`; el REST
   `/channels/:id/clients` devuelve `id` (=sub), `connection_id`, `device`,
   `client_type`, `project`, `connected_at`. La presencia solo es visible para el
   proyecto dueño del canal (404 para el resto).


### FASE 4 — Ciclo de vida, revocación y límites

#### Cambios incompatibles

1. **JWT más estricto**: algoritmo fijado a **HS256**, `iss`/`aud` obligatorios y
   validados (`socket-core`), leeway de 5 s. Los tokens emitidos con versiones
   anteriores (sin `iss`/`aud`) son rechazados: deja expirar el TTL corto o pide
   tokens nuevos tras el despliegue.

2. **Revocación activa e inmediata**: revocar/borrar una key, editar su prefijo o
   scopes, revocar un jti o borrar un canal **cierra los WebSockets afectados al
   instante** (bus de control interno). Además hay revalidación cada 2 s como red
   de seguridad (cubre cambios hechos directamente en la DB).

3. **Fail-closed**: si la DB no responde al comprobar la revocación de un jti o la
   vigencia de la key, la petición se deniega con **500** (nunca se asume válida).
   Los errores internos no se exponen al cliente.

4. **Gate de upgrade WS**: el proyecto debe existir, estar activo y no expirado.
   Se valida el JWT contra el **prefijo vivo** de la key y se aplican límites:
   50 conexiones por proyecto y 20 por IP (429 al superar).

5. **Protecciones WS nuevas**: timeout de handshake 8 s, frame máximo 64 KB,
   ping cada 30 s con idle timeout 120 s, rate limit de publicación 50/s por
   conexión. Códigos de cierre: `handshake_timeout`, `token_expired`,
   `credential_revoked`, `token_revoked`, `channel_deleted`, `server_shutdown`,
   `rate_limited`, `message_too_large`, `idle_timeout`, `invalid_payload`,
   `not_found`, `forbidden`. Un cliente lento recibe `{"type":"missed_events",
   "missed":N}` en vez de un cierre silencioso.

6. **Apagado ordenado**: SIGTERM/SIGINT cierran limpiamente los WS
   (`server_shutdown`). Tarea periódica purga `revoked_tokens` expirados y
   entradas viejas del rate limiter/registro.

### Nota de estado

- La suite de integración en `tests/` define el comportamiento objetivo: los
  tests aún en rojo corresponden a la fase 5 (red, secretos y configuración).

#### Mantener en mente (no incompatible)

- `Cargo.toml` ya tiene el nombre canónico (antes `cargo.toml` minúsculas al repo).
- Dependencias depuradas: fuera `tokio-tungstenite` y `url` de producción;
  `dotenv` → `dotenvy`; sqlx sin features por defecto (sqlite/runtime-tokio/macros).
- Suite de integración en `tests/` ahora referencia el comportamiento objetivo.
