# Seguridad — configuración manual en GitHub y operación

Documentación relacionada: [`docs/COMO-FUNCIONA.md`](docs/COMO-FUNCIONA.md),
[`docs/DESPLIEGUE.md`](docs/DESPLIEGUE.md) y [`docs/CODIGOS.md`](docs/CODIGOS.md).

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
  `sudo chown -R 10001:10001 ./data` (el deploy ya lo hace). El entrypoint
  (`docker-entrypoint.sh`) aborta con un mensaje claro si el volumen no es
  escribible, antes de intentar abrir SQLite.
- **Backups**: el deploy guarda `backups/data-AAAAMMDD-HHMMSS.tgz` antes de
  reconstruir (retención 10). Cópialos también fuera del servidor.
- `ADMIN_PORT` no se publica; el acceso es solo por `docker exec` (ver §4).
- `TRUSTED_PROXIES` debe contener el **gateway de `central_network`** (o la IP del
  contenedor de Nginx si está en esa red). El arranque **rechaza loopback**
  (`127.0.0.0/8`, `::1`) y `0.0.0.0/0`/`::/0`. Averígualo en §5.
- Nginx sobrescribe `X-Forwarded-For` con `$remote_addr`; no lo cambies a
  `$proxy_add_x_forwarded_for`.

## 4. Acceso a /admin (vía elegida)

El listener admin escucha en `127.0.0.1:ADMIN_PORT` **dentro del contenedor** y no
se publica. La vía soportada es `docker exec`:

```bash
docker exec -i socket-core curl -sS \
  -H "X-Master-Key: $(grep ^MASTER_KEY= .env | cut -d= -f2)" \
  http://127.0.0.1:3015/admin/stats
```

- `scripts/alta_cliente.py` y `scripts/estado.py` intentan el acceso directo a
  `--admin` y caen automáticamente a `docker exec`.
- El `HEALTHCHECK` comprueba `/health` en 3005 **y** en 3015 dentro del contenedor.
- Nunca publiques `ADMIN_PORT` en compose ni lo expongas por Nginx.

## 5. TRUSTED_PROXIES: cómo obtener la IP correcta

El contenedor ve la conexión desde el **gateway de `central_network`** (o desde la
IP del contenedor de Nginx si Nginx está en esa red), no desde la IP del cliente.

```bash
# Gateway de la red Docker
docker network inspect central_network --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'

# Si Nginx es un contenedor en central_network, su IP:
docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' <contenedor-nginx>
```

Añade esa IP/CIDR a `TRUSTED_PROXIES` en `.env` y reinicia el servicio.
`scripts/estado.py` compara el gateway y las IPs de `central_network` con lo
configurado y avisa si no coincide. Sin lista, `X-Forwarded-For` se ignora.

## 6. CI/CD (verificación)

El workflow `.github/workflows/ci.yml` ejecuta en PR y push:
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test
--all-targets` y `cargo audit`. El job `deploy` usa `needs: [quality]` y solo se
ejecuta en push a `main`: **no despliega si CI falla**. Las actions están fijadas
por versión exacta (`checkout@v4.2.2`, `rust-toolchain@1.97.0`,
`rust-cache@v2.7.8`, `ssh-action@v1.2.0`).

## 7. Backup y rollback del despliegue

Antes de reconstruir, el deploy guarda el commit anterior en `.last_deploy_commit`
y un backup fechado del volumen en `backups/data-AAAAMMDD-HHMMSS.tgz`.

**Rollback documentado:**

```bash
cd /srv/infra/socket-core

# 1) Volver al commit que funcionaba
PREV=$(cat .last_deploy_commit)
git fetch origin
git reset --hard "$PREV"

# 2) Restaurar los datos de ese momento (elige el backup anterior al fallo)
sudo tar xzf backups/data-20260101-120000.tgz -C /srv/infra/socket-core
sudo chown -R 10001:10001 data

# 3) Reconstruir y levantar
docker compose build socket-core
docker compose up -d --remove-orphans
curl -f http://127.0.0.1:3005/health
```

## 8. Reporte de vulnerabilidades

Si encuentras un fallo de seguridad, no abras un issue público: escribe a la
persona responsable del repositorio por un canal privado y adjunta reproducción,
impacto y versión/commit afectado.
