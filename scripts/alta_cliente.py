#!/usr/bin/env python3
"""Alta de un proyecto cliente en socket-core.

Se ejecuta EN EL VPS. Lee MASTER_KEY del .env local (nada viaja por red salvo
al listener admin, que es loopback). Genera un **paquete de credenciales** en un
fichero con permisos 0600 (no imprime la API key en el terminal para no dejarla
en el historial ni en logs).

Uso:
    cd /srv/infra/socket-core
    python3 scripts/alta_cliente.py [--env .env] [--admin http://127.0.0.1:3015]
    python3 scripts/alta_cliente.py --list-prefixes   # ver prefijos existentes

Acceso admin: directo a --admin o, si no responde (normal desde el host),
`docker exec -i socket-core curl ... http://127.0.0.1:3015/...`.

Solo librería estándar.
"""

import argparse
import datetime
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request


class AdminError(Exception):
    def __init__(self, status: int, body: str):
        super().__init__(f"HTTP {status}")
        self.status = status
        self.body = body


def default_scopes():
    """Scopes mínimos para el caso normal (sin manage: crear canales es del core)."""
    return ["publish", "subscribe"]


def resolver_env(ruta: str) -> str:
    if os.path.isfile(ruta):
        return ruta
    if ruta == ".env":
        for c in ("../.env", "/srv/infra/socket-core/.env"):
            if os.path.isfile(c):
                return c
    return ruta


def leer_master_key(env_path: str) -> str:
    try:
        with open(env_path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line.startswith("MASTER_KEY="):
                    return line.split("=", 1)[1].strip().strip('"').strip("'")
    except FileNotFoundError:
        pass
    return ""


def _direct(args, method: str, path: str, payload, master: str):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{args.admin.rstrip('/')}{path}",
        data=data,
        headers={"Content-Type": "application/json", "X-Master-Key": master},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read().decode(), "directo"
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace"), "directo"


def _docker_exec(args, method: str, path: str, payload, master: str):
    port = args.admin.rstrip("/").rsplit(":", 1)[-1] or "3015"
    cmd = [
        "docker", "exec", "-i", "socket-core",
        "curl", "-sS", "-X", method,
        f"http://127.0.0.1:{port}{path}",
        "-H", "Content-Type: application/json",
        "-H", f"X-Master-Key: {master}",
        "-w", "\n%{http_code}",
    ]
    data = None if payload is None else json.dumps(payload).encode()
    if data is not None:
        cmd += ["--data-binary", "@-"]
    r = subprocess.run(cmd, input=data, capture_output=True, timeout=25)
    if r.returncode != 0:
        raise AdminError(0, r.stderr.decode(errors="replace").strip())
    raw = r.stdout.decode(errors="replace")
    cuerpo, _, codigo = raw.rpartition("\n")
    try:
        return int(codigo.strip() or "0"), cuerpo, "docker exec"
    except ValueError:
        return 0, raw, "docker exec"


def admin_request(args, method: str, path: str, payload=None, master: str = ""):
    """Devuelve (status, cuerpo_texto, vía). Lanza AdminError si no hay contacto."""
    try:
        return _direct(args, method, path, payload, master)
    except Exception:
        pass
    try:
        return _docker_exec(args, method, path, payload, master)
    except Exception as e:
        raise AdminError(0, str(e))


def listar_prefijos(args, master: str) -> list:
    status, cuerpo, _via = admin_request(args, "GET", "/admin/keys", None, master)
    if status != 200:
        return []
    try:
        keys = json.loads(cuerpo)["data"]
    except Exception:
        return []
    return keys


def imprimir_prefijos(keys: list) -> None:
    if not keys:
        print("   (no hay keys todavía)")
        return
    print("   Prefijos existentes:")
    for k in sorted(keys, key=lambda x: x.get("name", "")):
        estado = "REVOCADA" if k.get("revoked") else "activa"
        prefijos = ",".join(k.get("channel_prefixes") or [])
        print(f"     - {k.get('name','?'):24} {prefijos:24} [{estado}]")


def explain_create_error(status: int, body_text: str) -> str:
    try:
        error = str(json.loads(body_text).get("error", ""))
    except Exception:
        error = body_text.strip()
    if "solapa" in error.lower():
        return (
            f"{error}\n"
            "   Los proyectos no pueden compartir espacio de nombres (ni usar '*').\n"
            "   Incluye keys REVOCADAS: una key revocada sigue ocupando su prefijo\n"
            "   hasta que se BORRA o se le cambia el prefijo.\n"
            "   Consulta los existentes con:  python3 scripts/alta_cliente.py --list-prefixes\n"
            "   Para liberar: borra la key antigua (o edítale el prefijo) y reintenta."
        )
    if "internal" in error.lower() or status >= 500:
        return "Error interno del hub al crear la key. Revisa los logs del contenedor."
    return f"El hub rechazó la creación (HTTP {status}): {error or 'sin detalle'}"


def render_package(
    project,
    key_id,
    api_key,
    public_url,
    admin_url,
    domains,
    prefix,
    scopes,
    examples,
) -> str:
    ejemplo = prefix.replace(":*", ":general") if ":*" in prefix else prefix
    origen = examples[0] if examples else "https://tu-app.example"
    return f"""# Paquete de credenciales — socket-core
# Proyecto: {project}
# Generado: {datetime.datetime.now().isoformat(timespec="seconds")}
#
# La API key se muestra una sola vez. Guárdala en el gestor de secretos del
# proyecto y BORRA este fichero después. No la pongas en el frontend.

KEY_ID={key_id}
API_KEY={api_key}
PREFIX={prefix}
SCOPES={",".join(scopes)}
DOMAINS={",".join(domains)}
PUBLIC_URL={public_url}
ADMIN_EXEC=docker exec -i socket-core curl -sS http://127.0.0.1:3015

# --- Backend del proyecto: pedir JWT (aquí vive la API key) ---
curl -X POST {public_url}/auth/token \\
  -H "X-Api-Key: {api_key}" \\
  -H "Content-Type: application/json" \\
  -d '{{"user_id":"<id-opaco-del-usuario>","channel":"{ejemplo}","role":"both"}}'

# --- Navegador: WebSocket (SOLO con el JWT; nunca con la API key) ---
# ws(s)://<host>/ws?token=<JWT>
#   handshake: {{"channel":"{ejemplo}","device":"web"}}

# --- Referencia ---
# Emisor del JWT: {public_url}/auth/token
# Admin (loopback, MASTER_KEY): {admin_url}
# Dominios permitidos al emitir desde navegador: {", ".join(domains)}
# Prefijo de canales: {prefix}
"""


def write_package(path: str, content: str) -> str:
    """Escribe el paquete con permisos 0600 (solo el dueño puede leerlo)."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(content)
    if os.name == "posix":
        os.chmod(path, 0o600)
    return path


def preguntar(txt: str, defecto: str = "") -> str:
    suger = f" [{defecto}]" if defecto else ""
    r = input(f"{txt}{suger}: ").strip()
    return r if r else defecto


def main() -> int:
    ap = argparse.ArgumentParser(description="Alta de cliente en socket-core")
    ap.add_argument("--core", default="https://ws.seventwo.tech", help="URL pública del hub")
    ap.add_argument("--admin", default="http://127.0.0.1:3015", help="listener admin (loopback)")
    ap.add_argument("--env", default=".env")
    ap.add_argument("--out", default=".", help="directorio donde escribir el paquete 0600")
    ap.add_argument("--list-prefixes", action="store_true", help="lista prefijos y sale")
    args = ap.parse_args()

    master = leer_master_key(resolver_env(args.env))
    if not master:
        print(f"[ERROR] No encontré MASTER_KEY en {resolver_env(args.env)}.")
        return 1

    # Prefijos existentes (siempre antes de crear; también con --list-prefixes).
    try:
        keys = listar_prefijos(args, master)
    except AdminError as e:
        print(f"[ERROR] No pude contactar con el admin: {e}")
        return 1
    print("== Prefijos en uso ==")
    imprimir_prefijos(keys)
    if args.list_prefixes:
        return 0

    print("\n== Alta de proyecto cliente ==")
    name = preguntar("Nombre del proyecto (ej: monitoreo)")
    while not name:
        print("   el nombre es obligatorio.")
        name = preguntar("Nombre del proyecto (ej: monitoreo)")

    domains_raw = preguntar(
        "Dominios permitidos al emitir JWT (coma-separados, reales; ej: https://panel.midominio.com)"
    )
    while not domains_raw:
        print("   indica al menos un dominio real (no '*'): es la primera barrera anti-abuso.")
        domains_raw = preguntar("Dominios permitidos (coma-separados)")
    domains = [d.strip() for d in domains_raw.split(",") if d.strip()]
    if "*" in domains:
        print("   [AVISO] '*' permite cualquier web; en producción usa dominios reales.")
    if "*" not in domains and any(not d.startswith(("http://", "https://")) for d in domains):
        print("   [AVISO] se recomienda incluir esquema (https://...) en los dominios.")

    prefix = preguntar("Prefijo de canales", f"{name}:*")
    scopes_raw = preguntar("Scopes (manage solo si el proyecto crea canales)", ",".join(default_scopes()))
    scopes = [s.strip() for s in scopes_raw.split(",") if s.strip()]
    rate = preguntar("Rate limit por minuto", "120")
    exp = preguntar("Vigencia en días (vacío = sin expiración)", "")

    payload = {
        "name": name,
        "domains": domains,
        "channel_prefix": prefix or f"{name}:*",
        "scopes": scopes,
        "rate_limit_per_min": int(rate or 120),
    }
    if exp.strip():
        payload["expires_in_days"] = int(exp)

    print("\nSe creará esta key:")
    print(json.dumps(payload, indent=2, ensure_ascii=False))
    if preguntar("¿Confirmar? (s/N)", "N").lower() not in ("s", "si", "sí", "y", "yes"):
        print("Cancelado.")
        return 0

    status, cuerpo, via = admin_request(args, "POST", "/admin/keys", payload, master)
    if status != 201:
        print("\n[ERROR] " + explain_create_error(status, cuerpo))
        return 1
    d = json.loads(cuerpo)["data"]
    print(f"   (admin vía {via})")

    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    seguro = "".join(c if c.isalnum() or c in "-_." else "_" for c in name)
    paquete = render_package(
        project=d["name"],
        key_id=d["id"],
        api_key=d["api_key"],
        public_url=args.core.rstrip("/"),
        admin_url=f"docker exec -i socket-core curl -sS http://127.0.0.1:{args.admin.rstrip('/').rsplit(':', 1)[-1] or '3015'}",
        domains=d.get("domains") or domains,
        prefix=d.get("channel_prefix") or prefix,
        scopes=d.get("scopes") or scopes,
        examples=domains,
    )
    ruta = os.path.join(args.out, f"credenciales-{seguro}-{stamp}.txt")
    write_package(ruta, paquete)

    print("\n" + "=" * 64)
    print("[OK] Paquete de credenciales generado (LA KEY SE MUESTRA UNA SOLA VEZ)")
    print(f"     Proyecto: {d['name']}   Key ID: {d['id']}")
    print(f"     Fichero:  {ruta}   (permisos 0600)")
    print("     Muévela al gestor de secretos del proyecto y BORRA el fichero.")
    print("     La API key NO se imprime aquí para no dejarla en el historial/logs.")
    print("=" * 64)
    print("\nRevocar / borrar (listener admin loopback):")
    print(f"  docker exec -i socket-core curl -sS -X POST http://127.0.0.1:3015/admin/keys/{d['id']}/revoke -H 'X-Master-Key: <MASTER_KEY>'")
    print(f"  docker exec -i socket-core curl -sS -X DELETE http://127.0.0.1:3015/admin/keys/{d['id']} -H 'X-Master-Key: <MASTER_KEY>'")
    return 0


if __name__ == "__main__":
    sys.exit(main())
