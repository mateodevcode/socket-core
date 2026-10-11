# socket-core definitivo — hub seguro multi-proyecto

Servicio único de transporte (REST + WebSocket) para todos los proyectos presentes y futuros.
Seguridad: **API Key por proyecto (dominios + prefijo + scopes + revocable en SQLite) + JWT corto (5-15 min) emitido por el propio core**. Sin Tailscale.

## Despliegue

```bash
cp .env.example .env
# editar JWT_SECRET (openssl rand -hex 64) y MASTER_KEY (openssl rand -hex 32)
# en prod: ALLOW_BOOTSTRAP=false, ALLOWED_ORIGINS=https://tu-dominio.com

# local
cargo run

# docker (red externa compartida con tus otros proyectos)
docker network create central_network  # solo una vez
docker compose up -d --build
curl http://127.0.0.1:3005/health
```

## Alta de un proyecto (otorgar acceso)

```bash
# 1. crear key ligada a dominios + prefijo de canales
curl -X POST http://127.0.0.1:3005/admin/keys \
  -H "X-Master-Key: $MASTER_KEY" -H "Content-Type: application/json" -d '{
    "name": "monitoreo",
    "domains": ["https://panel.midominio.com"],
    "channel_prefix": "monitoreo:*",
    "scopes": ["publish","subscribe","manage"],
    "rate_limit_per_min": 120
  }'
# -> devuelve {"api_key":"sc_live_..."} (mostrar UNA vez, guardar en backend del proyecto)

# 2. revocar / borrar
curl -X POST http://127.0.0.1:3005/admin/keys/<id>/revoke -H "X-Master-Key: $MASTER_KEY"
curl -X DELETE http://127.0.0.1:3005/admin/keys/<id> -H "X-Master-Key: $MASTER_KEY"

# 3. revocar un JWT concreto
curl -X POST "http://127.0.0.1:3005/admin/tokens/revoke?jti=<jti>" \
  -H "X-Master-Key: $MASTER_KEY" -H "Content-Type: application/json" -d '{}'
```

## Uso por proyecto

```bash
# Backend: pide JWT corto con su API key (valida Origin vs domains de la key)
curl -X POST http://127.0.0.1:3005/auth/token \
  -H "X-Api-Key: sc_live_..." -H "Origin: https://panel.midominio.com" \
  -H "Content-Type: application/json" -d '{"user_id":"user-1","channel":"monitoreo:alertas","role":"both"}'
# -> {"token":"eyJ...","expires_in":600}

# Backend emite (X-Api-Key con scope publish)
curl -X POST http://127.0.0.1:3005/channels/<id>/events \
  -H "X-Api-Key: sc_live_..." -H "Content-Type: application/json" -d '{
    "source":"admin-agent","targets":["*"],"payload":{"msg":"hola"}
  }'

# Frontend WS (solo JWT, el core corta la conexión al expirar -> reconectar y pedir JWT nuevo)
# ws://host:3005/ws?token=eyJ...
# handshake: {"channel":"<id-o-nombre>","client_id":"front-1"}
```

## Endpoints

| Método | Ruta | Auth |
|---|---|---|
| GET | /health | pública |
| POST | /auth/token | X-Api-Key + Origin permitido |
| GET | /stats, /channels, /channels/:id, /channels/:id/clients | X-Api-Key |
| POST | /channels | X-Api-Key scope `manage` + canal en prefijo |
| POST | /channels/:id/events | X-Api-Key `publish` o JWT `publish` |
| GET | /ws?token=JWT | JWT (`subscribe`) o service key `sc_live_*` |
| GET/POST/DELETE | /admin/keys... | X-Master-Key |
| POST | /admin/tokens/revoke | X-Master-Key |

## Hardening incluido

- CORS estricto por `ALLOWED_ORIGINS` global + chequeo `Origin/Referer` por key (`keys.rs:origin_allowed`).
- Aislamiento por `channel_prefix` (`monitoreo:*` no ve `jarvis:*`).
- Rate-limit por key (configurable) + global por IP 600/min (`middleware.rs`).
- Body 1MB, headers `nosniff` + `DENY frame`, `/stats` ya no pública, JWT con `jti` revocable y corte de WS al expirar.
- Bind `127.0.0.1:3005` en compose (TLS en Nginx por delante).
