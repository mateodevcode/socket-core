# Ejemplo completo: backend + chat con socket-core

Mini aplicación con:

- `backend/`: Node + Express. Hace un **login simulado**, guarda un ID **opaco**
  de usuario y expone `POST /socket-token`, que pide el JWT a socket-core usando la
  **API key** (solo en el servidor; nunca en el navegador).
- `frontend/chat.html`: chat que usa el SDK (`sdk/js/socket-core.mjs`).

## Requisitos

- Hub local corriendo (ver README principal).
- Node.js ≥ 18.

## 1. Arranca el hub en local

```bash
cp .env.example .env
# Edita: JWT_SECRET (>=32), MASTER_KEY (>=32), APP_ENV=dev (CORS de desarrollo)
cargo run
# health: http://127.0.0.1:3005/health
```

## 2. Crea el canal y las credenciales (dos keys distintas)

El **canal se crea aparte**, con una key temporal de setup que tiene `manage`.
La key del backend queda **desde el principio** con solo `publish,subscribe`.

```bash
# 2.1 Key temporal SOLO para crear el canal (manage)
python3 scripts/alta_cliente.py --env .env --out .
#   Dominios: http://127.0.0.1:8080
#   Prefijo:  demo:*
#   Scopes:   publish,subscribe,manage

SETUP_FILE=$(ls -1t credenciales-demo-*.txt | head -1)
SETUP_KEY=$(grep ^API_KEY= "$SETUP_FILE" | cut -d= -f2)
SETUP_ID=$(grep ^KEY_ID= "$SETUP_FILE" | cut -d= -f2)

curl -s -X POST http://127.0.0.1:3005/channels   -H "X-Api-Key: $SETUP_KEY" -H 'Content-Type: application/json'   -d '{"name":"demo:chat","description":"canal de ejemplo"}'

# 2.2 La key de setup ya no se necesita: revócala y bórrala
MASTER=$(grep ^MASTER_KEY= .env | cut -d= -f2)
curl -s -X POST http://127.0.0.1:3015/admin/keys/$SETUP_ID/revoke -H "X-Master-Key: $MASTER"
curl -s -X DELETE http://127.0.0.1:3015/admin/keys/$SETUP_ID -H "X-Master-Key: $MASTER"
rm -f "$SETUP_FILE"

# 2.3 Key del BACKEND, con scopes mínimos desde el principio
python3 scripts/alta_cliente.py --env .env --out .
#   Dominios: http://127.0.0.1:8080
#   Prefijo:  demo:*
#   Scopes:   publish,subscribe        <- SIN manage
```

El fichero `credenciales-demo-*.txt` de la key del backend contiene la `API_KEY`.
Cópiala a una variable de entorno y **borra el fichero** después.

## 3. Arranca el backend de ejemplo

```bash
cd examples/backend
npm install
SOCKET_API_KEY=sc_live_... SOCKET_CORE_URL=http://127.0.0.1:3005 npm start
```

Abre <http://127.0.0.1:8080>, escribe un nombre y entra. Abre otra pestaña con otro
nombre para ver presencia y mensajes en el mismo canal.

## Variables del ejemplo

| Variable | Default | Descripción |
|---|---|---|
| `SOCKET_CORE_URL` | — (**obligatoria**) | Hub visto desde el **backend** (interno). |
| `SOCKET_PUBLIC_URL` | — (**obligatoria**) | Hub que recibe el **navegador** (puede diferir). |
| `SOCKET_API_KEY` | — (**obligatoria**) | API key del proyecto (solo backend; scopes `publish,subscribe`). |
| `SOCKET_CHANNEL` | `demo:chat` | Canal del chat. |
| `PORT` | `8080` | Puerto del ejemplo. |

El backend comprueba con `canAccess(user, CHANNEL, CHANNEL)` (ver
`examples/backend/policy.mjs`) **antes** de pedir el JWT: en una app real ahí va tu
autorización (rol, tenant, membresía…). Los errores del hub no se reenvían al
navegador: solo `{"error":"token_unavailable"}` y el detalle queda en el log.

## Cómo funciona (resumen)

1. El navegador hace login (simulado) y recibe una cookie con un ID opaco.
2. `fetch("/socket-token")` → el backend pide a `/auth/token` con `X-Api-Key` y
   `user_id` obligatorio; devuelve **solo** `{ token, expires_in, channel, url }`.
3. El navegador abre el WS con el SDK (`?token=...`) y hace el handshake.
4. Publica con `sc.publish(...)`; el servidor sobrescribe `source` con
   `proyecto:sub`. Los targets son IDs de usuario.

> Recuerda: `domains` solo protege el uso desde navegador y solo al emitir el JWT;
> los frontends no deben confiar en `source` para seguridad.
