# Informe final de hardening — socket-core

Fecha de cierre: 2026-10-26. Rama: `security-hardening`.
Línea base verificada: `ebf8a6d` (Fase 8.9).

Este informe consolida, **sin recortes**, los hallazgos de los bloques 4.1, 6 y
8.x y su estado. Los IDs internos (`M`, `P`, `Q`, `R`, `S`, `U`, `V`, `W`)
coinciden con el `CHANGELOG.md`. Solo R10 queda abierto y está acotado en
`docs/PENDIENTE.md`.

## 1. Resultado

- **Cerrados:** todos los hallazgos de 4.1, 6 y 8.2–8.9, más W1–W3 de 8.10.
- **Documentado, no implementado:** W4/R10 (rotación de API keys), acotado en
  `docs/PENDIENTE.md`.
- **Riesgos residuales aceptados:** `env_file` expone secretos a quien controle el
  daemon Docker; los paquetes `apt` de la imagen no están pinneados por versión;
  el daemon Docker no estaba disponible en el equipo local, por lo que las
  pruebas de Nginx/fail2ban se delegan a sus jobs de CI.

## 2. Hallazgos y estado

### 2.1 Bloque 4.1 — autorización y validación

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| 4.1.1 | `user_id` (`sub`) opcional o con caracteres reservados | Cerrado | `tests/identidad_ws.rs`, `tests/roles_jwt.rs` |
| 4.1.2 | Rol no exacto y scopes no revalidados en JWT vivos | Cerrado | `tests/roles_jwt.rs` |
| 4.1.3 | Prefijos de canal solapados, incluido `*` y keys revocadas | Cerrado | `tests/prefijos.rs` |
| 4.1.4 | Nombre de canal/descripción sin límites ni charset | Cerrado | `tests/security_regressions.rs` |
| 4.1.5 | Errores de DB filtrados y alta de canal no atómica | Cerrado | `tests/security_regressions.rs`, `src/channel_manager.rs` |

### 2.2 Bloque 6 — contenedor, despliegue y cadena de suministro

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| 6.1 | Contenedor como root, FS escribible, capabilities y admin publicado | Cerrado | `Dockerfile`, `docker-compose.yml`, `tests/docker_context.rs` |
| 6.2 | Deploy sin gate de CI ni rollback | Cerrado | `.github/workflows/ci.yml` (`needs`), `scripts/deploy.sh`, `scripts/test_deploy.py` |
| 6.3 | Volumen/entrypoint sin verificación de permisos | Cerrado | `docker-entrypoint.sh`, `tests/dockerfile_healthcheck.rs` |
| 6.4 | Healthcheck solo del puerto público | Cerrado | `tests/dockerfile_healthcheck.rs`, `docker-compose.yml` |
| 6.5 | `TRUSTED_PROXIES` sin documentar ni validar contra la red real | Cerrado | `tests/red_fase5.rs`, `scripts/estado.py`, `docs/DESPLIEGUE.md` |
| 6.6 | Actions sin SHA, auditoría y digests de imagen | Cerrado | `scripts/verify_action_shas.py`, `cargo audit`, `Dockerfile`, `.github/dependabot.yml` |

### 2.3 Bloque 8.2 — endurecimiento final (M1–M9)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| M1 | Sin `ConnectInfo` se asumía loopback | Cerrado | `tests/connect_info.rs` |
| M2 | `rate_limit_per_min` sin rango validado | Cerrado | `tests/rate_limit_validation.rs` |
| M3 | Auditoría de auth/rate sin IP | Cerrado | `tests/audit_ip.rs` |
| M4 | XFF de proxies no confiables y IPv4 mapeada | Cerrado | `tests/red_fase5.rs`, `tests/rate_limit_xff.rs` |
| M5 | Tests JWT sin control positivo | Cerrado | `tests/jwt_hardening.rs` |
| M6 | CI sin jobs de SDK/Python | Cerrado | `.github/workflows/ci.yml` |
| M7 | Renovación sin jitter | Cerrado | `sdk/js/test/sdk.test.mjs` |
| M8 | Backend de ejemplo sin prueba de ida y vuelta | Cerrado | `examples/backend/smoke.mjs` |
| M9 | Dimensionado de rate limits sin documentar | Cerrado | `docs/COMO-FUNCIONA.md` §6.1 |

### 2.4 Bloque 8.4 — administración de keys (P1–P8)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| P1 | Borrar key dejaba canales y WS huérfanos | Cerrado | `tests/ciclo_vida.rs` |
| P2 | Nombre duplicado devolvía error interno | Cerrado | `tests/key_admin_hardening.rs` |
| P3 | `expires_in_days` sin rango ni desbordamiento controlado | Cerrado | `tests/key_admin_hardening.rs` |
| P4 | Scopes/prefijos/domains aceptaban valores inválidos | Cerrado | `tests/key_admin_hardening.rs`, `tests/prefijos.rs` |
| P5 | Log incorrecto en `update()` | Cerrado | `src/keys.rs` |
| P6 | Altas concurrentes con el mismo prefijo | Cerrado | `tests/key_admin_hardening.rs` |
| P7 | Test de binding admin omitible sin control | Cerrado | `tests/admin_bind_runtime.rs` |
| P8 | Efectos de borrado/renombrado sin documentar | Cerrado | `docs/COMO-FUNCIONA.md`, `docs/DESPLIEGUE.md` |

### 2.5 Bloque 8.5 — empaquetado y preflight (Q1–Q9)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| Q1 | Apagado sin gracia acotada | Cerrado | `docker-compose.yml`, `tests/arranque_estricto.rs` |
| Q2 | Contexto Docker incluía secretos y artefactos | Cerrado | `.dockerignore`, `.gitattributes`, `tests/docker_context.rs` |
| Q3 | Builds sin `--locked` | Cerrado | `Dockerfile`, `.github/workflows/ci.yml` |
| Q4 | Imágenes sin digest y dependencias sin actualización | Cerrado | `Dockerfile`, `.github/dependabot.yml` |
| Q5 | Secretos de `env_file` sin documentar | Cerrado | `docs/DESPLIEGUE.md` §1.1, `SECURITY.md` |
| Q6 | Sin preflight de DB/keys/canales | Cerrado | `scripts/preflight.py`, `scripts/test_preflight.py` |
| Q7 | Keys inválidas al arrancar sin aviso | Cerrado | `tests/startup_warnings.rs` |
| Q8 | SQLite BUSY sin `busy_timeout` ni 503 | Cerrado | `src/db.rs`, `src/handlers.rs` |
| Q9 | Fila P4 repetida en el informe anterior | Cerrado | Este informe (sin duplicados) |

### 2.6 Bloque 8.6 — despliegue y suministro (R1–R9)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| R1 | Deploy embebido en YAML, no reproducible | Cerrado | `scripts/deploy.sh` |
| R2 | Fallo de build/salud sin rollback | Cerrado | `scripts/test_deploy.py` |
| R3 | `.last_good_commit` se escribía antes de verificar | Cerrado | `scripts/deploy.sh`, `scripts/test_deploy.py` |
| R4 | Backup SQLite sin consistencia ni integridad | Cerrado | `scripts/sqlite_backup.py`, `scripts/test_sqlite_backup.py` |
| R5 | `.env` copiado a `/tmp` durante el deploy | Cerrado | `scripts/deploy.sh`, `scripts/test_deploy.py` |
| R6 | Workflows sin concurrencia ni entorno protegido | Cerrado | `.github/workflows/ci.yml` |
| R7 | Sin prueba real de la imagen en CI | Cerrado | Job `docker` de CI |
| R8 | Política de reinicio no probada | Cerrado | `docker-compose.yml`, job `docker` |
| R9 | `TRUSTED_PROXIES` no comparada con la red real | Cerrado | `scripts/estado.py`, `scripts/preflight.py` |

### 2.7 Bloque 8.7 — Nginx y fail2ban (S1–S8)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| S1 | `limit_conn` devolvía 503 y sin límite WS por IP | Cerrado | `nginx/ws.seventwo.tech.https.conf`, `scripts/test_nginx.py` |
| S2 | Superficie pública y `/admin/` más amplias de lo necesario | Cerrado | `nginx/`, `scripts/test_nginx.py`, `docs/DESPLIEGUE.md` §4 |
| S3 | Config Nginx sin prueba real | Cerrado | `scripts/test_nginx.py`, job `nginx` |
| S4 | Filtro fail2ban sin validar | Cerrado | `scripts/test_fail2ban.sh`, job `fail2ban` |
| S5 | `on-failure` no probado ante reinicios del daemon | Cerrado | `docker-compose.yml` (`unless-stopped`), docs |
| S6 | Deploy podía ejecutar un script de origin obsoleto | Cerrado | `scripts/deploy.sh`, `scripts/test_deploy.py` |
| S7 | Push a `main` ejecuta código en el VPS sin aviso | Cerrado | `SECURITY.md` §9, `docs/sudoers-deploy.example` |
| S8 | Actions fijadas sin verificación automática | Cerrado | `scripts/verify_action_shas.py`, job `quality` |

### 2.8 Bloque 8.8 — secretos y alta segura (U1–U6)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| U1 | MASTER_KEY en argv de `curl` | Cerrado | `scripts/estado.py`, `scripts/alta_cliente.py`, `scripts/test_alta_cliente.py` |
| U2 | `estado.py` leía `.master_key` | Cerrado | `scripts/estado.py`, `docs/DESPLIEGUE.md` |
| U3 | Credenciales impresas o dentro del repo | Cerrado | `scripts/alta_cliente.py`, `scripts/test_alta_cliente.py` |
| U4 | Tests Python sin cobertura por job | Cerrado | `.github/workflows/ci.yml`, §3 de este informe |
| U5 | Filtro fail2ban final sin validación | Cerrado | `scripts/test_fail2ban.sh` |
| U6 | Delegación de Docker por sudoers sin análisis | Cerrado | `docs/sudoers-deploy.example`, `SECURITY.md` |

### 2.9 Bloque 8.9 — despliegue de rama y smoke (V1–V5)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| V1 | §0.2 usaba una imagen/preflight que no correspondían a Compose | Cerrado | `docs/DESPLIEGUE.md`, `scripts/preflight.py --db-only` |
| V2 | No se podía desplegar una rama/SHA sin tocar `main` | Cerrado | `scripts/deploy.sh` (`DEPLOY_REF`), `scripts/test_deploy.py` |
| V3 | `git fetch` no era la primera acción con efecto | Cerrado | `scripts/deploy.sh`, `scripts/test_deploy.py` |
| V4 | Smoke manual y credenciales visibles | Cerrado | `scripts/smoke.py`, `scripts/test_smoke.py` |
| V5 | `estado.py` asumía un dominio fijo | Cerrado | `scripts/estado.py`, `scripts/test_estado.py` |

### 2.10 Bloque 8.10 — cierre (W1–W4, R10)

| ID | Hallazgo | Estado | Evidencia |
|---|---|---|---|
| W1 | Informe final incompleto o recortado | Cerrado | Este documento |
| W2 | `IP_RATE_PER_MIN` fijo en 600 y sin validación | Cerrado | `src/config.rs`, `src/lib.rs`, `src/handlers.rs`, `tests/ip_rate_config.rs` |
| W3 | 42 tests Python sin desglose por fichero/job | Cerrado | §3 de este informe |
| W4/R10 | Rotación de API keys no definida | Documentado, no implementado | `docs/PENDIENTE.md` |

## 3. Tests y CI (W3)

Los **58 tests de Python** de `unittest discover` se reparten así:

| Fichero | Tests | Job de CI |
|---|---:|---|
| `scripts/test_alta_cliente.py` | 13 | `python` |
| `scripts/test_deploy.py` | 19 | `python` |
| `scripts/test_preflight.py` | 12 | `python` |
| `scripts/test_smoke.py` | 3 | `python` |
| `scripts/test_sqlite_backup.py` | 9 | `python` |
| `scripts/test_estado.py` | 2 | `python` |
| **Total** | **58** | **`python`** |

Pruebas que no son casos `unittest` y se ejecutan en su propio job:

| Comprobación | Job de CI | Resultado esperado |
|---|---|---|
| `scripts/test_docs_curl.py` | `quality` | 8 bloques ejecutables |
| `scripts/test_nginx.py` | `nginx` | Nginx real en Docker |
| `scripts/test_fail2ban.sh` | `fail2ban` | 2 coincidencias 401/403 |
| `scripts/verify_action_shas.py` | `quality` | Todos los SHA coinciden |
| `sdk/js/test/sdk.test.mjs` | `sdk` | 22 pasan, 1 skip (`node:sqlite`) |

`test_docs_curl.py` y `test_nginx.py` también son importados por
`unittest discover` (0 casos); su ejecución real es la de la tabla anterior.

## 4. Verificación local (2026-10-26)

| Comprobación | Resultado |
|---|---|
| `cargo fmt --check` | OK |
| `cargo clippy --locked --all-targets -- -D warnings` | OK |
| `cargo test --locked --all-targets` | OK |
| `cargo test -- --list` | 162 tests (base `ebf8a6d`: 157; desaparecidos: 0; nuevos: 5) |
| `python -m unittest discover -s scripts -p "test_*.py"` | 42/42 OK |
| `node --test sdk/js/test/sdk.test.mjs` | 22 OK, 1 skip |
| `python scripts/test_docs_curl.py` | 8/8 bloques OK |
| `cargo audit` | 0 vulnerabilidades; 1 aviso permitido (`tokio` unsound) |
| `scripts/test_nginx.py` | SKIP local: Docker no disponible (CI lo exige) |
| `scripts/test_fail2ban.sh` | No ejecutado local: `fail2ban-regex` ausente (CI lo exige) |

## 5. Pendiente

- **R10**: `POST /admin/keys/:id/rotate` + `alta_cliente.py --rotar`. Alcance,
  garantías transaccionales y tests exigidos en `docs/PENDIENTE.md`. No se
  implementó en 8.10.
