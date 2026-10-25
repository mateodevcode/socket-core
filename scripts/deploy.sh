#!/usr/bin/env bash
# Deploy remoto de socket-core (se ejecuta en el VPS, invocado por CI por SSH).
# NO copia .env a /tmp. Usa un backup consistente de SQLite y .last_good_commit
# (solo se actualiza tras un despliegue sano).
set -euo pipefail
umask 077

ROOT="${DEPLOY_ROOT:-/srv/infra/socket-core}"
BRANCH="${DEPLOY_BRANCH:-main}"
SUDO="${SUDO-sudo}"
BACKUP_DIR="${BACKUP_DIR:-$ROOT/backups}"
DB_FILE="${DB_FILE:-$ROOT/data/socket-core.db}"
PUBLIC_URL="${PUBLIC_URL:-https://ws.seventwo.tech}"
NGINX_AVAILABLE="${NGINX_AVAILABLE:-/etc/nginx/sites-available}"
NGINX_ENABLED="${NGINX_ENABLED:-/etc/nginx/sites-enabled}"
NGINX_CONF_D="${NGINX_CONF_D:-/etc/nginx/conf.d}"
FAIL2BAN_FILTER="${FAIL2BAN_FILTER:-/etc/fail2ban/filter.d}"
FAIL2BAN_JAIL="${FAIL2BAN_JAIL:-/etc/fail2ban/jail.d}"
MAX_HEALTH_ATTEMPTS="${MAX_HEALTH_ATTEMPTS:-30}"
HEALTH_SLEEP="${HEALTH_SLEEP:-2}"
SKIP_CERTBOT="${SKIP_CERTBOT:-0}"

log() { echo "→ $*"; }
fail() { echo "❌ $*" >&2; exit 1; }

cd "$ROOT"
mkdir -p "$BACKUP_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
PREV="$(git rev-parse HEAD)"
log "Commit previo: $PREV"

# Trae origin y, si el script de origin difiere del que se está ejecutando,
# re-ejecuta el nuevo (protege invocaciones manuales de una versión vieja).
git fetch origin "$BRANCH"
NUEVO="$BACKUP_DIR/deploy-origin-$STAMP.sh"
git show "origin/$BRANCH:scripts/deploy.sh" > "$NUEVO" \
  || fail "no se pudo obtener scripts/deploy.sh de origin/$BRANCH"
chmod 700 "$NUEVO"
if [ "${DEPLOY_REEXEC:-0}" != "1" ] && ! cmp -s "$NUEVO" "${BASH_SOURCE[0]}"; then
  log "El script de origin difiere del actual; re-ejecutando el nuevo"
  DEPLOY_REEXEC=1 exec bash "$NUEVO"
fi
rm -f "$NUEVO"

# ---------- R4: backup consistente de SQLite (API backup + integrity_check) ----------
if [ -f "$DB_FILE" ]; then
  HELPER="scripts/sqlite_backup.py"
  if [ ! -f "$HELPER" ]; then
    HELPER="$BACKUP_DIR/sqlite_backup.py"
    git show "origin/$BRANCH:scripts/sqlite_backup.py" > "$HELPER" \
      || fail "no se pudo obtener scripts/sqlite_backup.py"
  fi
  log "Backup consistente de SQLite: data-$STAMP.db"
  $SUDO python3 "$HELPER" backup --db "$DB_FILE" --out "$BACKUP_DIR/data-$STAMP.db" \
    || fail "backup de SQLite falló (integridad)"
  find "$BACKUP_DIR" -maxdepth 1 -type f -name 'data-*.db' -printf '%T@ %p
'     | sort -rn | tail -n +11 | cut -d' ' -f2-     | while IFS= read -r viejo; do [ -n "$viejo" ] && rm -f "$viejo"; done
else
  log "Sin DB todavía; se omite backup inicial"
fi

# ---------- Actualizar código (sin tocar .env/data/backups/proyectos) ----------
git reset --hard HEAD >/dev/null 2>&1 || true
git clean -fd -e proyectos/ -e .env -e data/ -e backups/ -e .last_good_commit >/dev/null 2>&1 || true
$SUDO chown -R 10001:10001 data 2>/dev/null || true
log "Actualizando a origin/$BRANCH"
git reset --hard "origin/$BRANCH"

[ -f .env ] || fail "Falta .env en $ROOT (cópialo una vez, no se versiona)"

# ---------- Build + arranque ----------
log "Construyendo imagen"
docker compose build socket-core || fail "el build falló (no se toca .last_good_commit)"
docker rm -f socket-core >/dev/null 2>&1 || true
log "Levantando contenedor"
docker compose up -d --remove-orphans || fail "docker compose up falló"

salud_ok() {
  curl -fsS --max-time 3 http://127.0.0.1:3005/health >/dev/null 2>&1 \
    && docker exec socket-core curl -fsS --max-time 3 http://127.0.0.1:3015/health >/dev/null 2>&1
}
wait_health() {
  local i
  for ((i = 0; i < MAX_HEALTH_ATTEMPTS; i++)); do
    if salud_ok; then return 0; fi
    sleep "$HEALTH_SLEEP"
  done
  return 1
}

restore_db() {
  local backup
  backup="$(find "$BACKUP_DIR" -maxdepth 1 -type f -name 'data-*.db'     -printf '%T@ %p
' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)"
  if [ -n "$backup" ]; then
    log "Restaurando DB desde $(basename "$backup")"
    $SUDO python3 "$HELPER" restore --backup "$backup" --db "$DB_FILE" \
      || echo "⚠️ restore de DB falló" >&2
    $SUDO chown -R 10001:10001 data 2>/dev/null || true
  else
    echo "⚠️ Sin backup de DB para restaurar" >&2
  fi
}

# ---------- R2: rollback SOLO al último commit sano ----------
if ! wait_health; then
  log "Salud falló. Rollback..."
  docker compose logs --tail=120 socket-core || true
  LAST_GOOD="$(cat .last_good_commit 2>/dev/null || true)"
  if [ -n "$LAST_GOOD" ]; then
    log "Volviendo a .last_good_commit=$LAST_GOOD"
    git reset --hard "$LAST_GOOD"
    restore_db
    docker compose build socket-core || true
    docker compose up -d --remove-orphans || true
    if wait_health; then
      log "Rollback sano en $LAST_GOOD"
    else
      echo "❌ El rollback tampoco quedó sano; intervención manual" >&2
    fi
  else
    echo "❌ Sin .last_good_commit (primer deploy): no hay rollback automático" >&2
  fi
  exit 1
fi

# ---------- R3: Nginx con backup + restore si -t/reload fallan ----------
NGINX_BAK="$BACKUP_DIR/nginx-$STAMP"
mkdir -p "$NGINX_BAK"
: > "$NGINX_BAK/existentes"
: > "$NGINX_BAK/creados"
backup_nginx() {
  local f
  for f in \
    "$NGINX_AVAILABLE/ws.seventwo.tech.http.conf" \
    "$NGINX_AVAILABLE/ws.seventwo.tech.https.conf" \
    "$NGINX_CONF_D/socketcore-limits.conf"; do
    if [ -f "$f" ]; then
      $SUDO cp "$f" "$NGINX_BAK/$(basename "$f")"
      echo "$f" >> "$NGINX_BAK/existentes"
    else
      echo "$f" >> "$NGINX_BAK/creados"
    fi
  done
}
restore_nginx() {
  local f
  while IFS= read -r f; do
    [ -n "$f" ] && $SUDO cp "$NGINX_BAK/$(basename "$f")" "$f"
  done < "$NGINX_BAK/existentes"
  while IFS= read -r f; do
    [ -n "$f" ] && $SUDO rm -f "$f"
  done < "$NGINX_BAK/creados"
}

backup_nginx
log "Aplicando configuración de Nginx"
$SUDO cp nginx/ws.seventwo.tech.http.conf "$NGINX_AVAILABLE/ws.seventwo.tech.http.conf"
$SUDO ln -sf "$NGINX_AVAILABLE/ws.seventwo.tech.http.conf" "$NGINX_ENABLED/ws.seventwo.tech.http.conf"
$SUDO cp nginx/conf.d/socketcore-limits.conf "$NGINX_CONF_D/socketcore-limits.conf"

log "nginx -t (fase 80)"
if ! $SUDO nginx -t; then
  restore_nginx
  $SUDO nginx -t || true
  fail "nginx -t falló; configuración restaurada"
fi
$SUDO systemctl reload nginx || {
  restore_nginx
  $SUDO nginx -t || true
  fail "reload de nginx falló; configuración restaurada"
}

if [ "$SKIP_CERTBOT" != "1" ]; then
  if [ ! -f /etc/letsencrypt/live/ws.seventwo.tech/fullchain.pem ]; then
    log "Obteniendo certificado"
    $SUDO certbot certonly --webroot -w /var/www/html \
      -d ws.seventwo.tech --non-interactive --agree-tos || fail "certbot falló"
  fi
fi

log "Aplicando Nginx 443"
$SUDO cp nginx/ws.seventwo.tech.https.conf "$NGINX_AVAILABLE/ws.seventwo.tech.https.conf"
$SUDO ln -sf "$NGINX_AVAILABLE/ws.seventwo.tech.https.conf" "$NGINX_ENABLED/ws.seventwo.tech.https.conf"

if command -v fail2ban-client >/dev/null 2>&1; then
  log "fail2ban (401/403; 429 no cuenta)"
  $SUDO cp fail2ban/filter.d/socketcore-auth.conf "$FAIL2BAN_FILTER/socketcore-auth.conf"
  $SUDO cp fail2ban/jail.d/socketcore-auth.conf "$FAIL2BAN_JAIL/socketcore-auth.conf"
  $SUDO fail2ban-client reload || true
  $SUDO fail2ban-client status socketcore-auth || true
fi

log "nginx -t (final)"
if ! $SUDO nginx -t; then
  restore_nginx
  $SUDO nginx -t || true
  fail "nginx -t final falló; configuración restaurada"
fi
$SUDO systemctl reload nginx || {
  restore_nginx
  $SUDO nginx -t || true
  fail "reload final de nginx falló; configuración restaurada"
}

# ---------- Chequeo público (obligatorio antes de marcar el commit como bueno) ----------
log "Chequeo público $PUBLIC_URL/health"
OK=0
for i in 1 2 3 4 5 6; do
  if curl -f --max-time 10 "$PUBLIC_URL/health"; then OK=1; break; fi
  echo "→ intento $i falló, reintentando..."
  sleep 2
done
[ "$OK" = "1" ] || fail "el chequeo público falló; .last_good_commit NO se actualiza"

DEPLOYED="$(git rev-parse HEAD)"
echo "$DEPLOYED" > .last_good_commit
log "Deploy OK. .last_good_commit=$DEPLOYED"
docker compose ps || true
