#!/usr/bin/env python3
"""Alta de un proyecto cliente en socket-core.

Se ejecuta EN EL VPS. Lee MASTER_KEY del .env local (nada viaja por red salvo
al listener admin, que es loopback). Genera un **paquete de credenciales** en un
fichero con permisos 0600 (no imprime la API key en el terminal para no dejarla
en el historial ni en logs).

La MASTER_KEY NUNCA se pasa por argv: para `docker exec` va en la configuración
de curl por stdin (`-K -`).

Uso:
    cd /srv/infra/socket-core
    SOCKET_PUBLIC_URL=https://hub.ejemplo.com python3 scripts/alta_cliente.py
    python3 scripts/alta_cliente.py --list-prefixes

Salida por defecto: ~/credenciales-socketcore (0700, fuera del repo).

Solo librería estándar.
"""

import argparse
import datetime
import json
import os
import subprocess
import sys
from pathlib import Path
import urllib.error
import urllib.request

SCOPES_VALIDOS = {"publish", "subscribe", "manage"}


class AdminError(Exception):
    def __init__(self, status: int, body: str):
        super().__init__(f"HTTP {status}")
        self.status = status
        self.body = body


def default_scopes():
    """Scopes mínimos para el caso normal (sin manage: crear canales es del core)."""
    return ["publish", "subscribe"]


def default_out_dir() -> Path:
    return Path.home() / "credenciales-socketcore"


def default_core() -> str:
    """Sin dominio fijo: SOCKET_PUBLIC_URL o --core obligatorio."""
    return os.environ.get("SOCKET_PUBLIC_URL", "").strip()


def parse_entero(texto: str, nombre: str, minimo: int, maximo: int, permitir_vacio=False):
    """Valida enteros con mensajes claros. Devuelve (ok, valor, mensaje)."""
    t = (texto or "").strip()
    if not t:
        if permitir_vacio:
            return True, None, ""
        return False, None, f"{nombre} es obligatorio"
    try:
        v = int(t)
    except ValueError:
        return False, None, f"{nombre} debe ser un número entero (recibido: {t!r})"
    if v < minimo or v > maximo:
        return False, None, f"{nombre} debe estar entre {minimo} y {maximo} (recibido: {v})"
    return True, v, ""


def ensure_safe_out(outdir: Path, filename: str) -> tuple:
    """Crea el directorio 0700 y evita escribir en un worktree si no está ignorado."""
    outdir = outdir.expanduser().resolve()
    try:
        outdir.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name == "posix":
            os.chmod(outdir, 0o700)
    except Exception as e:
        return False, f"no se pudo crear {outdir}: {e}"
    destino = outdir / filename
    try:
        r = subprocess.run(
            ["git", "-C", str(outdir), "rev-parse", "--is-inside-work-tree"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception:
        r = None
    dentro = bool(r and r.returncode == 0 and (r.stdout or "").strip() == "true")
    if dentro:
        ign = subprocess.run(
            ["git", "-C", str(outdir), "check-ignore", "-q", str(destino)],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if ign.returncode != 0:
            return False, (
                f"{outdir} está dentro de un worktree git y '{filename}' NO está ignorado. "
                "Añade 'credenciales-*.txt' al .gitignore o usa --out fuera del repo."
            )
    return True, str(destino)


def _curl_cfg(value: str) -> str:
    out = value.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + out.replace("\n", "\\n").replace("\r", "\\r") + '"'


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
    """curl dentro del contenedor con cabecera/cuerpo por stdin (-K -)."""
    port = args.admin.rstrip("/").rsplit(":", 1)[-1] or "3015"
    cfg = [
        "url = " + _curl_cfg(f"http://127.0.0.1:{port}{path}"),
        "request = " + _curl_cfg(method),
        "header = " + _curl_cfg("X-Master-Key: " + master),
        "silent",
        "show-error",
        "max-time = 20",
        'write-out = "\\n%{http_code}"',
    ]
    if payload is not None:
        cfg.append("header = " + _curl_cfg("Content-Type: application/json"))
        cfg.append("data = " + _curl_cfg(json.dumps(payload)))
    r = subprocess.run(
        ["docker", "exec", "-i", "socket-core", "curl", "-sS", "-K", "-"],
        input="\n".join(cfg) + "\n",
        capture_output=True,
        text=True,
        timeout=30,
        shell=False,
    )
    if r.returncode != 0:
        raise AdminError(0, r.stderr.decode(errors="replace").strip() if isinstance(r.stderr, bytes) else (r.stderr or "").strip())
    raw = r.stdout or ""
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
        return json.loads(cuerpo)["data"]
    except Exception:
        return []


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
    if status == 409:
        return f"Ya existe un proyecto con ese nombre: {error}"
    return f"El hub rechazó la creación (HTTP {status}): {error or 'sin detalle'}"


def render_package(project, key_id, api_key, public_url, domains, prefix, scopes, examples=None) -> str:
    """Paquete para el PROYECTO: sin admin, sin contenedor y sin MASTER_KEY."""
    _ = examples
    ejemplo = prefix.replace(":*", ":general") if ":*" in prefix else prefix
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

# --- Backend del proyecto: pedir JWT (aquí vive la API key) ---
curl -X POST {public_url}/auth/token \\
  -H "X-Api-Key: {api_key}" \\
  -H "Content-Type: application/json" \\
  -d '{{"user_id":"<id-opaco-del-usuario>","channel":"{ejemplo}","role":"both"}}'

# --- Navegador: WebSocket (SOLO con el JWT; nunca con la API key) ---
# wss://<host-del-hub>/ws?token=<JWT>
#   handshake: {{"channel":"{ejemplo}","device":"web"}}
"""


def render_operator_sheet(project, key_id, prefix, admin_hint) -> str:
    """Ficha SOLO para el operador del hub (no se entrega al proyecto)."""
    return f"""# FICHA DE OPERADOR — NO ENTREGAR AL PROYECTO
# Proyecto: {project}   KEY_ID={key_id}   prefijo={prefix}
# Requiere MASTER_KEY (no se escribe aquí).
# {admin_hint}
# Revocar:  POST /admin/keys/{key_id}/revoke
# Borrar:   DELETE /admin/keys/{key_id}   (borra sus canales en cascada)
# Listar:   GET /admin/keys
"""


def write_package(path: str, content: str) -> str:
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
    ap.add_argument("--core", default=default_core(), help="URL pública del hub (o SOCKET_PUBLIC_URL)")
    ap.add_argument("--admin", default="http://127.0.0.1:3015", help="listener admin (loopback)")
    ap.add_argument("--env", default=".env")
    ap.add_argument("--out", default="", help="directorio de salida (por defecto ~/credenciales-socketcore)")
    ap.add_argument("--list-prefixes", action="store_true", help="lista prefijos y sale")
    args = ap.parse_args()

    if not args.core:
        print("[ERROR] Falta la URL pública del hub: define SOCKET_PUBLIC_URL o usa --core.")
        return 1

    master = leer_master_key(resolver_env(args.env))
    if not master:
        print(f"[ERROR] No encontré MASTER_KEY en {resolver_env(args.env)}.")
        return 1

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

    prefix = preguntar("Prefijo de canales", f"{name}:*")
    scopes_raw = preguntar("Scopes (manage solo si el proyecto crea canales)", ",".join(default_scopes()))
    scopes = [s.strip() for s in scopes_raw.split(",") if s.strip()]

    rate_txt = preguntar("Rate limit por minuto", "120")
    ok, rate, msg = parse_entero(rate_txt, "rate_limit_per_min", 1, 100000)
    if not ok:
        print(f"[ERROR] {msg}")
        return 1

    exp_txt = preguntar("Vigencia en días (vacío = sin expiración)", "")
    ok, exp, msg = parse_entero(exp_txt, "expires_in_days", 1, 3650, permitir_vacio=True)
    if not ok:
        print(f"[ERROR] {msg}")
        return 1

    payload = {
        "name": name,
        "domains": domains,
        "channel_prefix": prefix or f"{name}:*",
        "scopes": scopes,
        "rate_limit_per_min": rate,
    }
    if exp is not None:
        payload["expires_in_days"] = exp

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
    outdir = Path(args.out).expanduser() if args.out else default_out_dir()
    nombre_pkg = f"credenciales-{seguro}-{stamp}.txt"
    ok, ruta = ensure_safe_out(outdir, nombre_pkg)
    if not ok:
        print(f"[ERROR] {ruta}")
        return 1

    paquete = render_package(
        project=d["name"],
        key_id=d["id"],
        api_key=d["api_key"],
        public_url=args.core.rstrip("/"),
        domains=d.get("domains") or domains,
        prefix=d.get("channel_prefix") or prefix,
        scopes=d.get("scopes") or scopes,
    )
    write_package(ruta, paquete)

    admin_hint = f"docker exec -i socket-core curl ... http://127.0.0.1:{args.admin.rstrip('/').rsplit(':', 1)[-1] or '3015'}"
    ficha = os.path.join(ruta.rsplit(os.sep, 1)[0], f"operador-{seguro}-{stamp}.txt")
    write_package(ficha, render_operator_sheet(d["name"], d["id"], d.get("channel_prefix") or prefix, admin_hint))

    print("\n" + "=" * 64)
    print("[OK] Paquete de credenciales generado (LA KEY SE MUESTRA UNA SOLA VEZ)")
    print(f"     Proyecto: {d['name']}   Key ID: {d['id']}")
    print(f"     Paquete:  {ruta}   (0600)")
    print(f"     Operador: {ficha}   (0600; NO entregar al proyecto)")
    print("     Muévela al gestor de secretos del proyecto y BORRA el paquete.")
    print("     La API key NO se imprime aquí para no dejarla en el historial/logs.")
    print("=" * 64)
    return 0


if __name__ == "__main__":
    sys.exit(main())
