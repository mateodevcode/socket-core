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
`.last_good_commit`; el rollback está documentado en `SECURITY.md §7`.

**TRUSTED_PROXIES**: el hub ve el tráfico desde el gateway de `central_network`
(o la IP del contenedor de Nginx si está en esa red). Averígualo con:

```bash
docker network inspect central_network --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
```

y ponlo en `TRUSTED_PROXIES`. Sin esa lista `X-Forwarded-For` se ignora (más
seguro; el rate limit por IP usará el gateway). `scripts/estado.py` lo verifica.

**IP_RATE_PER_MIN**: peticiones por minuto por IP antes de validar la key
(1–100000, 600 por defecto). Aplica también al cupo de emisión. Si varios
backends llaman directo por `central_network`, comparten la IP de su contenedor
o la del gateway; ajústalo al agregado real.

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
MASTER=$(grep ^MASTER_KEY= .env | cut -d= -f2)
# El listener admin vive en 127.0.0.1:3015 DENTRO del contenedor (no publicado).
dc() { docker exec -i socket-core curl -sS "$@"; }
dc -X POST http://127.0.0.1:3015/admin/keys \
  -H "X-Master-Key: $MASTER" -H "Content-Type: application/json" -d '{
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

> **Atajo:** `python3 scripts/alta_cliente.py` crea el proyecto y escribe un **paquete de credenciales** en un fichero `0600` (la API key no se imprime en el terminal), lista los prefijos en uso y explica el error si el prefijo solapa con otro proyecto.

> `domains` solo se comprueba **al emitir el JWT desde un navegador**. En prod usa
> dominios reales; `*` permite cualquier web.

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

URL: `ws://HOST:3005/ws?token=JWT` (o `wss://` tras Nginx). El token va en la query
porque el navegador no permite cabeceras en WebSocket; Nginx está configurado para
**no registrar query strings**. Nada más conectar, envía el handshake:

```json
{"channel": "demo:alertas", "device": "front-1"}
```

`device` es opcional y se sanea. **La identidad no la elige el cliente**: es el
`sub` del JWT. Un usuario puede tener varias conexiones a la vez.

Recomendado: usa el SDK, que gestiona handshake, renovación y reconexión:

```js
import { connect } from "./socket-core.mjs";
const sc = connect({
  url: "ws://127.0.0.1:3005",
  channel: "demo:alertas",
  device: "front-1",
  tokenProvider: async () => {
    const r = await fetch("/socket-token", { method: "POST" }); // tu backend
    const j = await r.json();
    return { token: j.token, expires_in: j.expires_in };
  },
});
sc.on("message", (ev) => console.log("evento:", ev));
```

Reglas del WS:

- Sin JWT válido no hay upgrade (`401`). Una **API key no es un token** (401).
- `subscribe`/`both` pueden entrar; solo `publish`/`both` pueden emitir JSON por el socket.
- Cierre al expirar el JWT con `{"type":"error","code":"token_expired"}`; el SDK renueva
  antes de que ocurra y reconecta si hace falta.
- Códigos y acciones: [`docs/CODIGOS.md`](docs/CODIGOS.md).

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

- `targets: ["*"]` = todos los suscritos; o lista de `user_id` (`sub`) para mensajes dirigidos.
- El suscriptor recibe el `Event` completo: `{id, channel, source, targets, payload, timestamp}`.
  En REST, `source` lo fija el backend; por WS el servidor lo sobrescribe con `proyecto:sub`.

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

La vía recomendada es el **SDK** (`sdk/js/socket-core.mjs`), que gestiona
reconexión, renovación del JWT y deduplicación. Detalle completo en
[`docs/COMO-FUNCIONA.md`](docs/COMO-FUNCIONA.md) y [`sdk/js/README.md`](sdk/js/README.md).

```js
import { connect } from "./socket-core.mjs";

const sc = connect({
  url: "wss://hub.ejemplo.com",       // o https://hub.ejemplo.com
  channel: "tuproyecto:chat",
  device: "web",
  tokenProvider: async () => {         // lo implementa TU backend
    const r = await fetch("/socket-token", { method: "POST" });
    const j = await r.json();
    return { token: j.token, expires_in: j.expires_in };
  },
});
sc.on("message", (ev) => console.log(ev.source, ev.payload));
```

### 5.1 El backend pide el JWT (los frontends nunca ven la API key)

```bash
curl -X POST https://hub.ejemplo.com/auth/token \
  -H "X-Api-Key: sc_live_..." -H "Content-Type: application/json" \
  -d '{"user_id":"<id-opaco>","channel":"tuproyecto:chat","role":"both"}'
# -> {"data":{"token":"***","expires_in":600,"channel":"tuproyecto:chat","role":"both"}}
```

`user_id` es **obligatorio**, opaco (sin `*` ni `:`, <=128) y se muestra al resto del
canal en la presencia. El `Origin` solo se comprueba si la petición lo trae (navegador).

### 5.2 Navegador: WS con JWT y handshake

El SDK abre `ws(s)://.../ws?token=<JWT>` y envía como primer mensaje
`{"channel":"tuproyecto:chat","device":"web"}`. Los eventos normales son
`{id, channel, source, targets, payload, timestamp}`; la presencia llega como
`client_joined`/`client_left` (solo `user`, `connection_id`, `total_clients`).
La lista completa se consulta con `GET /channels/:id/clients` (con `X-Api-Key`).

### 5.3 Publicar

Por WS con el SDK: `await sc.publish({type:"chat",text:"hola"},{targets:["user-1"]})`.
`targets` se refieren a `user_id` (o `*`). El servidor sobrescribe `source`.

Por REST desde el backend (API key con scope `publish`):

```bash
curl -X POST https://hub.ejemplo.com/channels/tuproyecto:chat/events \
  -H "X-Api-Key: sc_live_..." -H "Content-Type: application/json" \
  -d '{"source":"mi-backend","targets":["*"],"payload":{"type":"chat","text":"hola"}}'
```

### 5.4 Reglas que no perdonan

- Sin JWT válido no hay upgrade (`401`). Una **API key no es un token** (401).
- JWT acotado a un canal no entra en otro; canal ajeno = `404`/`fatal not_found`.
- El WS se cierra al expirar el JWT (`token_expired`): el SDK renueva antes y reconecta.
- Códigos estables y acciones recomendadas: [`docs/CODIGOS.md`](docs/CODIGOS.md).

## 5.5 SDK, ejemplo y códigos de error

- **SDK JS** (un fichero, sin dependencias): [`sdk/js/README.md`](sdk/js/README.md).
- **Ejemplo completo** (backend Express + chat): [`examples/README.md`](examples/README.md).
- **Códigos de error estables** (WS y HTTP): [`docs/CODIGOS.md`](docs/CODIGOS.md).

## 6. Operación

- **Backups:** el deploy guarda `backups/data-<fecha>.tgz` antes de reconstruir
  (retención 10) y el commit previo en `.last_good_commit`. Ver
  [`docs/DESPLIEGUE.md`](docs/DESPLIEGUE.md).
- **Monitoreo:** `SOCKET_PUBLIC_URL=https://tu-dominio SOCKET_MASTER_KEY=... python3 scripts/estado.py` — contenedor,
  salud, `/admin/stats` por proyecto, Nginx, certificado, fail2ban y coherencia de
  `TRUSTED_PROXIES`. No requiere ninguna API key con prefijo `*`.
- **Admin:** solo por el listener loopback (`docker exec`), nunca por el dominio
  público. Ejemplos en `docs/COMO-FUNCIONA.md §7`.
- **Rotar secretos:** `JWT_SECRET` solo invalida JWT (las API keys siguen);
  `MASTER_KEY` solo cambia el acceso admin. Reinicia tras cambiarlos.
- **Logs:** `docker compose logs -f socket-core` (`RUST_LOG=info`); los eventos de
  auditoría llevan `target=audit` y nunca contienen secretos.
- **TLS/red:** Nginx delante (proxy al puerto loopback); el admin no se publica y el
  contenedor corre como UID 10001 con FS de solo lectura (ver `docs/DESPLIEGUE.md`).

## 7. Código (mapa rápido)

```text
src/
  main.rs            arranque estricto (valida secretos/orígenes)
  lib.rs             routers público/admin, CORS, serve con ConnectInfo y apagado
  config.rs          AppConfig (ENV) + validaciones de arranque
  db.rs              SQLite + esquema + migración idempotente de canales
  auth.rs            JWT HS256 con iss/aud + hash/generación de API keys
  authz.rs           AUTORIZACIÓN CENTRAL de canal (proyecto, canal, acción)
  keys.rs            KeyStore (CRUD) + políticas de dominios/prefijos/scopes
  middleware.rs      IP de confianza, rate limit, require_master/key/jwt
  handlers.rs        REST + /auth/token + /admin/*
  websocket.rs       gate de upgrade, límites, revalidación y cierre ordenado
  channel_manager.rs canales en memoria + dueño + creación atómica
  control.rs         bus interno de cierre (revocación/borrado/apagado)
  limits.rs          límites configurables + registro de conexiones
  audit.rs           registro de auditoría sin secretos
  models.rs          Event/Channel/DTOs + validaciones
```

Tests: `cargo test` (integración con hub temporal, aislamiento, revocación,
límites y contrato de códigos). SDK: `node --test --test-force-exit sdk/js/test/sdk.test.mjs`.
Docs ejecutables: `python3 scripts/test_docs_curl.py`.
