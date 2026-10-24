# socket-core definitivo — hub seguro multi-proyecto

Servicio único de transporte en tiempo real (REST + WebSocket) para todos los proyectos presentes y futuros.
Cada proyecto se conecta con sus propias credenciales y solo ve sus canales. Sin Tailscale.

```text
┌─────────────┐   X-Api-Key    ┌──────────────┐   JWT (10 min)   ┌──────────┐
│ Tu backend  │ ─────────────▶ │ socket-core  │ ───────────────▶ │ Frontend │
│ (servidor)  │  /auth/token   │  :3005       │  /ws?token=...   │ (naveg. ) │
└─────────────┘                └──────────────┘                  └──────────┘
        │ REST directo con X-Api-Key (emitir, crear canales) ▲
        └────────────────────────────────────────────────────┘
```

---

## 1. Cómo funciona (el modelo en 5 minutos)

Hay **3 credenciales**, de mayor a menor poder:

| Credencial | Qué es | Dónde vive | Poder |
|---|---|---|---|
| `MASTER_KEY` | Contraseña madre del core | Solo `.env` del VPS, nunca sale de ahí | Crear / listar / revocar / borrar API keys y JWTs (`/admin/*`) |
| `API Key` (`sc_live_...`) | Identidad de **un proyecto** | Backend de cada proyecto (secreto) | Pedir JWTs, usar el REST, conectarse al WS como servicio |
| `JWT` (`eyJ...`) | Pase temporal de **un usuario** | Frontend / cliente (vive 10 min) | Entrar al WS y hablar solo en su canal y rol |

Cada API Key además lleva **candados propios**:

- **`domains`**: desde qué orígenes web puede usarse (ej: `["https://panel.midominio.com"]`). Si el `Origin` no coincide → `403`. Sin `Origin` (backend-a-backend con curl) se permite, porque ya presenta la key.
- **`channel_prefix`**: qué canales puede tocar (ej: `"monitoreo:*"`). `monitoreo` jamás ve ni emite en `jarvis:*`.
- **`scopes`**: qué acciones (`publish`, `subscribe`, `manage`). Crear canales exige `manage`; emitir exige `publish`.
- **`rate_limit_per_min`**: tope de peticiones por minuto de esa key (defecto 120).
- **`expires_at` / `revoked`**: caducidad y revocación inmediata. Revocar = efecto instantáneo, sin redeploy.

El JWT pide con la API Key y dura `JWT_TTL_SECS` (defecto 600 s). Lleva `channel` + `role` (`publish` / `subscribe` / `both`): un JWT `subscribe` no puede emitir, y uno atado a `demo:alertas` no entra a otro canal. El WS **corta la conexión al expirar** el JWT: el cliente pide otro y reconecta.

---

## 2. Despliegue

```bash
cp .env.example .env
# Editar: JWT_SECRET (openssl rand -hex 64, >=32), MASTER_KEY (openssl rand -hex 32, >=32)
# En prod: APP_ENV=prod, ALLOWED_ORIGINS=https://tu-dominio.com, ALLOW_BOOTSTRAP=false
# El proceso NO arranca si faltan secretos fuertes o ALLOWED_ORIGINS (ver §6).
# ADMIN_PORT (3015) es loopback del contenedor: no se publica en Docker.

# Local
cargo run

# Docker (red externa compartida con tus otros proyectos)
docker network create central_network   # solo una vez
docker compose up -d --build
curl http://127.0.0.1:3005/health       # {"status":"ok","service":"socket-core"}
```

En el VPS el deploy es automático por GitHub Actions (push a `main`, **solo si CI
pasa**); lo único manual es pegar el `.env` una vez. Antes de reconstruir se guarda
`backups/data-<fecha>.tgz` (retención 10) y el commit previo en
`.last_deploy_commit`; el rollback está documentado en `SECURITY.md §7`.

**TRUSTED_PROXIES**: el hub ve el tráfico desde el gateway de `central_network`
(o la IP del contenedor de Nginx si está en esa red). Averígualo con:

```bash
docker network inspect central_network --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
```

y ponlo en `TRUSTED_PROXIES`. Sin esa lista `X-Forwarded-For` se ignora (más
seguro; el rate limit por IP usará el gateway). `scripts/estado.py` lo verifica.

**Contenedor**: corre como UID 10001 con FS de solo lectura. El volumen `data/`
debe ser escribible por ese UID (`sudo chown -R 10001:10001 data`); el entrypoint
aborta con mensaje claro si no lo es. El `HEALTHCHECK` prueba `/health` en 3005 y 3015.

---

## 3. Paso a paso: de cero a mensaje en vivo

Fija la base (cambia el host si es el VPS por dominio/Nginx):

```bash
CORE=http://127.0.0.1:3005
```

### Paso 1 — Salud del servicio (público, sin auth)

```bash
curl $CORE/health
# {"status":"ok","service":"socket-core"}
```

### Paso 2 — Crear la API Key del proyecto (requiere `MASTER_KEY`)

Solo el administrador del core hace esto, **una vez por proyecto**. La `api_key` se muestra **una sola vez**: guárdala en el backend del proyecto.

```bash
MASTER_KEY=$(grep ^MASTER_KEY= .env | cut -d= -f2)
ADMIN=http://127.0.0.1:3005   # /admin/* SOLO por localhost (bloqueado desde internet)
curl -X POST $ADMIN/admin/keys \
  -H "X-Master-Key: $MASTER_KEY" -H "Content-Type: application/json" -d '{
    "name": "demo",
    "domains": ["https://panel.midominio.com"],
    "channel_prefix": "demo:*",
    "scopes": ["publish", "subscribe", "manage"],
    "rate_limit_per_min": 120
  }'
```

Respuesta (ejemplo):

```json
{"success":true,"data":{
  "id":"<KEY_ID>",
  "name":"demo",
  "api_key":"sc_live_...",
  "domains":["https://panel.midominio.com"],
  "channel_prefix":"demo:*",
  "scopes":["publish","subscribe","manage"],
  "rate_limit_per_min":120,
  "expires_at":null},"error":null}
```

Guarda `id` (para revocar) y `api_key` (para usar):

```bash
RAW=sc_live_...   # la api_key recibida
```

> **Atajo:** en el VPS corre `python3 scripts/alta_cliente.py` — te pide nombre, dominios, prefijo, scopes y límite, confirma, y te imprime el bloque de credenciales listo para copiar (lee `MASTER_KEY` del `.env` local).

> En local/dev puedes usar `"domains":["*"]`. En prod, siempre dominios reales.

### Paso 3 — Pedir un JWT para un usuario (requiere `X-Api-Key`)

El **backend** del proyecto pide pases cortos para sus usuarios. El `Origin` debe estar en los `domains` de la key.

```bash
curl -X POST $CORE/auth/token \
  -H "X-Api-Key: $RAW" -H "Origin: https://panel.midominio.com" \
  -H "Content-Type: application/json" \
  -d '{"user_id":"user-1","channel":"demo:alertas","role":"both"}'
```

Respuesta:

```json
{"success":true,"data":{
  "token":"eyJ...",
  "expires_in":600,
  "channel":"demo:alertas",
  "role":"both"},"error":null}
```

- `channel: "*"` = acceso a todo su prefijo (`demo:*`). Restringe al canal exacto siempre que puedas.
- `role`: `subscribe` (solo escucha), `publish` (solo emite), `both` (las dos).
- Si el canal pedido está fuera de su prefijo → `403`. Si el `Origin` no está en sus dominios → `403`.

```bash
TOK=eyJ...   # el token recibido
```

### Paso 4 — Crear el canal (requiere key con scope `manage`)

```bash
curl -X POST $CORE/channels \
  -H "X-Api-Key: $RAW" -H "Content-Type: application/json" \
  -d '{"name":"demo:alertas","description":"Alertas del panel demo"}'
```

Respuesta:

```json
{"success":true,"data":{
  "id":"<CHANNEL_ID>",
  "name":"demo:alertas",
  "description":"Alertas del panel demo",
  "created_at":1791788800,
  "clients_count":0},"error":null}
```

Guarda el `id` (vale también el `name` para el WS y los eventos):

```bash
CH=<CHANNEL_ID>
```

> Los canales se **persisten en SQLite**: sobreviven a reinicios y deploys conservando su `id`
> (los JWT atados a ellos siguen valiendo). Lo que NO sobrevive son los clientes conectados:
> al reiniciar, cada frontend reconecta solo. Tenlo en cuenta para la lógica de reconexión del §5.2.

> El nombre debe respetar el `channel_prefix` de la key (`demo:otro` → `403`).

### Paso 5 — Suscribirse por WebSocket (requiere JWT)

URL: `ws://HOST:3005/ws?token=JWT` (o `wss://` tras Nginx). Nada más conectar, el cliente **debe** mandar el handshake como primer mensaje:

```json
{"channel": "<CHANNEL_ID o name>", "client_id": "front-1"}
```

Ejemplo en navegador:

```js
const ws = new WebSocket(`ws://127.0.0.1:3005/ws?token=${TOK}`);
ws.onopen = () => ws.send(JSON.stringify({ channel: "demo:alertas", client_id: "front-1" }));
ws.onmessage = (e) => console.log("evento:", JSON.parse(e.data));
// Al recibir {"error":"token expirado, reconectar"} -> pedir JWT nuevo al backend y reconectar.
```

Desde terminal (si tienes `wscat`: `npm i -g wscat`):

```bash
wscat -c "ws://127.0.0.1:3005/ws?token=$TOK"
# > {"channel":"demo:alertas","client_id":"terminal-1"}
# < {"id":"...","channel":"...","source":"__system__",...,"payload":{"type":"client_joined",...}}
```

Reglas del WS:

- Sin token → `401` y no hay upgrade. Con token de otro canal → `{"error":"No autorizado para este canal"}` y se cierra.
- JWT `subscribe`/`both` para entrar; solo `publish`/`both` pueden **emitir** escribiendo JSON por el socket (se re-emite a `*` del canal).
- Al expirar el JWT el servidor cierra con `{"error":"token expirado, reconectar"}`: el frontend pide JWT fresco al backend y reconecta.

### Paso 6 — Emitir un evento por REST (requiere key `publish` o JWT `publish`)

El backend emite así (lo verán al instante todos los suscritos con `targets` que los incluya):

```bash
curl -X POST $CORE/channels/$CH/events \
  -H "X-Api-Key: $RAW" -H "Content-Type: application/json" -d '{
    "source": "mi-backend",
    "targets": ["*"],
    "payload": {"msg": "hola mundo", "nivel": "info"}
  }'
# {"success":true,"data":{"success":true}}
```

- `targets: ["*"]` = todos los suscritos; o lista de `client_id` para mensajes dirigidos.
- El suscriptor recibe el `Event` completo: `{id, channel, source, targets, payload, timestamp}`.

### Paso 7 — Inspeccionar (requiere `X-Api-Key`)

```bash
curl $CORE/stats -H "X-Api-Key: $RAW"                 # {"channels":1,"total_clients":2}
curl $CORE/channels -H "X-Api-Key: $RAW"              # lista de canales
curl $CORE/channels/$CH/clients -H "X-Api-Key: $RAW"  # clientes conectados al canal
```

### Paso 8 — Administrar keys (requiere `MASTER_KEY`, SOLO por localhost)

```bash
# El listener admin vive en 127.0.0.1:3015 DENTRO del contenedor (no publicado).
# Vía soportada desde el host:
dc() { docker exec -i socket-core curl -sS "$@"; }
MASTER=$(grep ^MASTER_KEY= .env | cut -d= -f2)

# Editar una key: solo lo enviado, aplica al instante sin reiniciar
dc -X PATCH http://127.0.0.1:3015/admin/keys/<KEY_ID> \
  -H "X-Master-Key: $MASTER" -H "Content-Type: application/json" \
  -d '{"rate_limit_per_min":3000}'
# Campos editables: name, domains, channel_prefix, scopes, rate_limit_per_min,
#   expires_in_days (días desde hoy; 0 = quitar expiración; omitido = no tocar)
# Quitar acceso a un PROYECTO entero (efecto inmediato)
dc -X POST http://127.0.0.1:3015/admin/keys/<KEY_ID>/revoke -H "X-Master-Key: $MASTER"
# Borrar la key definitivamente
dc -X DELETE http://127.0.0.1:3015/admin/keys/<KEY_ID> -H "X-Master-Key: $MASTER"
# Revocar un JWT concreto (sácale el jti decodificando el payload base64)
dc -X POST "http://127.0.0.1:3015/admin/tokens/revoke?jti=<JTI>" \
  -H "X-Master-Key: $MASTER" -H "Content-Type: application/json" -d '{}'
# Ver todas las keys (sin exponer los secretos, solo metadatos)
dc http://127.0.0.1:3015/admin/keys -H "X-Master-Key: $MASTER"
# Estadísticas globales por proyecto (canales, conexiones, clientes) — sin payloads
dc http://127.0.0.1:3015/admin/stats -H "X-Master-Key: $MASTER"
```

`scripts/alta_cliente.py` y `scripts/estado.py` hacen esto automáticamente
(acceso directo → fallback `docker exec`).

---

## 3.1 Hechos de seguridad que debes asumir

- **`domains` solo protege desde navegador** y **solo al emitir el JWT** (`/auth/token`).
  No se comprueba en el WebSocket ni protege llamadas de backend. Es una defensa
  contra uso no autorizado del token desde webs ajenas, no un control de acceso.
- **En REST, `source` lo fija el backend** que tiene la API key. En WS el servidor lo
  sobrescribe con `proyecto:sub`. Los frontends **no deben confiar en `source`** para
  decisiones de seguridad (cualquiera puede leerlo en un evento recibido).
- **`sub` debe ser un ID opaco** (no un email): se muestra al resto del canal en
  presencia (`/channels/:id/clients`) y en eventos de sistema (`user`).

---

## 4. Referencia de endpoints

| Método | Ruta | Auth | Notas |
|---|---|---|---|
| GET | `/health` | pública | `{"status":"ok"}` |
| POST | `/auth/token` | `X-Api-Key` + `Origin` permitido | Body: `{user_id (obligatorio), channel? ("*"), role? ("both")}` |
| GET | `/stats` | `X-Api-Key` | `{channels, total_clients}` |
| GET/POST | `/channels` | `X-Api-Key` (crear exige scope `manage` + prefijo) | Body crear: `{name, description?}` |
| GET | `/channels/:id` | `X-Api-Key` | `:id` = id o nada; acepta id |
| DELETE | `/channels/:id` | `X-Api-Key` scope `manage` + prefijo | acepta id o nombre; borra de memoria y DB (no revive) |
| GET | `/channels/:id/clients` | `X-Api-Key` | clientes del canal |
| POST | `/channels/:id/events` | `X-Api-Key` (`publish`) o JWT (`publish`) | Body: `{source, targets, payload}` |
| GET | `/ws?token=` | SOLO JWT (`subscribe`); las API keys no son tokens | handshake `{"channel", "device?"}`; identidad = `sub` |
| PATCH | `/admin/keys/:id` | `X-Master-Key` | edita solo lo enviado: `name, domains, channel_prefix, scopes, rate_limit_per_min, expires_in_days` (`0` = sin expiración) |
| POST | `/admin/keys/:id/revoke` | `X-Master-Key` | revocación inmediata |
| DELETE | `/admin/keys/:id` | `X-Master-Key` | borrado definitivo |
| POST | `/admin/tokens/revoke?jti=` | `X-Master-Key` | revoca un JWT |
| GET | `/admin/stats` | `X-Master-Key` | global por proyecto (sin payloads) |

Las rutas `/admin/*` NO existen en el puerto público: se sirven en `ADMIN_PORT`
(127.0.0.1, no publicado por Docker). `GET/POST /admin/keys` listan/crean keys.

Errores típicos:

| Código | Significado |
|---|---|
| `401` | falta/rotó la credencial: sin `X-Api-Key`, key revocada/expirada, JWT inválido/expirado/revocado, WS sin token |
| `403` | credencial válida pero sin permiso: `Origin` fuera de `domains`, canal fuera de `channel_prefix`, scope/rol insuficiente |
| `404` | canal o key inexistente |
| `429` | `rate_limit` de la key o de la IP excedido (incluye `Retry-After`) |

---

## 5. Cómo conectarse (guía para clientes)

Bases públicas (TLS siempre):

```text
REST: https://ws.seventwo.tech
WS:   wss://ws.seventwo.tech/ws?token=JWT
```

> Desde dentro del VPS / Docker (`central_network`) puedes usar directo
> `http://socket-core:3005` y `ws://socket-core:3005` (sin salir a internet).
> El `/admin/*` **solo** funciona por localhost, nunca por el dominio público.

### 5.1 El backend pide el JWT (los frontends nunca ven la API Key)

```bash
curl -X POST https://ws.seventwo.tech/auth/token \
  -H "X-Api-Key: sc_live_..." -H "Origin: https://tu-frontend.com" \
  -H "Content-Type: application/json" \
  -d '{"user_id":"user-1","channel":"tuproyecto:chat:123","role":"both"}'
# -> {"data":{"token":"eyJ...","expires_in":600,...}}
```

Mismo flujo en Node:

```js
const r = await fetch("https://ws.seventwo.tech/auth/token", {
  method: "POST",
  headers: { "Content-Type": "application/json", "X-Api-Key": process.env.SOCKET_API_KEY, "Origin": "https://tu-frontend.com" },
  body: JSON.stringify({ user_id: "user-1", channel: "tuproyecto:chat:123", role: "both" }),
});
const { data: { token } } = await r.json();
```

### 5.2 El frontend abre el WS y hace handshake

```js
const ws = new WebSocket(`wss://ws.seventwo.tech/ws?token=${token}`);

// 1. Primer mensaje OBLIGATORIO tras conectar:
ws.onopen = () => ws.send(JSON.stringify({ channel: "tuproyecto:chat:123", client_id: "front-1" }));

// 2. Eventos que llegan (canal, sistema, mensajes):
ws.onmessage = (e) => {
  const ev = JSON.parse(e.data); // {id, channel, source, targets, payload, timestamp}
  if (ev.payload?.type === "client_joined") return pintarPresencia(ev.payload);
  if (ev.source === "__system__") return;
  pintarMensaje(ev);
};

// 3. El JWT vive 10 min: al expirar el servidor cierra con aviso -> pedir otro y reconectar:
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.error === "token expirado, reconectar") return reconectar(); // pide JWT fresco al §5.1
  /* ... */
};
ws.onclose = () => setTimeout(reconectar, 2000); // reintento simple
```

Prueba desde terminal (`npm i -g wscat`):

```bash
wscat -c "wss://ws.seventwo.tech/ws?token=$TOK"
# > {"channel":"tuproyecto:chat:123","client_id":"terminal-1"}
```

### 5.3 El backend publica (REST con API Key, o WS con rol publish)

```bash
curl -X POST https://ws.seventwo.tech/channels/<CHANNEL_ID>/events \
  -H "X-Api-Key: sc_live_..." -H "Content-Type: application/json" -d '{
    "source": "mi-backend",
    "targets": ["*"],
    "payload": {"msg": "hola mundo"}
  }'
```

`targets: ["*"]` = todos los suscritos; o `["front-1","front-2"]` para dirigidos.
Desde el WS también se emite escribiendo el JSON del payload (requiere rol `publish`/`both`).

### 5.4 Reglas que no perdonan

- Sin `?token=` no hay upgrade (`401`). Con JWT de otro canal/prefijo: `{"error":"No autorizado"}` y cierre.
- JWT `subscribe` entra pero no emite; `publish` emite pero ojo: para suscribirse necesita `subscribe`/`both`.
- El WS se corta al expirar el JWT: **siempre** implementa pedir-token + reconectar.
- `curl` no habla `wss://`: para probar el handshake usa `https://.../ws` con headers `Upgrade` (ver §3 paso 6).

## 6. Operación

- **Backups que importan:** `.env` (secretos) y `data/socket-core.db` (keys + revocaciones). El workflow de deploy los preserva solos en cada despliegue.
- **Exposición pública (`ws.seventwo.tech`):** el deploy instala `nginx/` (puertos 80/443, TLS, rate-limits, `/admin/` solo-localhost, logs sin `?token=`) y `fail2ban/` (30×401/403 en 5 min → ban 1h). El certificado se crea con `certbot certonly --webroot` **solo si falta y solo para ese dominio**; `nginx -t` corre antes de cada reload para no tumbar los demás dominios.
- **Rotar secretos:** cambia `JWT_SECRET`/`MASTER_KEY` en `.env`, `docker compose up -d`, y regenera las API keys (las viejas siguen hasheadas contra nada que cambie, pero los JWT viejos mueren al cambiar el secret).
- **Logs:** `docker compose logs -f socket-core` (`RUST_LOG=info`).
- **Monitoreo:** `python3 scripts/estado.py` muestra contenedor, health, stats, canales con conectados, Nginx, certificado y fail2ban, con veredicto final (`exit 0` = bien, `1` = alertas, apto para cron). Necesita una key ops (`ops-monitoreo`, prefijo `*`) vía `SOCKET_OPS_KEY`, `--key` o archivo `.ops_key`. Solo-core público desde cualquier PC: `... --solo-core --core https://ws.seventwo.tech`.
- **Recrear `ops-monitoreo` (si se pierde o en un VPS nuevo):** `python3 scripts/alta_cliente.py` con nombre `ops-monitoreo`, dominios `*`, prefijo `*`, scopes por defecto, rate `600`, sin vigencia; guarda la `api_key` en `.ops_key` (ignorado por git). Bitácora privada completa: `proyectos/RUNBOOK.md` (solo vive en el VPS, nunca se commitea).
- **Ponerlo detrás de TLS:** Nginx → `proxy_pass http://127.0.0.1:3005` (el compose solo expone loopback a propósito); el WS pasa con `Upgrade`/`Connection`.

## 7. Código (mapa rápido)

```text
src/
  main.rs            arranque (ENV → config → DB → serve)
  lib.rs             router + CORS estricto + headers de seguridad + serve()
  config.rs          AppConfig desde ENV
  db.rs              SQLite + schema (api_keys, revoked_tokens)
  auth.rs            JWT (crear/validar) + hash y generación de API keys
  keys.rs            KeyStore (CRUD/validar) + políticas: domains, prefijos, scopes
  middleware.rs      require_master / require_api_key / require_jwt + rate-limit + Origin
  handlers.rs        REST + /auth/token + /admin/*
  websocket.rs       pre-auth antes del upgrade + autorización por canal/rol + corte al expirar JWT
  channel_manager.rs canales en memoria (DashMap + broadcast)
  models.rs          Event, Channel, DTOs de auth/admin
```

Tests: `cargo test` (políticas de origen/canal/scopes + roundtrip JWT).
