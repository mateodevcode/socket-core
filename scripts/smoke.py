#!/usr/bin/env python3
"""Prueba de humo post-despliegue de socket-core.

Crea una key temporal, un canal, emite un evento, comprueba que el canal existe
y borra la key (en `finally`, pase lo que pase). Todas las credenciales
(MASTER_KEY y API key) van en la configuración de curl por **stdin** (`-K -`);
nunca aparecen en argv ni en la línea de comandos.

Uso (en el VPS, tras desplegar):
    SOCKET_MASTER_KEY=... SOCKET_PUBLIC_URL=https://hub.example python3 scripts/smoke.py

Variables:
    SOCKET_MASTER_KEY   obligatoria (no se acepta por argv)
    SOCKET_PUBLIC_URL   obligatoria (o --public-url)
    SOCKET_ADMIN_PORT   opcional, por defecto 3015 (loopback del contenedor)

Solo librería estándar.
"""

import argparse
import json
import os
import subprocess
import sys
import time

DEFAULT_ADMIN_PORT = "3015"


class SmokeError(Exception):
    pass


def _cfg(value: str) -> str:
    out = str(value).replace("\\", "\\\\").replace('"', '\\"')
    out = out.replace("\n", "\n").replace("\r", "\r")
    return '"' + out + '"'


def _request(container: bool, method: str, url: str, headers=None, payload=None, timeout=10):
    """curl -K - con TODAS las credenciales/cabeceras/cuerpo por stdin."""
    cfg = [
        "url = " + _cfg(url),
        "request = " + _cfg(method),
        "silent",
        "show-error",
        "max-time = " + str(int(timeout)),
        'write-out = "\n%{http_code}"',
    ]
    for k, v in (headers or {}).items():
        cfg.append("header = " + _cfg(f"{k}: {v}"))
    if payload is not None:
        cfg.append("header = " + _cfg("Content-Type: application/json"))
        cfg.append("data = " + _cfg(json.dumps(payload)))
    argv = (
        ["docker", "exec", "-i", "socket-core", "curl", "-sS", "-K", "-"]
        if container
        else ["curl", "-sS", "-K", "-"]
    )
    r = subprocess.run(
        argv,
        input="\n".join(cfg) + "\n",
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout + 5,
        shell=False,
    )
    if r.returncode != 0:
        raise SmokeError(r.stderr.strip() or f"curl exit {r.returncode}")
    cuerpo, _, codigo = (r.stdout or "").rpartition("\n")
    try:
        status = int(codigo.strip() or "0")
    except ValueError:
        status = 0
    try:
        data = json.loads(cuerpo) if cuerpo.strip() else {}
    except json.JSONDecodeError:
        data = {"_raw": cuerpo}
    return status, data


def _data(resp):
    if isinstance(resp, dict) and isinstance(resp.get("data"), dict):
        return resp["data"]
    return resp if isinstance(resp, dict) else {}


def run(public_url: str, master: str, admin_port: str = DEFAULT_ADMIN_PORT) -> int:
    public_url = public_url.rstrip("/")
    if not public_url:
        raise SmokeError("SOCKET_PUBLIC_URL / --public-url es obligatorio")
    if not master:
        raise SmokeError("SOCKET_MASTER_KEY es obligatorio (solo por entorno)")

    admin = f"http://127.0.0.1:{admin_port}"
    stamp = f"{int(time.time())}-{os.getpid()}"
    name = f"smoke-{stamp}"
    prefix = f"{name}:*"
    canal = f"{name}:probe"
    key_id = None
    api_key = None

    try:
        print(f"-> creando key temporal {name}")
        st, body = _request(
            True,
            "POST",
            f"{admin}/admin/keys",
            {"X-Master-Key": master},
            {
                "name": name,
                "channel_prefix": prefix,
                "domains": ["*"],
                "scopes": ["publish", "subscribe", "manage"],
                "rate_limit_per_min": 600,
            },
        )
        d = _data(body)
        key_id, api_key = d.get("id"), d.get("api_key")
        if st != 200 or not key_id or not api_key:
            raise SmokeError(f"crear key: HTTP {st} {body}")

        print(f"-> creando canal {canal}")
        st, body = _request(
            False,
            "POST",
            f"{public_url}/channels",
            {"X-Api-Key": api_key},
            {"name": canal, "description": "smoke test temporal"},
        )
        if st != 201:
            raise SmokeError(f"crear canal: HTTP {st} {body}")

        print(f"-> emitiendo evento en {canal}")
        st, body = _request(
            False,
            "POST",
            f"{public_url}/channels/{canal}/events",
            {"X-Api-Key": api_key},
            {"source": "smoke", "targets": ["*"], "payload": {"type": "smoke", "ts": stamp}},
        )
        if st != 200 or not _data(body).get("success", True):
            raise SmokeError(f"emitir: HTTP {st} {body}")

        print("-> comprobando que el canal existe y es visible")
        st, body = _request(False, "GET", f"{public_url}/channels/{canal}", {"X-Api-Key": api_key})
        if st != 200 or _data(body).get("name") != canal:
            raise SmokeError(f"comprobar canal: HTTP {st} {body}")

        print("[OK] smoke: key + canal + evento + lectura")
        return 0
    finally:
        if key_id:
            print(f"-> borrando key temporal {name} (cascada de canales)")
            try:
                st, body = _request(
                    True, "DELETE", f"{admin}/admin/keys/{key_id}", {"X-Master-Key": master}
                )
                if st not in (200, 204):
                    print(f"[AVISO] borrar key: HTTP {st} {body}", file=sys.stderr)
            except Exception as e:
                print(f"[AVISO] no se pudo borrar la key {key_id}: {e}", file=sys.stderr)


def main() -> int:
    ap = argparse.ArgumentParser(description="Prueba de humo post-despliegue (credenciales por stdin)")
    ap.add_argument("--public-url", default=os.environ.get("SOCKET_PUBLIC_URL", ""))
    ap.add_argument("--admin-port", default=os.environ.get("SOCKET_ADMIN_PORT", DEFAULT_ADMIN_PORT))
    args = ap.parse_args()
    master = os.environ.get("SOCKET_MASTER_KEY", "")
    if not master:
        print("[ERROR] Falta SOCKET_MASTER_KEY en el entorno.", file=sys.stderr)
        return 2
    try:
        return run(args.public_url, master, args.admin_port)
    except SmokeError as e:
        print(f"[FALLO] {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
