#!/usr/bin/env python3
"""Alta de un proyecto cliente en socket-core.

Se ejecuta EN EL VPS (lee MASTER_KEY del .env local, nada viaja por red).
Pide los datos del cliente y devuelve el bloque de credenciales listo para copiar
al backend del proyecto.

Uso:
    cd /srv/infra/socket-core
    python3 scripts/alta_cliente.py [--core http://127.0.0.1:3005] [--env .env]

Solo librería estándar (sin dependencias).
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error


def resolver_env(ruta: str) -> str:
    """Busca el .env en la ruta dada, subiendo carpetas y en la ruta estándar del VPS."""
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


def post_json(url: str, payload: dict, headers: dict) -> dict:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        try:
            body = json.loads(e.read().decode())
            print(f"\n[ERROR] HTTP {e.code}: {body.get('error', body)}")
        except Exception:
            print(f"\n[ERROR] HTTP {e.code} sin detalle.")
        sys.exit(1)


def preguntar(txt: str, defecto: str = "") -> str:
    suger = f" [{defecto}]" if defecto else ""
    r = input(f"{txt}{suger}: ").strip()
    return r if r else defecto


def main() -> None:
    ap = argparse.ArgumentParser(description="Alta de cliente en socket-core")
    ap.add_argument("--core", default="http://127.0.0.1:3005")
    ap.add_argument("--env", default=".env")
    args = ap.parse_args()

    env_path = resolver_env(args.env)
    master = leer_master_key(env_path)
    if not master:
        print(f"[ERROR] No encontre MASTER_KEY en {env_path}. Corre desde /srv/infra/socket-core.")
        sys.exit(1)

    print("== Alta de proyecto cliente ===")
    name = preguntar("Nombre del proyecto (ej: monitoreo)")
    while not name:
        print("   el nombre es obligatorio.")
        name = preguntar("Nombre del proyecto (ej: monitoreo)")
    domains = preguntar("Dominios permitidos coma-separados (ej: https://panel.midominio.com)", "*")
    prefix = preguntar("Prefijo de canales", f"{name}:*")
    scopes = preguntar("Scopes coma-separados", "publish,subscribe,manage")
    rate = preguntar("Rate limit por minuto", "120")
    exp = preguntar("Vigencia en días (vacío = sin expiración)", "")

    payload = {
        "name": name,
        "domains": [d.strip() for d in domains.split(",") if d.strip()],
        "channel_prefix": prefix or "*",
        "scopes": [s.strip() for s in scopes.split(",") if s.strip()],
        "rate_limit_per_min": int(rate or 120),
    }
    if exp.strip():
        payload["expires_in_days"] = int(exp)

    print("\nSe creará esta key:")
    print(json.dumps(payload, indent=2, ensure_ascii=False))
    if preguntar("¿Confirmar? (s/N)", "N").lower() not in ("s", "si", "sí", "y", "yes"):
        print("Cancelado.")
        return

    res = post_json(
        f"{args.core}/admin/keys",
        payload,
        {"Content-Type": "application/json", "X-Master-Key": master},
    )
    d = res["data"]

    print("\n" + "=" * 60)
    print("[OK] CREDENCIALES — copiar al backend del proyecto (se muestran UNA vez)")
    print("=" * 60)
    print(f"Proyecto:        {d['name']}")
    print(f"Key ID:          {d['id']}   (para revocar/borrar)")
    print(f"API Key:         {d['api_key']}")
    print(f"Dominios:        {', '.join(d['domains'])}")
    print(f"Prefijo canales: {d['channel_prefix']}")
    print(f"Scopes:          {', '.join(d['scopes'])}")
    print("-" * 60)
    print("Ejemplo: pedir JWT desde el backend")
    print(f"  curl -X POST {args.core}/auth/token \\")
    print(f"    -H \"X-Api-Key: {d['api_key']}\" -H \"Origin: {d['domains'][0]}\" \\")
    print("    -H 'Content-Type: application/json' \\")
    print(f"    -d '{{\"user_id\":\"user-1\",\"channel\":\"{d['channel_prefix'].replace(':*', ':general') if ':*' in d['channel_prefix'] else d['channel_prefix']}\",\"role\":\"both\"}}'")
    print("-" * 60)
    print("Revocar:  POST /admin/keys/" + d["id"] + "/revoke  (con X-Master-Key)")
    print("Borrar:   DELETE /admin/keys/" + d["id"] + "  (con X-Master-Key)")
    print("=" * 60)


if __name__ == "__main__":
    main()
