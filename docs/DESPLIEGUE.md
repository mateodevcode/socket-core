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

# Deben cumplirse o el proceso NO arranca:
#   JWT_SECRET != valor por defecto y >= 32 chars; MASTER_KEY >= 32; JWT_TTL_SECS en [60,3600]
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

```bash
cd /srv/infra/socket-core
cp data/socket-core.db /tmp/migracion-prueba.db

# Arranca una instancia temporal contra la copia (puertos altos, sin tocar el servicio)
docker run --rm -v /tmp/migracion-prueba.db:/tmp/hub.db \
  -e APP_ENV=dev -e PORT=39905 -e ADMIN_PORT=39915 \
  -e JWT_SECRET="$(openssl rand -hex 64)" -e MASTER_KEY="$(openssl rand -hex 32)" \
  -e DATABASE_URL="sqlite:/tmp/hub.db?mode=rwc" -e ALLOW_BOOTSTRAP=false \
  -p 127.0.0.1:39905:39905 socket-core:latest &
sleep 3
# (La instancia sale sola al terminar; si no, docker stop)

# Recuento de huérfanos en la copia (requiere sqlite3 en el host)
sqlite3 /tmp/migracion-prueba.db "SELECT COUNT(*) FROM channels WHERE project_id='';"
sqlite3 /tmp/migracion-prueba.db "SELECT name FROM channels WHERE project_id='';"
```

Salida real de una migración de prueba (canal con dueño por prefijo + uno huérfano):

```text
canales: [('demo:chat', 'legacy-key'), ('orphan:x', '')]
huérfanos (project_id=''): 1
```

Si hay huérfanos esperados, decide antes de desplegar: renombrar el canal, ajustar
el prefijo de una key, o aceptar que queden inaccesibles. NO borres filas a mano
sin backup.

**Asignar un canal huérfano a un proyecto (manual, sobre la copia primero):**

```sql
-- Repite por cada canal huérfano, eligiendo la key dueña por su nombre.
-- (Ajusta 'proyecto-demo' y 'canal-huerfano' a tus datos reales.)
UPDATE channels
SET project_id = (SELECT id FROM api_keys WHERE name = 'proyecto-demo')
WHERE project_id = '' AND name = 'canal-huerfano';
```

Verifica dentro de la copia:

```bash
sqlite3 /tmp/migcheck/hub.db "SELECT COUNT(*) FROM channels WHERE project_id='';"
```

Después, en el deploy real (con backup hecho): aplica el UPDATE sobre
`data/socket-core.db` con el servicio parado, `sudo chown -R 10001:10001 data`, y
arranca. Un canal huérfano (`project_id=''`) hoy es **invisible para todos**
(responde 404) hasta asignarle dueño.

> **Tests de red:** `admin_no_escucha_en_otra_interfaz` requiere una interfaz
> no-loopback. Si el entorno no la tiene, el test **falla** para no dar falsos
> positivos; define `SKIP_NET_IFACE_TEST=1` para omitirlo explícitamente (p. ej. CI
> sin red externa).

> Si no tienes `sqlite3` en el host, puedes consultar la copia con python3:
> `python3 -c "import sqlite3;c=sqlite3.connect('/tmp/migcheck/hub.db');print(c.execute(\"SELECT COUNT(*) FROM channels WHERE project_id=''\").fetchone()[0])"`
> o revisar los logs del arranque: la migración informa
> `Migración de canales: N repartidos por prefijo`.

### 0.3 Backup de la DB

```bash
cd /srv/infra/socket-core
mkdir -p backups
STAMP=$(date +%Y%m%d-%H%M%S)
sudo tar czf "backups/data-$STAMP.tgz" data
echo "backup: backups/data-$STAMP.tgz"
```

El propio deploy automático hace también este backup (retención de 10) y guarda el
commit previo en `.last_deploy_commit`.

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

> **Primer despliegue de esta rama (manual):** no fusiones a `main` todavía.
> Despliega la rama a mano con `docker compose logs -f socket-core` abierto en otra
> terminal para ver el arranque y cualquier fallo; verifica `/health`, `estado.py` y
> un mensaje de prueba. Solo después fusiona a `main` para que el workflow automático
> tome el relevo.

El despliegue es automático: push a `main` → workflow **CI** (`quality`) → si pasa,
job **deploy** (needs) entra por SSH, hace backup, actualiza, reconstruye y espera
hasta **60 s** a que `/health` responda en el puerto público **y** en el admin dentro
del contenedor. Si no responde, hace **rollback automático**: vuelve al commit de
`.last_deploy_commit`, restaura el backup más reciente, reconstruye, reverifica 60 s
y **marca el job como fallido** (`exit 1`) para que se investigue. No se despliega
si CI falla.

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
- `restart: on-failure:5`: tras 5 fallos seguidos el contenedor queda parado para no
  entrar en bucle. Revisa `docker compose logs` y vuelve a `up -d` tras corregir.
  Alternativa: `unless-stopped` (reintenta indefinidamente; enmascara crash loops).
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
# 1) Salud de ambos listeners dentro del contenedor
docker exec socket-core curl -fsS http://127.0.0.1:3005/health
docker exec socket-core curl -fsS http://127.0.0.1:3015/health

# 2) Estado completo (contenedor, stats por proyecto, Nginx, cert, fail2ban, TRUSTED_PROXIES)
SOCKET_MASTER_KEY=$(grep ^MASTER_KEY= .env | cut -d= -f2) python3 scripts/estado.py

# 3) Prueba de mensaje real con una key de pruebas (backend -> hub)
MASTER=$(grep ^MASTER_KEY= .env | cut -d= -f2)
dc() { docker exec -i socket-core curl -sS "$@"; }
dc -X POST http://127.0.0.1:3015/admin/keys -H "X-Master-Key: $MASTER" \
  -H 'Content-Type: application/json' \
  -d '{"name":"smoke-test","channel_prefix":"smoke:*","domains":["https://smoke.example"],"scopes":["publish","subscribe","manage"]}'
# guarda la api_key de la respuesta (solo se muestra una vez) y:
docker exec -i socket-core curl -sS -X POST http://127.0.0.1:3005/channels \
  -H "X-Api-Key: <API_KEY>" -H 'Content-Type: application/json' -d '{"name":"smoke:chat"}'
docker exec -i socket-core curl -sS -X POST http://127.0.0.1:3005/channels/smoke:chat/events \
  -H "X-Api-Key: <API_KEY>" -H 'Content-Type: application/json' \
  -d '{"source":"smoke","targets":["*"],"payload":{"type":"ping"}}'
# 4) Limpieza: revocar y borrar la key de pruebas
dc -X POST http://127.0.0.1:3015/admin/keys/<KEY_ID>/revoke -H "X-Master-Key: $MASTER"
dc -X DELETE http://127.0.0.1:3015/admin/keys/<KEY_ID> -H "X-Master-Key: $MASTER"
```

Salud esperada: `{"service":"socket-core","status":"ok"}` en ambos puertos y
`{"success":true,...}` al emitir.

## 3. Rollback

El deploy guarda el commit anterior en `.last_deploy_commit` y backups fechados en
`backups/`.

```bash
cd /srv/infra/socket-core

# 1) Volver al código anterior
PREV=$(cat .last_deploy_commit)
git fetch origin
git reset --hard "$PREV"

# 2) Restaurar los datos previos (elige el backup anterior al fallo)
ls -1t backups/data-*.tgz | head
sudo tar xzf backups/data-AAAAMMDD-HHMMSS.tgz -C /srv/infra/socket-core
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
