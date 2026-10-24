# Códigos estables de error — socket-core

Contrato para clientes y SDK. Los códigos **no cambian** sin una nota en
`CHANGELOG.md`. Los mensajes `detail` son informativos y pueden variar.

## WebSocket (`/ws`)

Los errores del WS llegan como un frame de texto con esta forma y, salvo indicación,
la conexión se cierra después:

```json
{ "type": "error", "code": "<código>", "detail": "<texto informativo>" }
```

| Código | Causa | Acción recomendada del cliente |
|---|---|---|
| `token_expired` | El JWT expiró durante la conexión (TTL típico 10 min). | **Reconectar** pidiendo un JWT nuevo al backend. El SDK lo hace solo y además renueva antes de expirar. |
| `server_error` | Fallo interno temporal (p. ej. DB no disponible al revalidar la credencial). NO significa que la key esté revocada. | **Reconectar** con backoff+jitter (el SDK lo hace); si persiste, revisar la DB/logs. |
| `credential_revoked` | La API key del proyecto fue revocada, borrada o editada (prefijo/scopes). | **No reconectar.** Es un cierre definitivo (`fatal`): avisar/cerrar sesión. |
| `token_revoked` | El `jti` de este JWT fue revocado por el admin. | **No reconectar** con el mismo token; pedir uno nuevo solo si el backend lo autoriza. |
| `forbidden` | Sin permiso: rol insuficiente para la acción, canal no cubierto por el claim, o publicación denegada. | **No reconectar** en bucle. Revisar rol/canal; en `publish`, el rol debe incluir `publish`. |
| `not_found` | El canal no existe o no pertenece al proyecto. | **No reconectar.** Verificar nombre de canal. |
| `channel_deleted` | El canal se borró mientras la conexión estaba abierta. | **No reconectar** a ese canal; elegir otro o crearlo de nuevo. |
| `handshake_timeout` | No se envió el handshake `{ "channel": ... }` en 8 s. | Reconectar y enviar el handshake **inmediatamente** tras abrir. |
| `invalid_payload` | JSON inválido, falta `channel`, targets inválidos, frame binario, tipo de evento reservado o payload excesivo/improfundo. | Corregir el cliente. No reintentar igual (posible `fatal`). |
| `message_too_large` | Frame/payload supera 64 KB. | Reducir el payload. No reintentar igual. |
| `rate_limited` | Demasiadas publicaciones por conexión (50/s) o por proyecto (500/s). | Es transitorio: esperar más (el SDK espera ≥10 s con jitter) y reintentar. |
| `idle_timeout` | Sin actividad entrante durante 120 s (ping/pong). | Reconectar (red lenta o cliente suspendido). |
| `server_shutdown` | Apagado ordenado del servidor (SIGTERM/SIGINT). | Reconectar con **backoff + jitter** (evita avalancha al volver el servicio). |
| `missed_events` | No es error: el cliente fue lento y el buffer interno perdió eventos. Llega como `{ "type": "missed_events", "missed": N }` sin cerrar. | Re-sincronizar el estado (p. ej. volver a pedir snapshot por REST). |

Convenciones adicionales:

- Al abrir el WS, el cliente envía `{ "channel": "<id o nombre>", "device": "<opcional>" }`.
- Los eventos normales son objetos `{ id, channel, source, targets, payload, timestamp }`.
  El SDK deduplica por `id` durante la renovación de token.
- La presencia (`client_joined` / `client_left`) solo trae `{ user, connection_id,
  total_clients }`; la lista completa se consulta por `GET /channels/:id/clients`.
- El token viaja en `?token=` (limitación del navegador). Nginx **no registra query
  strings** en el access log; el JWT dura ~10 min.

## HTTP (REST)

| Código | Causa | Acción recomendada |
|---|---|---|
| `200` | OK. | — |
| `201` | Canal/key creado. | Guardar la key (solo se muestra una vez). |
| `400` | Petición inválida: validación (nombre/sub/description), prefijo solapado, rol inválido, payload fuera de límites. | Corregir la petición; el `error` indica el motivo sin detalles internos. |
| `401` | Credencial ausente/ inválida / revocada / expirada (API key o JWT), o master incorrecta en admin. | Renovar credencial. No reintentar con la misma. |
| `403` | Credencial válida sin permiso: origen (`domains`) no permitido, scope insuficiente, canal fuera de prefijo. | Revisar scopes/dominios/prefijo. |
| `404` | Recurso inexistente **o de otro proyecto** (no se filtra existencia). | Verificar id/nombre; no asumir que no existe si es de otro proyecto. |
| `429` | Rate limit por IP, por key, bloqueo temporal de admin o exceso de conexiones WS. Incluye `Retry-After`. | Esperar `Retry-After` y aplicar backoff con jitter. |
| `500` | Error interno (incluye fallo de DB al verificar credenciales/revocación). Nunca incluye detalles internos. | Reintentar más tarde; si persiste, revisar logs del servidor. |

### Notas de seguridad

- `domains` solo protege el uso desde navegador y **solo** al emitir el JWT; no se
  comprueba en el WebSocket.
- En REST, `source` lo fija el backend; los frontends **no** deben usarlo para
  decisiones de seguridad.
- `sub` (`user_id`) debe ser un identificador opaco (no un email): se muestra al
  resto del canal en presencia y eventos de sistema.
