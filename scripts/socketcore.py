#!/usr/bin/env python3
"""Herramienta de operador de socket-core (se ejecuta EN EL SERVIDOR).

  python3 scripts/socketcore.py listar
  python3 scripts/socketcore.py alta
  python3 scripts/socketcore.py revocar NOMBRE
  python3 scripts/socketcore.py borrar  NOMBRE
  python3 scripts/socketcore.py editar  NOMBRE [--scopes a,b] [--dominios x,y]
                                               [--rate N] [--expira-dias N] [--prefijo p:*]

Nunca muestra ni guarda API keys. La MASTER_KEY se lee de .env y viaja por la
entrada estándar de curl (no aparece en la lista de procesos). Guía completa:
docs/OPERADOR.md.

Variables opcionales:
  SC_REPO              carpeta del proyecto (por defecto, la que contiene scripts/)
  SC_ADMIN             URL del panel admin dentro del contenedor (http://127.0.0.1:3015)
  SOCKET_PUBLIC_URL    URL pública del hub; solo la usa el comando `alta`
"""

import argparse
import datetime
import glob
import json
import os
import subprocess
import sys
from pathlib import Path

HOME = os.path.expanduser("~")
REPO = os.environ.get("SC_REPO") or str(Path(__file__).resolve().parent.parent)
ADM = os.environ.get("SC_ADMIN", "http://127.0.0.1:3015")
CORE = os.environ.get("SOCKET_PUBLIC_URL", "")


def q(s):
    """Protege comillas y barras para el fichero de configuración de curl."""
    return s.replace("\\", "\\\\").replace('"', '\\"')


def curl(url, method="GET", headers=None, body=None):
    """Ejecuta curl DENTRO del contenedor, con la configuración por stdin.
    Devuelve (código_http, cuerpo)."""
    hs = dict(headers or {})
    if body is not None:
        hs["Content-Type"] = "application/json"
    lines = [
        f'url = "{q(url)}"',
        f'request = "{method}"',
        'write-out = "\\n%{http_code}"',
    ]
    lines += [f'header = "{q(k)}: {q(v)}"' for k, v in hs.items()]
    if body is not None:
        lines.append(f'data = "{q(json.dumps(body))}"')
    r = subprocess.run(
        ["docker", "exec", "-i", "socket-core", "curl", "-sS", "-m", "10", "-K", "-"],
        input="\n".join(lines).encode(),
        capture_output=True,
    )
    out = r.stdout.decode(errors="replace")
    texto, _, code = out.rpartition("\n")
    try:
        return int(code.strip() or 0), texto
    except ValueError:
        return 0, out


def js(t):
    try:
        return json.loads(t)
    except Exception:
        return {}


def master():
    try:
        with open(f"{REPO}/.env", encoding="utf-8") as f:
            for ln in f:
                if ln.startswith("MASTER_KEY="):
                    return ln.strip().split("=", 1)[1].strip("\"'")
    except OSError:
        pass
    sys.exit(f"No puedo leer MASTER_KEY de {REPO}/.env")


def admin(method, path, body=None):
    return curl(ADM + path, method, {"X-Master-Key": master()}, body)


def todas():
    s, t = admin("GET", "/admin/keys")
    if s != 200:
        sys.exit(
            f"El admin respondió HTTP {s}. ¿Está el contenedor socket-core en marcha?"
        )
    return js(t)["data"]


def stats():
    s, t = admin("GET", "/admin/stats")
    if s != 200:
        return {}
    return {p["project_name"]: p for p in js(t).get("data", {}).get("projects", [])}


def buscar(nombre):
    for k in todas():
        if k["name"] == nombre:
            return k
    sys.exit(
        f"No existe el proyecto '{nombre}'. Mira los nombres con:  python3 {sys.argv[0]} listar"
    )


def fecha(e):
    if not e:
        return "nunca"
    d = (e - datetime.datetime.now().timestamp()) / 86400
    f = datetime.datetime.fromtimestamp(e).strftime("%Y-%m-%d")
    return f"{f} (CADUCADA)" if d < 0 else f"{f} (en {d:.0f} días)"


def cmd_listar(_):
    ks, st = todas(), stats()
    if not ks:
        print("No hay proyectos.")
    for k in sorted(ks, key=lambda x: x["name"]):
        s = st.get(k["name"], {})
        estado = "REVOCADA" if k["revoked"] else "activa"
        print(f"\n● {k['name']}   [{estado}]   id={k['id']}")
        print(
            f"    prefijo: {','.join(k['channel_prefixes'])}   scopes: {','.join(k['scopes'])}   límite: {k['rate_limit_per_min']}/min"
        )
        print(
            f"    dominios: {','.join(k['domains'])}   expira: {fecha(k.get('expires_at'))}"
        )
        print(
            f"    canales: {s.get('channels', '?')}   conexiones ahora: {s.get('connections', '?')}"
        )
        fs = sorted(
            glob.glob(f"{HOME}/credenciales-socketcore/credenciales-{k['name']}-*.txt")
        )
        print(
            f"    paquete de credenciales: {fs[-1] if fs else '(no hay fichero: la key solo se mostró al crearla)'}"
        )
    print(f"\n{len(ks)} proyecto(s). Las API keys NO se pueden volver a ver.")


def cmd_revocar(a):
    k = buscar(a.nombre)
    if k["revoked"]:
        sys.exit(f"'{a.nombre}' ya está revocada.")
    print(
        f"Se revocará '{a.nombre}': su key deja de valer y SUS CONEXIONES SE CORTAN AL INSTANTE."
    )
    print("No hay 'desrevocar'. Su prefijo seguirá ocupado hasta que la borres.")
    if input("¿Continuar? (s/N): ").strip().lower() not in ("s", "si", "sí"):
        return print("Cancelado.")
    s, t = admin("POST", f"/admin/keys/{k['id']}/revoke")
    print("revocada" if s == 200 else f"HTTP {s}: {js(t).get('error', t)[:200]}")


def cmd_borrar(a):
    k = buscar(a.nombre)
    n = stats().get(a.nombre, {}).get("channels", "?")
    print(
        f"Se BORRARÁ '{a.nombre}' y sus {n} canal(es). Libera el prefijo. No se puede deshacer."
    )
    if input(f"Escribe el nombre '{a.nombre}' para confirmar: ").strip() != a.nombre:
        return print("Cancelado.")
    s, t = admin("DELETE", f"/admin/keys/{k['id']}")
    d = js(t).get("data", {})
    print(
        f"borrada, canales eliminados: {d.get('channels_deleted', '?')}"
        if s == 200
        else f"HTTP {s}: {js(t).get('error', t)[:200]}"
    )


def cmd_editar(a):
    k = buscar(a.nombre)
    b = {}
    if a.scopes is not None:
        b["scopes"] = [x.strip() for x in a.scopes.split(",") if x.strip()]
    if a.dominios is not None:
        b["domains"] = [x.strip() for x in a.dominios.split(",") if x.strip()]
    if a.rate is not None:
        b["rate_limit_per_min"] = a.rate
    if a.expira_dias is not None:
        b["expires_in_days"] = a.expira_dias
    if a.prefijo is not None:
        b["channel_prefix"] = a.prefijo
    if not b:
        sys.exit(
            "Indica qué cambiar: --scopes, --dominios, --rate, --expira-dias o --prefijo"
        )
    if "scopes" in b or "channel_prefix" in b:
        print(
            "Cambiar scopes o prefijo CORTA las conexiones abiertas del proyecto (reconectan con los permisos nuevos)."
        )
    print("Cambios:", json.dumps(b, ensure_ascii=False))
    if input("¿Aplicar? (s/N): ").strip().lower() not in ("s", "si", "sí"):
        return print("Cancelado.")
    s, t = admin("PATCH", f"/admin/keys/{k['id']}", b)
    print("hecho" if s == 200 else f"HTTP {s}: {js(t).get('error', t)[:200]}")


def cmd_alta(_):
    os.chdir(REPO)
    cmd = ["python3", "scripts/alta_cliente.py"]
    if CORE:
        cmd += ["--core", CORE]
    sys.exit(subprocess.call(cmd))


def main():
    ap = argparse.ArgumentParser(description="Operador de socket-core")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("listar", help="ver los proyectos").set_defaults(f=cmd_listar)
    sub.add_parser("alta", help="crear un proyecto nuevo").set_defaults(f=cmd_alta)
    for n, f in (
        ("revocar", cmd_revocar),
        ("borrar", cmd_borrar),
        ("editar", cmd_editar),
    ):
        p = sub.add_parser(n)
        p.add_argument("nombre")
        p.set_defaults(f=f)
        if n == "editar":
            p.add_argument("--scopes")
            p.add_argument("--dominios")
            p.add_argument("--prefijo")
            p.add_argument("--rate", type=int)
            p.add_argument("--expira-dias", type=int, dest="expira_dias")
    a = ap.parse_args()
    a.f(a)


if __name__ == "__main__":
    main()
