# CHANGELOG

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/).

## [Unreleased] — hardening (rama `security-hardening`)

### Fase 8.10 — informe final, rate por IP configurable y rotación pendiente

- **W1**: `docs/INFORME-FINAL.md` con la tabla completa de hallazgos
  (4.1, 6 y 8.x), estado, evidencia y tests.
- **W2**: `IP_RATE_PER_MIN` (1–100000, defecto 600) configurable por env; se
  aplica al límite por IP de las peticiones con API key y al cupo de emisión
  (antes 600 fijo). El arranque rechaza valores fuera de rango. Documentado el
  efecto cuando los backends llaman directo por `central_network`/Docker.
- **W3**: desglose de los 42 tests de Python por fichero y su job de CI.
- **W4/R10**: alcance de `POST /admin/keys/:id/rotate` y
  `alta_cliente.py --rotar` en `docs/PENDIENTE.md`; **no implementado** en esta
  fase.

### Fase 8.9 — despliegue de rama, fetch seguro, smoke y estado sin dominio

- **V1**: §0.2 usa la imagen real de Compose (`docker compose config --images`,
  p. ej. `socket-core-socket-core`) y `scripts/preflight.py --db ... --db-only`
  en vez de `sqlite3`. Probado sobre una DB sintética con esquema antiguo.
- **V2/R11**: `deploy.sh` acepta `DEPLOY_REF` (rama o SHA) sin tocar `main`; si no
  existe `.last_good_commit` lo inicializa con el HEAD previo al fetch; el rollback
  reverifica solo el puerto público (las versiones viejas pueden no tener 3015) y
  se simula sin Docker en `scripts/test_deploy.py`.
- **V3/R12**: `git fetch origin "$DEPLOY_REF"` es la primera acción con efecto y
  aborta sin modificar nada si falla. Documentadas deploy key de solo lectura y
  remoto SSH.
- **V4**: `scripts/smoke.py` (key temporal, canal, evento, lectura y borrado en
  `finally`) con credenciales solo por `curl -K -` (stdin); sustituye §2.3/§2.4.
- **V5**: `estado.py` sin dominio por defecto (`SOCKET_PUBLIC_URL` o `--core`).
  Documentado que cron solo loguea y cómo añadir aviso mail/webhook sin secretos
  en argv.


## Guía de migración rápida (fases 2 → 7)

Resumen de TODO lo incompatible y qué hacer en cada caso. El detalle está en la
sección de cada fase.

| # | Cambio incompatible (fase) | Qué debes hacer |
|---|---|---|
| 1 | La **API key ya no es token** de WS/Bearer; `sc_live_` en `Authorization`/`?token=` → 401 (F2) | Backend: pide JWT en `/auth/token` con `X-Api-Key`. Frontend: abre WS solo con el JWT. |
| 2 | Canales con **dueño**; listados/stats/clientes filtrados; canal ajeno → **404**; canales viejos sin prefijo quedan huérfanos e inaccesibles (F2) | Revisa `channel_prefix` de las keys; renombra/asigna canales huérfanos (ver `docs/DESPLIEGUE.md`). |
| 3 | `emit_event` con ramas **exclusivas**; errores de rate limit ya no se enmascaran como 401 (F2) | Ajusta clientes que esperasen 401 bajo rate limit: ahora es 429. |
| 4 | **Identidad = `sub`**; se ignora `client_id` del handshake; hay `connection_id`; targets por usuario (F2/F3) | Backend: fija `user_id` al pedir JWT. Frontend: handshake `{channel, device?}`; targets = `user_id`. |
| 5 | Prefijos **estrictos**: `a:*` no acepta `a`, `a:`, `a2:x` ni `A:x` (F2) | Usa nombres `prefijo:segmento`. |
| 6 | **`source` de WS lo impone el servidor** (`proyecto:sub`); `__system__` y tipos de sistema reservados (F3) | No falsifiques `source`; los eventos de sistema solo los emite el hub. |
| 7 | **Límites de payload**: 64 KB, profundidad 16, targets ≤50 (128 chars), frame >64 KB cierra (F3) | Reduce payloads; parte mensajes grandes. |
| 8 | Envelope de error WS `{"type":"error","code":...}`; presencia `{user, connection_id, total_clients}` **sin lista** (F3/F5) | Cambia parsers de `error` a `type/code`; usa `GET /channels/:id/clients` para la lista. |
| 9 | **JWT**: HS256, `iss`/`aud` obligatorios, leeway 5 s; tokens viejos dejan de valer (F4) | Deja expirar o pide tokens nuevos tras desplegar. |
| 10 | **Revocación activa** cierra WS al instante; fail-closed (DB caída → 500) (F4) | Maneja cierres `credential_revoked`/`token_revoked`/`channel_deleted` como definitivos. |
| 11 | Límites WS: 50/proyecto, 20/IP, handshake 8 s, ping 30 s, idle 120 s, publish 50/s por conexión (F4) | Revisa reconexiones; el SDK ya aplica backoff+jitter. |
| 12 | `sub` (`user_id`) **obligatorio** y validado (≤128, sin `*`/`:`) (F4.1) | Envía siempre un ID opaco. |
| 13 | Rol **exacto** y **scopes vivos**: `both` exige ambos scopes; retirar `publish` invalida JWT viejos para publicar (F4.1) | Revisa scopes de cada key; no confíes en tokens antiguos. |
| 14 | **Prefijos no solapan** entre keys (incluido `*`); keys revocadas siguen ocupando prefijo hasta borrarlas (F4.1) | Borra o cambia el prefijo de la key vieja antes de reutilizarlo. |
| 15 | Validación de **nombre de canal** (≤128, sin UUID, charset), `description` ≤500 (F4.1) | Ajusta nombres/descripciones. |
| 16 | Errores de DB → **500 genérico**; creación de canales **atómica** (F4.1) | No dependas de 201 con errores internos; reintenta 500. |
| 17 | **Arranque estricto**: aborta con secretos débiles/TTL fuera de rango/`ALLOWED_ORIGINS` ausente en prod (F5) | Actualiza `.env` (ver `docs/DESPLIEGUE.md §0.1`). |
| 18 | **`ALLOW_BOOTSTRAP=false`** por defecto; si se activa, la key va a `data/bootstrap-key.txt` 0600 (F5) | Usa `alta_cliente.py`; no esperes la key en logs. |
| 19 | **`/admin/*` fuera del puerto público** (404); listener en `ADMIN_PORT` loopback (F5) | Opera con `docker exec` (ver `docs/COMO-FUNCIONA.md §7`). |
| 20 | **IP de confianza**: XFF solo se lee desde `TRUSTED_PROXIES`; rate limit por IP **antes** de validar la key; 429 con `Retry-After` (F5) | Configura `TRUSTED_PROXIES` con el gateway de `central_network`. |
| 21 | Listener admin con **lockout** (5 fallos/60 s) y comparación en tiempo constante (F5) | No automatices reintentos de MASTER_KEY. |
| 22 | **Auditoría** sin secretos; errores internos ya no se filtran al cliente (F5) | Ajusta tests que buscasen detalles internos. |
| 23 | **Contenedor**: UID 10001, `read_only`, admin no publicado; el volumen debe ser escribible por 10001 (F6) | `sudo chown -R 10001:10001 data`; no publiques `ADMIN_PORT`. |
| 24 | Deploy **solo si CI pasa** (`needs`), backups fechados y rollback documentado (F6) | Configura el check de CI como requerido en `main` (ver `SECURITY.md`). |
| 25 | **SDK** nuevo y herramientas: `alta_cliente.py` ya no imprime la API key (fichero 0600) y exige dominios reales; `estado.py` usa `/admin/stats` con MASTER_KEY (F7) | Usa el paquete 0600; no esperes la key por pantalla; define `SOCKET_MASTER_KEY`. |

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


### FASE 4.1 — Endurecimiento de autorización y validación

#### Cambios incompatibles

1. **`sub` (user_id) obligatorio en `/auth/token`**: sin `user_id` → 400. Máx 128
   caracteres; charset `[A-Za-z0-9._@+-]`; prohibido `*` y `:`.
   Migración: el backend debe enviar siempre el identificador de usuario real.

2. **Rol exacto contra scopes**: `both` exige AMBOS scopes `publish` y `subscribe`
   (antes un `contains` dejaba pasar `both` con un solo scope). `publish`/`subscribe`
   exigen el scope correspondiente.

3. **Scopes vivos en la autorización JWT**: retirar `publish`/`subscribe` a una key
   invalida de inmediato los JWT viejos para esa acción (REST y WS), aunque el token
   siga firmado y vigente.

4. **Prefijos sin solape**: crear o editar una key con un `channel_prefix` que solape
   con otra (incluido `*`) devuelve 400. Solape = mismo namespace o anidado
   (`a:*` vs `a:b:*`), o comodín `*`.

5. **Validación de canales**: nombre ≤128, charset `[A-Za-z0-9._@-]` por segmento,
   segmentos no vacíos, sin `*`, y **no puede ser un UUID**; `description` ≤500.

6. **Errores de DB → 500 (no 401/201 silenciosos)**: validación de API key,
   persistencia de canales, borrado y carga al arranque propagan el error; se
   registran en log y no se filtran detalles. **Creación de canales atómica**
   (nombre reservado + persistencia con rollback; conflicto UNIQUE ⇒ 500).

7. **Límite agregado de publicación por proyecto** en WS (default 500/s, además
   del límite por conexión) y **watchdog configurable** (`auth_recheck_secs`,
   default 10 s). Al superar el agregado: `rate_limited` y cierre.


### FASE 5 — Red, secretos y configuración

#### Cambios incompatibles

1. **Arranque estricto**: el proceso aborta si `JWT_SECRET` es el valor por defecto
   o <32 caracteres, `MASTER_KEY` <32, `JWT_TTL_SECS` fuera de [60,3600], o
   `ALLOWED_ORIGINS` vacío/`*` con `APP_ENV!=dev`. `.env` existentes deben revisarse.
   Nuevas variables: `APP_ENV`, `ADMIN_PORT` (3015), `TRUSTED_PROXIES`,
   `SHUTDOWN_GRACE_SECS` (10).

2. **`ALLOW_BOOTSTRAP` ahora es `false` por defecto**. Si se activa y la DB está
   vacía, la key se escribe en `./data/bootstrap-key.txt` con permisos `0600`;
   **nunca** se loguea en claro (solo la ruta).

3. **`/admin/*` sale del puerto público**: se sirve en `ADMIN_PORT` escuchando en
   `127.0.0.1` (no publicado por Docker ni alcanzable desde otros contenedores).
   En el puerto público responden **404**. Operación: `docker exec socket-core
   curl -H "X-Master-Key: ..." http://127.0.0.1:3015/admin/stats`.

4. **IP del cliente**: `X-Forwarded-For`/`X-Real-IP` SOLO se leen si la IP del socket
   está en `TRUSTED_PROXIES` (IPs/CIDR, vacío por defecto). De XFF se usa el último
   valor. Se elimina el placeholder `"unknown"`. Nginx debe sobrescribir con
   `$remote_addr` (actualizado en `nginx/`).

5. **Rate limit por IP antes de validar la key** (las keys inválidas también
   consumen cupo) y **429 con `Retry-After`**. Las respuestas 401/403 no cambian.

6. **Listener admin endurecido**: bloqueo temporal tras 5 fallos de `MASTER_KEY`
   (60 s), comparación en tiempo constante (`subtle`) y errores genéricos.

7. **Nuevo `GET /admin/stats`** (global por proyecto: canales, conexiones, clientes;
   sin payloads). `scripts/estado.py` ahora usa este endpoint con `MASTER_KEY`
   (`SOCKET_MASTER_KEY`), ya no requiere una key con prefijo `*`.

8. **Errores sin detalles internos**: los fallos de DB/JWT hacia el cliente son
   mensajes genéricos (500 `internal error`); el detalle va solo al log.

9. **Auditoría**: log estructurado (`target=audit`) con `project`, `action` y
   `outcome` para altas/ediciones/revocaciones/borrados de keys, revocación de
   tokens, borrado de canales y autenticaciones fallidas. Nunca incluye secretos.

10. **Presencia mínima en eventos**: `client_joined`/`client_left` llevan solo
    `{user, connection_id, total_clients}`. La lista completa solo por
    `GET /channels/:id/clients` (con authz). Consumidores que leían `payload.clients`
    deben migrar a ese endpoint.

11. **Apagado con gracia**: `SHUTDOWN_GRACE_SECS` (10 s) limita el tiempo total;
    pasado ese plazo el proceso termina aunque queden WS abiertos.

#### Documentado (sin cambios de código)

- `domains` solo protege el uso desde navegador y solo al emitir el JWT; no se
  comprueba en el WebSocket.
- En REST el `source` lo fija el backend; los frontends no deben usarlo para
  decisiones de seguridad.
- `sub` debe ser un ID opaco (no email): se muestra al resto del canal.


### FASE 6 — Contenedor y CI/CD

#### Cambios

1. **Dockerfile endurecido**: imagen base fijada por versión (`rust:1.97-bookworm`,
   `debian:bookworm-slim`), usuario sin privilegios **UID/GID 10001** (sin shell),
   `HEALTHCHECK` propio contra `/health`; se instala `curl` (lo usan el healthcheck y `docker exec`).

2. **docker-compose**: `read_only: true`, `cap_drop: [ALL]`,
   `no-new-privileges:true`, `tmpfs /tmp`, límites de memoria (256 MB), CPU (1.0) y
   `pids_limit`. El listener admin sigue sin publicarse.

3. **CI** (`.github/workflows/ci.yml`) en PR y push: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets` y
   `cargo audit`. Actions fijadas por versión (`checkout@v4`,
   `rust-toolchain@1.97.0`, `rust-cache@v2`, `install-action@v2`).

4. **Deploy dependiente de CI**: `deploy.yml` ahora usa `workflow_run` sobre el
   workflow `CI` para `main` y solo despliega si la conclusión es `success`.

5. **SECURITY.md**: pasos manuales de GitHub (repo privado, 2FA, branch protection
   con el check de CI, permisos de Actions, secret scanning, Dependabot), rotación
   de secretos, backups y permisos del volumen.

#### Operación (obligatorio tras desplegar)

- El host debe permitir escritura al UID 10001 en `./data`:
  `sudo chown -R 10001:10001 data` (el workflow de deploy ya lo hace).
- El contenedor es de solo lectura: escribe únicamente en `/app/data` y `/tmp`.
- `ci.yml` debe ser el check requerido en la protección de `main`.


#### Ampliación Fase 6 (contenedor/CI)

- **Entrypoint** `docker-entrypoint.sh`: verifica que `/app/data` sea escribible por
  UID 10001 (prueba de escritura real) y aborta con instrucciones si no lo es.
  Complementa el `chown` del Dockerfile (el bind mount del host manda).
- **HEALTHCHECK** comprueba `/health` en el listener público (3005) y en el admin
  (3015) dentro del contenedor; `/health` se añadió también al router admin.
- **Acceso admin elegido**: `docker exec -i socket-core curl ... http://127.0.0.1:3015/...`.
  `scripts/alta_cliente.py` y `scripts/estado.py` lo usan automáticamente (directo →
  fallback `docker exec`).
- **TRUSTED_PROXIES**: documentado cómo obtener el gateway de `central_network`
  (`docker network inspect ... .IPAM.Config.Gateway`) o la IP del contenedor Nginx;
  `scripts/estado.py` verifica que la configuración lo incluya.
- **Deploy**: backup fechado `backups/data-AAAAMMDD-HHMMSS.tgz` ANTES de reconstruir
  (retención 10), guarda el commit previo en `.last_good_commit`, y **rollback
  documentado** en `SECURITY.md §7` (commit anterior + restaurar backup + rebuild).
- **CI/CD unificado**: `ci.yml` con jobs `quality` y `deploy` (`needs: [quality]`,
  solo push a `main`): el deploy no arranca si CI falla. Actions fijadas por versión
  exacta. Eliminado `deploy.yml` para evitar dobles despliegues.
- **Dos bugs encontrados al probar el contenedor de verdad** (corregidos):
  1. La imagen podía incluir el **binario dummy** de cache: `COPY` conservaba mtimes
     antiguos y Cargo no recompilaba. Ahora se hace `find src -type f -exec touch`
     antes del build (verificado: binario real de 9,7 MB, no 437 KB).
  2. El servicio se **auto-apagaba** a los `SHUTDOWN_GRACE_SECS` (10 s) porque el
     timeout de gracia se iniciaba al arrancar. Ahora la gracia empieza solo al
     recibir la señal; con test de regresión (`servidor_no_se_apaga_solo_antes_de_la_senal`).
- Prueba real del contenedor: `--read-only --tmpfs /tmp --cap-drop ALL
  --security-opt no-new-privileges` + volumen: arranca como UID 10001, crea
  `socket-core.db` propio, ambos `/health` OK, `docker stop` en ~1 s. Prueba
  negativa: volumen no escribible ⇒ el entrypoint aborta con instrucciones.

### FASE 7 — Facilidad de uso (SDK, ejemplo, alta de clientes)

#### Nuevo

1. **SDK JavaScript** (`sdk/js/socket-core.mjs`): un solo fichero, sin dependencias,
   navegador y Node ≥ 18 (cliente WS propio si no hay `WebSocket` global).
   - `connect({ tokenProvider, channel, device?, url })`; el SDK **nunca** ve la API key.
   - Reconexión con backoff exponencial **+ jitter** y espera mayor para `rate_limited`.
   - Renovación **sin cortes** ~60 s antes de expirar: abre otra conexión, espera su
     handshake y cierra la vieja; deduplica eventos por `id` durante el solape.
   - Estrategia por código: reconecta en `token_expired`/`server_shutdown`/
     `idle_timeout`/`handshake_timeout`/red; `fatal` (sin reconectar) en
     `forbidden`/`credential_revoked`/`token_revoked`/`channel_deleted`/`not_found`
     (y `invalid_payload`/`message_too_large` como errores de cliente).
   - `on('message'|'presence'|'missed_events'|'fatal'|'status')`, `publish()` (rechazo
     local >64 KB y de rol sin `publish`), `close()`, y getter `status`.

2. **Ejemplo completo** (`examples/`): backend Express con login simulado e IDs
   opacos, `POST /socket-token` (API key solo en servidor) y chat HTML que usa el SDK.

3. **`alta_cliente.py`**: genera un **paquete de credenciales** en fichero con permisos
   `0600` (la API key ya no se imprime en el terminal), lista los prefijos existentes,
   explica de forma legible el error de **prefijo solapado**, y propone scopes mínimos
   (`publish,subscribe`) y dominios reales (avisa si se usa `*`).

4. **`docs/CODIGOS.md`**: contrato estable de códigos WS y HTTP, con causa y acción.
   Un test (`tests/codigos_doc.rs`) falla si `src/` usa un código no documentado.

#### Cambios de comportamiento de herramientas

- `alta_cliente.py` exige al menos un dominio (no acepta vacío) y no muestra la API
  key por stdout; el paquete queda en `credenciales-<proyecto>-<fecha>.txt` (0600).
- `estado.py`/`alta_cliente.py` usan el listener admin por `docker exec` cuando el
  acceso directo a `127.0.0.1:ADMIN_PORT` no es posible.

#### Nota

- **No se tocó lógica del servidor** en esta fase; los fallos encontrados fueron del
  propio SDK/test (detección de presencia por `payload.type`, y un helper de test que
  consumía el body antes de parsearlo).


### FASE 8 — Documentación

#### Cambios

1. Nuevos documentos: `docs/COMO-FUNCIONA.md` (con salidas reales y diagrama),
   `docs/DESPLIEGUE.md` (checklist, migración sobre copia, verificación y rollback),
   y `docs/CODIGOS.md` ya existente como contrato estable.
2. **Doc ejecutable**: `scripts/test_docs_curl.py` extrae los bloques
   ```bash-doc-exec``` de `docs/COMO-FUNCIONA.md` y los ejecuta contra un hub
   temporal (falla si la documentación se desincroniza). Añadido a CI.
3. **Corrección del SDK** (no del servidor): un cierre fatal (`credential_revoked`,
   `forbidden`, ...) que llega **durante una renovación** ahora emite `fatal` y no
   entra en bucle de reintentos. Test: `credential_revoked durante renovación`.
4. `README.md` saneado (sin dominio real ni nombres de clientes) y actualizado al
   comportamiento real; `SECURITY.md` revisado y enlazado con los nuevos docs.

### Verificación previa al despliegue (Parte B)

- **B3**: 8 tests JWT (alg none, HS384, sin `aud`, `aud`/`iss` ajenos, expirado,
  firma con otro secreto y `sc_live_` como token) contra REST y WS → 401.
- **B4**: mutaciones manuales de 6 tests críticos; todos fallan al romper el código
  (XFF, `/admin` público, auditoría, lockout, rol `both`, scopes vivos). Sin cambios
  de código (solo verificación).
- **B6 (SDK)**: se elimina el cliente WS casero (se exige `WebSocket` global, Node
  ≥ 22); `tokenProvider` 401/403 → `fatal` y 5xx/red → backoff; throttle ≥220 ms
  entre tokens (5 req/s); dedupe acotado a 1000 ids; `close()` limpia timers/sockets;
  el ejemplo cuenta conexiones por usuario durante el solape de renovación.
- **B7**: el error de solape de prefijos **nombra la key** (incluidas revocadas) y
  `alta_cliente.py` explica cómo liberar el prefijo (borrar/editar la key).
- **B2**: deploy con espera de 60 s a ambos `/health` y **rollback automático**
  (commit previo + backup + rebuild + verificación; job `exit 1`).
- **B1**: documentado el UPDATE manual para asignar canales huérfanos en
  `docs/DESPLIEGUE.md`.


### Verificación pre-despliegue (S1–S8)

- **S1/S4**: errores de publish no fatales (`forbidden`/`invalid_payload`/
  `message_too_large`) emiten `publish_error` y no cierran; al cerrar el socket
  activo `publish` lanza `not_connected`.
- **S2**: fallo de DB a mitad de sesión ⇒ cierre `server_error` (reintentable);
  el SDK reconecta; key revocada sigue siendo `credential_revoked` fatal.
- **S3**: una sola reconexión por error+Close (rate_limited ≥10 s).
- **S5**: `tokenFromEndpoint` (401/403 fatal; 5xx/red backoff).
- **S6**: presencia por `Set(connection_id)` + seed `/presence`; el solape de
  renovación no marca offline.
- **S7**: ejemplo separa `SOCKET_CORE_URL`/`SOCKET_PUBLIC_URL`, `canAccess`,
  errores genéricos, sirve solo el SDK y no crea el canal.
- **S8**: jitter inicial, guía de dimensionado de `rate_limit_per_min`, y
  corrección de la prueba de migración (fue sintética; comandos reales incluidos).


### Fase 8.2 — Endurecimiento final (M1–M9)

- **M1**: sin `ConnectInfo` se responde 500 y se loguea (no se asume loopback).
- **M2**: `rate_limit_per_min` validado en **[1, 100000]** al crear/editar keys.
- **M3**: la auditoría de `auth_failed`/`rate_limited` incluye `ip=` (sin secretos).
- **M4**: `TRUSTED_PROXIES` rechaza loopback y `0/0`; las IPs IPv4-mapeadas se
  normalizan (`to_canonical`).
- **M5**: los tests JWT incluyen control positivo (REST+WS) y exigen 401 exacto con
  body JSON válido.
- **M6**: CI añade jobs Node 24 (SDK) y Python (scripts); el test de `node:sqlite`
  queda aislado (`RUN_SQLITE_TEST=1`). El deploy depende de los tres jobs.
- **M7**: renovación con jitter aleatorio (40–120 s) y jitter inicial configurable
  (0–5 s; 250 ms por defecto).
- **M8**: `examples/backend` con `express` y lockfile; `smoke.mjs` hace login,
  `/socket-token`, WS y mensaje de ida y vuelta contra un hub temporal.
- **M9**: documentado el dimensionado real (Nginx 5 r/s `/auth/token`, 10 conexiones
  WS/IP, `ip_rate_per_min` 600, cupo de emisión 600, y efecto de `TRUSTED_PROXIES`).


### Fase 8.4 — Administración de keys endurecida (P1–P8)

- **P1**: borrar una key borra en cascada sus canales (memoria + SQLite), cierra sus
  WS y audita `canales=N`; el prefijo y los nombres quedan libres.
- **P2**: nombre de key duplicado → **409** (sin detalles de DB) en alta y edición.
- **P3**: `expires_in_days` en alta `1..=3650`; en edición `0` (quitar) o `1..=3650`;
  negativos/desmesurados → 400; cálculo con `checked_mul`.
- **P4**: scopes solo `publish|subscribe|manage` (sin duplicados; `has_scope` ya no
  acepta `*`/`admin`); prefijos validados con el charset/longitud de canales;
  domains solo `*`, `*.dominio` o URL http(s).
- **P5**: corregido el log de `update()` ("update key").
- **P6**: alta atómica con `BEGIN IMMEDIATE` (nombre + solape + INSERT): dos altas
  concurrentes con el mismo prefijo, solo una gana.
- **P7**: el test de binding admin falla si no hay interfaz no-loopback salvo
  `SKIP_NET_IFACE_TEST=1` (documentado).
- **P8**: documentado el borrado en cascada, la liberación del prefijo y el efecto de
  renombrar una key en el `source` de los mensajes nuevos.


### Fase 8.5 — Empaquetado, dependencias y preflight (Q1–Q9)

- **Q1**: `stop_grace_period: 20s`; `SHUTDOWN_GRACE_SECS` validado 1–19.
- **Q2**: `.dockerignore` (sin `.env*`, `data`, `backups`, `proyectos`, `target`,
  `.git`, `node_modules`) y `.gitattributes` con `*.sh text eol=lf`.
- **Q3**: Dockerfile y CI con `cargo ... --locked`.
- **Q4**: imágenes base fijadas por digest; Dependabot para cargo, npm (SDK y
  ejemplo), github-actions y docker. Documentado el riesgo residual de `apt`.
- **Q5**: documentado que `env_file` expone secretos a `docker inspect/exec`;
  `restart: on-failure:5` evaluado y aplicado.
- **Q6**: `scripts/preflight.py` (solo lectura) con tests sobre SQLite sintética.
- **Q7**: al arrancar, WARN por key con scopes inválidos o prefijos solapados (solo
  nombres).
- **Q8**: `busy_timeout` en el pool y SQLite BUSY → **503 + Retry-After**.
- **Q9**: repetida la fila P4 del informe (ver entrega).


### Fase 8.6 — Deploy reproducible y cadena de suministro (R1–R9)

- **R1/R2/R3/R5**: deploy extraído a `scripts/deploy.sh` (ejecutado por SSH desde
  `origin/main`), probado con binarios simulados: éxito, build fallido, salud
  fallida con **rollback a `.last_good_commit`** y `nginx -t` fallido con
  restauración. `.last_good_commit` solo se escribe tras salud + chequeo público.
  Sin copia de `.env` a `/tmp` (`umask 077`). Shellcheck en CI.
- **R4**: backup consistente con la API de `sqlite3` + `integrity_check`
  (`scripts/sqlite_backup.py`) y tests de restauración.
- **R6**: concurrency por job (deploy `cancel-in-progress: false`), `environment:
  production` (revisor obligatorio, documentado), actions fijadas por **SHA** y
  `cargo-audit` fijado a 0.22.2.
- **R7**: job `docker` en CI (imagen, salud 3005/3015, uid 10001, binario >5 MB,
  FS read-only, JWT débil => exit 1). Verificado también localmente.
- **R8**: imagen reconstruida con digests; política `on-failure:5` probada con un
  contenedor que falla de verdad (5 reinicios y queda parado). No se reinició el
  daemon Docker (disruptivo).
- **R9**: `preflight.py` y `estado.py` comparan `TRUSTED_PROXIES` con el gateway de
  `central_network` y alertan de subredes anchas (/16 o mayores).


### Fase 8.7 — Nginx, fail2ban, deploy y cadena de suministro (S1–S8)

- **S1/S2**: `limit_conn_status 429`, `limit_conn ws_conn 30` (documentado el solape
  de renovación), `/admin/` => 404, `Connection` con `map`, superficie pública
  explícita (`/health`, `/auth/token`, `/ws`, `/channels`, `/stats`) y `/` => 404.
  Documentada la variante "solo /ws público".
- **S3**: `scripts/test_nginx.py` valida en contenedores (certificados de prueba y
  upstream falso): `nginx -t`, `/admin/x` y ruta desconocida 404, `/ws` con Upgrade
  y XFF real, y `limit_conn` => 429. Job CI `nginx`.
- **S4**: `ignoreip` (loopback + red Docker), sample log y validación real con
  `fail2ban-regex` (2 coincidencias 401/403; 200/429/tricky fuera). Job CI
  `fail2ban`. Documentado el alcance a 80/443 de todos los dominios y recidive.
- **S5**: `restart: unless-stopped` (on-failure no está probado ante reinicios del
  daemon/VPS).
- **S6**: el deploy re-ejecuta el script de origin si difiere; hoy CI ejecuta
  directamente `origin/main:scripts/deploy.sh` (documentado).
- **S7**: `docs/sudoers-deploy.example` (no aplicado) y aviso en SECURITY de que
  push a main = ejecución en el VPS.
- **S8**: `scripts/verify_action_shas.py` comprueba que cada SHA de action coincide
  con la etiqueta del comentario; integrado en CI.


### Fase 8.8 — Secretos fuera de argv y alta segura (U1–U6)

- **U1**: `estado.py` y `alta_cliente.py` pasan la MASTER_KEY a curl por **stdin**
  (`curl -K -`) dentro de `docker exec`; nunca en argv ni en cadenas de shell.
  Tests con `subprocess.run` simulado.
- **U2**: `estado.py` ya no lee `.master_key`; solo env/arg. Documentado cron/systemd
  con `EnvironmentFile` 0600 de root.
- **U3**: salida por defecto `~/credenciales-socketcore` (0700), `credenciales-*.txt`
  ignorado en git/Docker, rechazo a escribir en worktree sin ignorar, paquete sin
  admin/contenedor/MASTER y ficha de operador aparte, validación numérica clara y
  sin dominio fijo (env/`--core`).
- **U4**: 33 tests de Python en el job `python`; `test_nginx.py` en job `nginx`,
  `test_fail2ban.sh` en job `fail2ban` y `verify_action_shas.py` en `quality`.
  Los scripts hacen skip explícito solo en local sin Docker/fail2ban/red; en CI fallan.
- **U5**: filtro fail2ban final validado con `fail2ban-regex` (2 coincidencias
  401/403; 200/429 y línea tricky fuera).
- **U6**: `docs/sudoers-deploy.example` documenta por qué **no** se delega Docker por
  sudoers y la alternativa con wrappers root-owned; procedimiento y rollback (no se aplica).

### Nota de estado

- La suite de integración en `tests/` define el comportamiento objetivo: los
  tests aún en rojo corresponden a la fase 5 (red, secretos y configuración).

#### Mantener en mente (no incompatible)

- `Cargo.toml` ya tiene el nombre canónico (antes `cargo.toml` minúsculas al repo).
- Dependencias depuradas: fuera `tokio-tungstenite` y `url` de producción;
  `dotenv` → `dotenvy`; sqlx sin features por defecto (sqlite/runtime-tokio/macros).
- Suite de integración en `tests/` ahora referencia el comportamiento objetivo.
