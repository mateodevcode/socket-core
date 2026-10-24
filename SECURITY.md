# Seguridad — configuración manual en GitHub y operación

Este archivo lista lo que **no se puede versionar** (ajustes de la cuenta/repo en
GitHub) y los pasos manuales de seguridad que debes completar una vez.

## 1. Repositorio en GitHub (manual, una vez)

1. **Repositorio privado** (Settings → General → Danger Zone → Change visibility).
   El código de infraestructura no debe ser público.
2. **2FA obligatorio** en tu cuenta y en la organización (Settings → Password and
   authentication → Two-factor authentication; org → Authentication security →
   Require two-factor authentication).
3. **Branch protection en `main`** (Settings → Branches → Add branch ruleset):
   - Require a pull request before merging (1 aprobación; dismiss stale approvals).
   - Require status checks to pass: selecciona el check **CI / Calidad
     (fmt, clippy, test, audit)** (`quality`).
   - Require branches to be up to date before merging.
   - Do not allow bypassing the above settings (incluido admins).
   - Block force pushes y deletions de `main`.
4. **Actions permissions** (Settings → Actions → General):
   - "Allow enterprise/selected actions".
   - Workflow permissions: **Read repository contents** (no write salvo lo necesario).
   - "Require approval for first-time contributors" / fork PRs.
5. **Secret scanning + Push protection** (Settings → Code security):
   activa *Secret scanning* y *Push protection*.
6. **Dependabot alerts** y *Dependabot security updates* activados.
7. **Environments** (opcional recomendado): crea `production` con required
   reviewers y limita los secrets `VPS_*` a ese environment.
8. Verifica que el workflow **Deploy** solo corre tras CI en verde
   (`workflow_run` sobre `main`); un CI rojo no despliega.

## 2. Secretos y credenciales (manual)

- `JWT_SECRET`: aleatorio, ≥ 32 caracteres (`openssl rand -hex 64`). Rotarlo solo
  invalida **JWT** (los clientes piden uno nuevo); **no** invalida API keys.
- `MASTER_KEY`: aleatorio, ≥ 32 caracteres (`openssl rand -hex 32`). Vive solo en
  el `.env` del servidor; nunca en frontends ni logs.
- **Nunca** subas `.env`, `.env.production`, `.master_key`, `.ops_key` ni `data/`.
- Rotación de una API key de proyecto: crear key nueva → desplegar en el proyecto →
  revocar la vieja por el listener admin (`127.0.0.1:ADMIN_PORT`, `docker exec`).
- Bootstrap (`ALLOW_BOOTSTRAP=true`) escribe la key en `data/bootstrap-key.txt`
  con permisos `0600`: muévela a tu gestor de secretos y borra el fichero.

## 3. Host / contenedor (manual)

- El contenedor corre como **UID/GID 10001** con FS de solo lectura. El volumen
  `./data` debe ser escribible por ese UID:
  `sudo chown -R 10001:10001 ./data` (el deploy ya lo hace).
- **Backups**: copia `data/socket-core.db` (y `-wal`/`-shm` si existen) antes de
  cada despliegue; guarda fuera del servidor.
- `ADMIN_PORT` no se publica; el acceso es solo por `docker exec`/SSH local.
- `TRUSTED_PROXIES` debe contener únicamente la IP del Nginx local
  (`127.0.0.1` si Nginx y el hub comparten host/namespace). No confíes en
  cabeceras de clientes directos.
- Nginx sobrescribe `X-Forwarded-For` con `$remote_addr`; no lo cambies a
  `$proxy_add_x_forwarded_for`.

## 4. CI/CD (verificación)

El workflow `.github/workflows/ci.yml` ejecuta en PR y push:
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test
--all-targets` y `cargo audit`. El deploy depende de que CI concluya con éxito.

## 5. Reporte de vulnerabilidades

Si encuentras un fallo de seguridad, no abras un issue público: escribe a la
persona responsable del repositorio por un canal privado y adjunta reproducción,
impacto y versión/commit afectado.
