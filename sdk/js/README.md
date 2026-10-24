# SDK de socket-core (JavaScript)

Un solo fichero (`socket-core.mjs`), **sin dependencias**, para navegador y
**Node.js ≥ 22** (usa el `WebSocket` global; no incluye cliente propio). Tu
aplicación solo necesita un endpoint que devuelva un JWT; el SDK se encarga de
conexión, handshake, renovación y reconexión.

## Seguridad (leer)

- El SDK **nunca** recibe ni maneja la **API key**: se queda en el backend del
  proyecto. El SDK solo usa el **JWT** de corta vida.
- El JWT viaja en la query (`?token=...`) porque el navegador no permite cabeceras
  en WebSocket. Nginx está configurado para **no registrar query strings** en el
  access log. El JWT dura ~10 minutos y el SDK lo renueva antes de que expire.
- `sub` debe ser un identificador **opaco** (no emails): se muestra al resto del
  canal en la presencia.

## Uso

```js
import { connect } from "./socket-core.mjs";

const sc = connect({
  url: "wss://hub.ejemplo.com",     // o https://host (se convierte); default /ws
  channel: "demo:chat",
  device: "web",                     // opcional (se sanea)
  tokenProvider: async () => {       // LO IMPLEMENTA TU BACKEND
    const r = await fetch("/socket-token", { method: "POST" });
    const j = await r.json();
    return { token: j.token, expires_in: j.expires_in }; // expires_in en segundos
  },
});

sc.on("status", (s) => console.log("estado:", s));
sc.on("message", (ev) => console.log(ev.source, ev.payload));
sc.on("presence", (p) => console.log(p.type, p.user, p.total_clients));
sc.on("missed_events", (m) => console.log("eventos perdidos:", m.missed));
sc.on("fatal", (f) => console.error("cierre definitivo:", f.code, f.detail));

await sc.publish({ type: "chat", text: "hola" });            // a todo el canal
await sc.publish({ type: "chat", text: "privado" }, { targets: ["user-1"] });
sc.close();
```

`tokenProvider` puede devolver también `role` (`"publish"|"subscribe"|"both"`) si
tu backend lo conoce; así `publish()` rechaza localmente sin ir al servidor.

## API

| Miembro | Descripción |
|---|---|
| `connect({ tokenProvider, channel, device?, url, renewBeforeMs?, role? })` | Abre la conexión gestionada. |
| `on(event, fn)` / `off(event, fn)` | Eventos: `message`, `presence`, `missed_events`, `fatal`, `status`. |
| `publish(payload, { targets? })` | Publica por WS. Promesa: rechaza si no está conectado, si el payload serializado supera **64 KB**, o si el rol conocido no permite publicar. |
| `close()` | Cierre intencional (no reconecta). |
| `status` | Estado actual: `idle`, `connecting`, `connected`, `renewing`, `reconnecting`, `fatal`, `closed`. |

## Reconexión y renovación

- **Backoff exponencial con jitter** (500 ms → 30 s) para evitar avalanchas tras
  `server_shutdown` (Nginx limita `/auth/token` a 5 req/s por IP).
- **Renovación sin cortes**: ~60 s antes de `exp` abre una conexión nueva, espera
  su handshake, y solo entonces cierra la vieja. Mientras se solapan, los eventos
  se **deduplican por `id`**.
- Estrategia por código del servidor:

| Código | Acción del SDK |
|---|---|
| `token_expired`, `server_shutdown`, `idle_timeout`, `handshake_timeout`, cierres de red | Reconectar (pide token nuevo). |
| `rate_limited` | Espera mayor (≥10 s) + jitter antes de reconectar. |
| `forbidden`, `credential_revoked`, `token_revoked`, `channel_deleted`, `not_found`, `invalid_payload`, `message_too_large` | **No reconectar**: emite `fatal` con el código. |

Detalle de todos los códigos: [`docs/CODIGOS.md`](../../docs/CODIGOS.md).

## Node ≥ 22

Usa el `WebSocket` nativo del navegador o de Node ≥ 22. En Node 18–20 (sin
`WebSocket` global) el SDK lanza un error claro; puedes aportar un polyfill global
si lo necesitas (no se incluye cliente WS propio para evitar bugs de protocolo).
Si el `tokenProvider` lanza un error con `status` 401/403 (o el mensaje contiene
401/403), el SDK emite `fatal` y **no reintenta**; los fallos de red o 5xx usan
backoff con jitter. Además serializa/espacia las llamadas a `tokenProvider`
(≥220 ms) para respetar el límite de `/auth/token` (5 req/s por IP).

El deduplicador guarda solo los últimos **1000 ids**. `close()` limpia timers,
sockets y renovaciones en curso.

## Tests del SDK

Contra el hub real (se compila el binario y se levantan servidores temporales):

```bash
cargo build
node --test --test-force-exit sdk/js/test/sdk.test.mjs
```

Cubren: conexión, presencia, publicar/recibir, clasificación de códigos, no
reconexión tras `credential_revoked`, reconexión tras caída del servidor y
deduplicación durante la renovación solapada.

> Los tests usan el runner de Node y `--test-force-exit` (Node ≥ 22, igual que el runtime del SDK).
