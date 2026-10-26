# Despliegue seguro de socket-core

Checklist operativo para desplegar este repo. Todos los comandos están pensados
para ejecutarse en el servidor donde corre el contenedor (Nginx en el host).

## 0. Antes de desplegar (haz esto una vez por versión)

### 0.1 Revisa las variables nuevas del `.env`

Respecto a versiones anteriores, estas variables son **nuevas o cambiaron de
comportamiento**. Valores de ejemplo (cámbialos por secretos reales):

```dotenv
# Listener admin (loopback del contenedor; NO publicar)
ADMIN_PORT=3015

# "prod" exige ALLOWED_ORIGINS; "dev" permite CORS permisivo
APP_ENV=prod

# Obligatorio en producción: dominios reales, sin "*"
ALLOWED_ORIGINS=https://panel.ejemplo.com,https://app.ejemplo.com

# Antes true por defecto; ahora false
ALLOW_BOOTSTRAP=false

# Gateway de central_network (o IP del contenedor de Nginx). Vacío = ignorar XFF
TRUSTED_PROXIES=172.18.0.1

# Gracia total de apagado ordenado
SHUTDOWN_GRACE_SECS=10

# Peticiones por minuto por IP (antes de validar la API key). Rango 1..100000.
IP_RATE_PER_MIN=600

# Deben cumplirse o el proceso NO arranca:
#   JWT_SECRET != valor por defecto y >= 32 chars; MASTER_KEY >= 32; JWT_TTL_SECS en [60,3600];
#   IP_RATE_PER_MIN en [1,100000]
JWT_SECRET=<openssl rand -hex 64>
MASTER_KEY=<openssl rand -hex 32>
JWT_TTL_SECS=600
```

Averigua el gateway real:

```bash
docker network inspect central_network --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
# Si Nginx es un contenedor de esa red:
docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' <contenedor-nginx>
```

### 0.2 Prueba la migración sobre una COPIA de la DB

La migración añade `project_id` a `channels` y asigna dueño por prefijo. Los
canales cuyo nombre no encaja con el prefijo de ninguna key quedan **huérfanos**
(`project_id=''`, invisibles: 404 para todos).

> **Aclaración:** la prueba ejecutada en este repo fue sobre una copia
> **SINTÉTICA** (el repositorio no contiene `data/socket-core.db`). Los canales
> `demo:chat`/`orphan:x` de la salida de ejemplo son de laboratorio, no datos reales.

Comandos EXACTOS para probar sobre una copia de TU DB real. **No uses
`socket-core:latest`**: la imagen la construye Compose con el nombre
`<proyecto>-<servicio>` (aquí `socket-core-socket-core`). Obtén el nombre real:

```bash
cd /srv/infra/socket-core
IMG="$(docker compose config --images | head -1)"
echo "imagen que usa compose: $IMG"
# alternativa tras un build/up: docker compose images socket-core --format '{{.Repository}}:{{.Tag}}'
```

Procedimiento completo (copia + migración + informe con Python del repo):

```bash
cd /srv/infra/socket-core
IMG="$(docker compose config --images | head -1)"
mkdir -p /tmp/migcheck
sudo cp data/socket-core.db /tmp/migcheck/hub.db
sudo chown -R 10001:10001 /tmp/migcheck   # el contenedor corre como UID 10001

docker run -d --name sc-mig-check -v /tmp/migcheck:/app/data   -e APP_ENV=dev -e PORT=3005 -e ADMIN_PORT=3015   -e JWT_SECRET="$(openssl rand -hex 64)" -e MASTER_KEY="$(openssl rand -hex 32)"   -e DATABASE_URL="sqlite:/app/data/hub.db?mode=rwc" -e ALLOW_BOOTSTRAP=false   "$IMG"
sleep 5
docker logs sc-mig-check 2>&1 | tail -8    # ver "Migración de canales: N ..."
docker stop sc-mig-check && docker rm sc-mig-check

# Equivalente a sqlite3, pero con el Python del repo: lee keys y canales de la
# copia y calcula huérfanos/cascada SIN admin ni MASTER_KEY.
python3 scripts/preflight.py --db /tmp/migcheck/hub.db --db-only
```

Salida real de este repo sobre una DB **sintética** con esquema antiguo
(`channels` sin `project_id`, una key `demo:*` y los canales `demo:chat` y
`orphan:x`). El daemon Docker no estaba disponible en el entorno de la prueba,
así que se ejecutó el **mismo binario** que empaqueta la imagen; el nombre de
imagen anterior sí se obtuvo con `docker compose config --images`
(`socket-core-socket-core`):

```text
INFO socket_core::db: Migración de canales: 2 repartidos por prefijo
INFO socket_core::db: SQLite listo (1 keys registradas)
INFO socket_core::channel_manager: Canales restaurados desde SQLite: 2

columnas channels: ['id', 'name', 'description', 'created_at', 'project_id']
canales: [('demo:chat', 'k-demo'), ('orphan:x', '')]
huérfanos (project_id=''): 1

$ python3 scripts/preflight.py --db /tmp/migcheck/hub.db --db-only
Keys leídas: 1 (vía db-only)
Canales leídos de copia SQLite: 2 (/tmp/migcheck/hub.db)

-- canales por proyecto --
  legacy                         canales=1

-- si borras cada key, canales en cascada --
  legacy                         se borrarían=1

[BLOQUEANTE] 2:
  - canales huérfanos (invisibles): ['orphan:x']
  - TRUSTED_PROXIES vacío: se ignorará X-Forwarded-For
exit 1
```

`--db-only` no necesita MASTER_KEY: lee `api_keys` de la copia. Si prefieres el
listener admin, usa `SOCKET_MASTER_KEY=... python3 scripts/preflight.py --db ...`.

Si hay huérfanos esperados, decide antes de desplegar: renombrar el canal, ajustar
el prefijo de una key, o aceptar que queden inaccesibles. NO borres filas a mano
sin backup.

**Asignar un canal huérfano a un proyecto (manual, sobre la copia primero):**

```sql
UPDATE channels
SET project_id = (SELECT id FROM api_keys WHERE name = 'proyecto-demo')
WHERE project_id = '' AND name = 'canal-huerfano';
```

Verifica en la copia con `python3 scripts/preflight.py --db /tmp/migcheck/hub.db --db-only`
(no necesitas `sqlite3`). Después, en el deploy real (con backup hecho):
aplica el UPDATE sobre `data/socket-core.db` con el servicio parado,
`sudo chown -R 10001:10001 data`, y arranca.

> **Tests de red:** `admin_no_escucha_en_otra_interfaz` requiere una interfaz
> no-loopback. Si el entorno no la tiene, el test **falla** para no dar falsos
> positivos; define `SKIP_NET_IFACE_TEST=1` para omitirlo explícitamente (p. ej. CI
> sin red externa).

### 0.3 Backup de la DB

```bash
cd /srv/infra/socket-core
mkdir -p backups
STAMP=$(date +%Y%m%d-%H%M%S)
# Backup consistente (API de sqlite + PRAGMA integrity_check), sin tar en caliente.
sudo python3 scripts/sqlite_backup.py backup   --db data/socket-core.db --out "backups/data-$STAMP.db"
echo "backup: backups/data-$STAMP.db"
```

El deploy automático hace este backup (retención de 10) y guarda el commit sano en
`.last_good_commit` **solo** tras salud + chequeo público.

### 0.4 Avisa a los clientes de los cambios incompatibles

Resumen (detalle y migración en `CHANGELOG.md`):

- Las **API keys ya no valen como token de WS ni como `Bearer`** (401). Cada
  proyecto debe: backend pide JWT (`/auth/token`) y navegador abre el WS con JWT.
- El **handshake** ya no usa `client_id`: solo `{ "channel": ..., "device"?: ... }`;
  la identidad es el `sub` del JWT.
- `user_id` es **obligatorio** al pedir JWT (ID opaco, sin `*`/`:`, ≤128).
- Los **eventos de sistema** ya no traen la lista completa (`clients`); usad
  `GET /channels/:id/clients`.
- `/admin/*` **sale del puerto público** (404): se accede por `docker exec`.
- Rate limit por IP más estricto y **no confía en XFF** sin `TRUSTED_PROXIES`.
- JWT: `iss`/`aud` obligatorios; tokens antiguos dejan de valer (TTL corto: espera
  o pide nuevos).

## 1. Despliegue

### 1.0 Primer despliegue de la RAMA (sin tocar `main`)

No fusiones a `main` todavía. Orden exacto:

1. En tu equipo, sube la rama y espera a que CI esté **verde**:
   ```bash
   git push -u origin security-hardening
   gh run watch   # o espera el check en GitHub Actions
   ```
2. En el VPS, trae **solo** la rama y ejecuta el script versionado de esa rama con
   `DEPLOY_REF` (no modifica `main`, ni local ni remoto):
   ```bash
   cd /srv/infra/socket-core
   git fetch origin security-hardening
   git show origin/security-hardening:scripts/deploy.sh | DEPLOY_REF=security-hardening bash -s
   ```
   Si no existe `.last_good_commit`, el script lo inicializa con el **HEAD previo**
   (normalmente `main`) antes de resetear el árbol. Así, si este primer intento
   falla en salud, el rollback vuelve a `main` y restaura la DB.
3. Comprueba `/health`, `SOCKET_PUBLIC_URL=... python3 scripts/estado.py` y
   `SOCKET_PUBLIC_URL=... SOCKET_MASTER_KEY=... python3 scripts/smoke.py`.
4. Solo si todo va bien, fusiona la rama a `main` para que el workflow automático
   (que ejecuta `origin/main:scripts/deploy.sh`) tome el relevo.

El despliegue automático de `main` (push → CI `quality`/`sdk`/`python`/`docker`/
`nginx`/`fail2ban` → job `deploy` con entorno `production`) no cambia.

El despliegue es automático: push a `main` → workflow **CI** (`quality`) → si pasa,
job **deploy** (needs de quality/sdk/python/docker/nginx/fail2ban) exige el entorno
**`production`** (con revisor) y ejecuta por SSH el script versionado
`scripts/deploy.sh` desde `origin/main`. El script:
- hace un **backup consistente** de SQLite (API backup + `integrity_check`),
- espera hasta 60 s a `/health` en el puerto público **y** el admin,
- si falla, hace **rollback** al commit de `.last_good_commit` (que **solo** se
  escribe tras salud + chequeo público), restaura el backup, reconstruye y
  reverifica; el job termina con **exit 1**,
- aplica Nginx con **backup y restauración** si `nginx -t`/reload fallan,
- ya no copia `.env` a `/tmp` (umask 077).
No se despliega si CI falla.

Manual (solo si es necesario):

```bash
cd /srv/infra/socket-core
git fetch origin main && git reset --hard origin/main
sudo chown -R 10001:10001 data    # el contenedor corre como UID 10001
docker compose build socket-core
docker compose up -d --remove-orphans
```

Notas del contenedor: `read_only: true`, `cap_drop: ALL`, `no-new-privileges`,
`tmpfs /tmp`. Solo escribe en `/app/data` (volumen) y `/tmp`.

### 1.1 Notas de operación del contenedor

- `SHUTDOWN_GRACE_SECS` debe ser **1–19** (el compose usa `stop_grace_period: 20s`);
  el arranque rechaza 0 o ≥20.
- `restart: unless-stopped`: el contenedor se reinicia tras caídas, pero no tras un
  `docker stop` manual. `on-failure:5` no se adoptó porque no se probó ante reinicios
  del daemon/VPS. Revisa `docker compose logs` si hay bucles.
- **`env_file` expone secretos** en `docker inspect socket-core` y
  `docker exec socket-core env`. Quien tenga acceso al socket Docker los ve (es
  equivalente a root). Mantén `.env` en 600 y restringe el acceso al daemon.
- **Preflight (solo lectura)**: antes de tocar keys/canales, ejecuta:

  ```bash
  SOCKET_MASTER_KEY=$(grep ^MASTER_KEY= .env | cut -d= -f2) python3 scripts/preflight.py
  # recomendado con copia de la DB para analizar canales/huérfanos/cascada:
  docker cp socket-core:/app/data/socket-core.db /tmp/preflight.db
  python3 scripts/preflight.py --db /tmp/preflight.db
  ```

  Informa scopes inválidos, prefijos solapados (incluidas revocadas), expiraciones,
  canales por proyecto, huérfanos y cuántos canales se borrarían al eliminar cada key.
  Exit != 0 si hay bloqueantes. **No modifica nada.**

## 2. Verificación posterior

```bash
cd /srv/infra/socket-core

# 1) Salud de ambos listeners dentro del contenedor
docker exec socket-core curl -fsS http://127.0.0.1:3005/health
docker exec socket-core curl -fsS http://127.0.0.1:3015/health

# 2) Estado completo (contenedor, stats por proyecto, Nginx, cert, fail2ban, TRUSTED_PROXIES)
#    Sin dominio por defecto: SOCKET_PUBLIC_URL (o --core) es obligatorio.
SOCKET_PUBLIC_URL=https://ws.seventwo.tech SOCKET_MASTER_KEY=$(grep ^MASTER_KEY= .env | cut -d= -f2) python3 scripts/estado.py

# 3) Prueba de humo post-despliegue: crea key temporal, canal, emite,
#    comprueba y borra la key en un finally (también si algo falla).
SOCKET_PUBLIC_URL=https://ws.seventwo.tech SOCKET_MASTER_KEY=$(grep ^MASTER_KEY= .env | cut -d= -f2) python3 scripts/smoke.py
```

`smoke.py` no imprime ni acepta secretos por argv: la MASTER_KEY va por entorno
y todas las cabeceras/API key viajan en la configuración de `curl -K -` por
**stdin**. Usa `docker exec` para el listener admin (loopback) y la URL pública
para canal/evento/lectura. Si falla, intenta borrar la key temporal igualmente
(cascada de sus canales) y sale con código != 0.

Salud esperada: `{"service":"socket-core","status":"ok"}` en ambos puertos y
`[OK] smoke: key + canal + evento + lectura` al final.

## 3. Rollback

El deploy guarda el commit anterior en `.last_good_commit` y backups fechados en
`backups/`. En el **primer** despliegue el fichero no existe: `deploy.sh` lo
inicializa con el HEAD previo al `git fetch` (normalmente `main`). Si ese primer
intento falla en salud, el rollback vuelve a ese commit y restaura la DB, aunque
el fichero no existiera antes. El valor solo se **actualiza** tras salud +
chequeo público.

**Qué compose/Dockerfile usa el rollback:** los del commit viejo. `git reset
--hard "$LAST_GOOD"` deja en el árbol el `docker-compose.yml` y el `Dockerfile`
antiguos, y `docker compose build` reconstruye con ellos. Por eso el rollback
puede volver a una versión que corría como **root** y que **no tenía** el
listener admin (3015): el script exige solo el puerto público en la
reverificación del rollback (`wait_health_rollback`), no el 3015. Tras volver a
una versión antigua, el volumen `data/` puede quedar escrito por root; al
redesplegar la versión nueva hay que repetir `sudo chown -R 10001:10001 data`.

Simulación reproducible de ese caso (sin Docker real):

```bash
python -m unittest scripts.test_deploy.TestDeploy.test_primer_deploy_falla_salud_rollback_al_previo -v
python -m unittest scripts.test_deploy.TestDeploy.test_fetch_fallido_aborta_sin_tocar_nada -v
```

El primero arranca sin `.last_good_commit`, publica un commit nuevo cuyo health
falla, y comprueba que vuelve al HEAD previo, que `.last_good_commit` queda con
ese HEAD y que la DB recupera el marcador anterior.

```bash
cd /srv/infra/socket-core

# 1) Volver al código anterior
PREV=$(cat .last_good_commit)
git fetch origin
git reset --hard "$PREV"

# 2) Restaurar los datos previos (copia .db consistente; elige la anterior al fallo)
ls -1t backups/data-*.db | head
sudo python3 scripts/sqlite_backup.py restore   --backup backups/data-AAAAMMDD-HHMMSS.db --db data/socket-core.db
sudo chown -R 10001:10001 data

# 3) Reconstruir y arrancar
docker compose build socket-core
docker compose up -d --remove-orphans

# 4) Verificar
docker exec socket-core curl -fsS http://127.0.0.1:3005/health
```

Si el rollback es por un cambio incompatible de datos (migración), restaurar el
backup devuelve el esquema anterior: **el binario antiguo volverá a leer su DB**.
No mezcles binario nuevo con backup viejo ni al contrario sin probarlo en una copia.


## 4. Variante: solo /ws público

Si **todos los backends** están en `central_network` (usan `http://socket-core:3005`)
y no necesitas REST público, puedes dejar expuesto solo el WebSocket. Sustituye en
`nginx/ws.seventwo.tech.https.conf` las locations de `/health`, `/stats` y
`/channels` por:

```nginx
location /admin/ { return 404; }
location /ws { ...igual que ahora... }
location / { return 404; }
```

Activación:
1. Edita el fichero en el repo (no en el VPS).
2. `sudo nginx -t` (con backup previo; el deploy lo hace solo).
3. `sudo systemctl reload nginx`.
4. Verifica `curl -k -H 'Host: ws...' https://.../channels` => 404 y que el WS
   sigue funcionando. Si algo falla, restaura el backup de Nginx.

**Rate limit por IP con backends directos.** `IP_RATE_PER_MIN` (1..100000, 600
por defecto) cuenta por IP resuelta. Si un backend llama directo a
`http://socket-core:3005` desde `central_network`, todas sus peticiones comparten
la IP de su contenedor; si varios contenedores salen por el gateway, comparten la
IP del gateway. Con Nginx delante y sin `TRUSTED_PROXIES`, todas las peticiones
cuentan como la IP de Nginx. Ajusta el valor al agregado real: el límite por key
(`rate_limit_per_min`) sigue siendo independiente y por proyecto.

## 5. fail2ban: alcance y recidive

- La jail usa `port = http,https`, por lo que un baneo afecta a **80/443 de TODOS
  los dominios** de ese VPS (no solo al hub). Tenlo en cuenta antes de bajar
  `maxretry`.
- `ignoreip` excluye loopback y `172.18.0.0/16` (red Docker).
- Interacción con **recidive**: si tienes la jail `recidive` activa, los baneos
  repetidos de `socketcore-auth` pueden acabar en baneos más largos/generales.
  Revisa `bantime.increment` y los logs de recidive para no bloquear IPs legítimas.
- Solo cuentan 401/403; los 429 (rate limit) **no** se banean.

## 6. Sudoers del deploy

Ver **§10** y `docs/sudoers-deploy.example`. No se aplica automáticamente.

## 7. Qué script de deploy se ejecuta

Hoy `ci.yml` hace, por SSH: `git show origin/main:scripts/deploy.sh | bash -s`, es
decir, **siempre el script de origin/main** (aunque el working tree esté viejo).
Para una rama, el runbook de §1.0 usa `DEPLOY_REF=<rama>` (el script hace
`git fetch origin "$DEPLOY_REF"` y `git reset --hard origin/$DEPLOY_REF`).
Además, `deploy.sh` compara el script de origin con el que se está ejecutando y, si
difieren, se **re-ejecuta** desde la versión nueva (protege ejecuciones manuales de
una copia vieja). El commit sano se guarda en `.last_good_commit` **solo** tras
salud + chequeo público.

### 7.1 Remoto SSH y deploy key de solo lectura (R12)

El remoto actual es HTTPS; para que `git fetch` use una **deploy key de solo
lectura** (sin credenciales de usuario ni token en el remoto):

```bash
# En el VPS, como usuario del deploy y con una clave dedicada:
ssh-keygen -t ed25519 -C "socket-core-deploy-readonly" -f ~/.ssh/socket-core-deploy -N ''
cat ~/.ssh/socket-core-deploy.pub
# GitHub -> repo -> Settings -> Deploy keys -> Add deploy key
#   Title: socket-core-readonly   Key: (pega la .pub)   NO marcar "Allow write access"
```

Configura el remoto y compruébalo:

```bash
cd /srv/infra/socket-core
git remote set-url origin git@github.com:mateodevcode/socket-core.git
GIT_SSH_COMMAND='ssh -i ~/.ssh/socket-core-deploy -o IdentitiesOnly=yes' git fetch origin main
ssh -i ~/.ssh/socket-core-deploy -o IdentitiesOnly=yes -T git@github.com
# esperado: "Hi ...! You've successfully authenticated, but GitHub does not provide shell access."
```

La deploy key **no** puede escribir: `deploy.sh` solo hace `fetch`, `show` y
`reset`; nunca `push`. La clave SSH del CI (`VPS_SSH_KEY`) es otra cosa: sirve
para entrar al VPS, no para GitHub. Si rotas la deploy key, actualiza
`~/.ssh/config`/`GIT_SSH_COMMAND` y prueba el fetch antes de desplegar.


## 8. Programar estado.py (cron/systemd) sin filtrar MASTER_KEY

`estado.py` solo acepta `SOCKET_MASTER_KEY` (env) o `--master-key`; ya **no** lee
ficheros `.master_key` del repo. Para cron/systemd usa un EnvironmentFile de root
con permisos 0600:

```bash
sudo install -d -m 0700 /etc/socket-core
sudo sh -c 'umask 077; printf "SOCKET_MASTER_KEY=%s\n" "<MASTER_KEY>" > /etc/socket-core/estado.env'
sudo chown root:root /etc/socket-core/estado.env
sudo chmod 600 /etc/socket-core/estado.env
```

**Cron solo escribe en un log; no envía alertas.** Si el proceso falla o
`estado.py` sale != 0, nadie se entera salvo que revises el fichero. Para un
aviso real, usa un wrapper root-owned que lea los secretos de un fichero 0600 y
llame a `mail`/`msmtp`/`curl` **sin poner secretos en la línea de comandos**:

```bash
# /usr/local/sbin/socketcore-estado-cron  (root:root 0700)
#!/bin/sh
set -eu
set -a; . /etc/socket-core/estado.env; set +a
if ! /usr/bin/python3 /srv/infra/socket-core/scripts/estado.py >> /var/log/socketcore-estado.log 2>&1; then
  # La URL/token del webhook viven en webhook.curl (0600), nunca en argv.
  echo "socket-core: estado.py fallo ($(date -Is))" |
    curl -sS -K /etc/socket-core/webhook.curl --data-binary @- >/dev/null || true
fi
```

```cron
0 * * * * root /usr/local/sbin/socketcore-estado-cron
```

`/etc/socket-core/webhook.curl` (root 0600):
```text
url = "https://hooks.example/xxxx"
header = "Authorization: Bearer <token>"
```
Para correo, `msmtp` lee su configuración de `/etc/msmtprc` (0600) y el wrapper
hace `... | msmtp -t`; no pases contraseñas por argumentos. En systemd puedes
usar `OnFailure=socketcore-aviso.service` en el timer/servicio en vez del `if`.

Cron directo (sin aviso, solo log):
```cron
0 * * * * root set -a; . /etc/socket-core/estado.env; set +a; python3 /srv/infra/socket-core/scripts/estado.py >> /var/log/socketcore-estado.log 2>&1
```

systemd (timer):
```ini
# /etc/systemd/system/socketcore-estado.service
[Service]
Type=oneshot
EnvironmentFile=/etc/socket-core/estado.env
ExecStart=/usr/bin/python3 /srv/infra/socket-core/scripts/estado.py
```
`EnvironmentFile` sigue exponiendo el valor a `systemctl show`/`/proc` según
configuración; restringe el acceso al host y rota si sospechas fuga. La clave
**nunca** va en el `ExecStart` ni en la línea de cron.

## 9. Alta de clientes: paquete y ficha de operador

`alta_cliente.py` escribe por defecto en `~/credenciales-socketcore` (creado 0700,
fuera del repo). Se niega a escribir dentro de un worktree git salvo que el fichero
esté ignorado (`git check-ignore`); el repo ignora `credenciales-*.txt`.
Genera dos ficheros 0600: el **paquete del proyecto** (API key, URL, prefijo,
scopes, snippet; SIN admin/contenedor/MASTER_KEY) y una **ficha de operador**
(`operador-*.txt`, con instrucciones de revocación, sin secretos).
No hay dominio fijo: define `SOCKET_PUBLIC_URL` o pasa `--core`.

## 10. Sudoers restringido: por qué no se aplica solo

Ver `docs/sudoers-deploy.example`. **No se aplica automáticamente**. Un `docker
exec`/`docker compose` por sudoers es root-equivalente; el ejemplo documenta por qué
no se delega Docker y la alternativa de wrappers root-owned. Procedimiento de
aplicación y rollback están en el propio fichero.
