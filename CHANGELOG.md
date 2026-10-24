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

### Nota de estado

- La comprobación de revocación de jti ante fallo de DB sigue siendo fail-open;
  el modo fail-closed y el corte activo de conexiones llegan con la fase de
  ciclo de vida (4).
- La suite de integración en `tests/` define el comportamiento objetivo: los
  tests aún en rojo corresponden a fases 3-5 (identidad, límites, secretos).

#### Mantener en mente (no incompatible)

- `Cargo.toml` ya tiene el nombre canónico (antes `cargo.toml` minúsculas al repo).
- Dependencias depuradas: fuera `tokio-tungstenite` y `url` de producción;
  `dotenv` → `dotenvy`; sqlx sin features por defecto (sqlite/runtime-tokio/macros).
- Suite de integración en `tests/` ahora referencia el comportamiento objetivo.
