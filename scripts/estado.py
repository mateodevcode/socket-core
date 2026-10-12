#!/usr/bin/env python3
"""Estado completo del hub socket-core.

Muestra: contenedor, salud, stats, canales con conectados, borde Nginx
(certificado), fail2ban y un veredicto final. Exit 0 = todo bien, 1 = alertas.

Uso en el VPS:
    python3 scripts/estado.py
    # necesita una key ops: SOCKET_OPS_KEY, --key, o archivo .ops_key
    # crearla una vez: ver README § "monitoreo" o scripts/alta_cliente.py (nombre ops-monitoreo)

Uso parcial (solo core público, sirve desde tu PC):
    SOCKET_OPS_KEY=sc_live_... python3 scripts/estado.py --solo-core --core https://ws.seventwo.tech

Solo librería estándar.
"""

import argparse
import datetime
import json
import os
import ssl
import subprocess
import sys
import urllib.request
import urllib.error

VERDE, ROJO, AMAR = "\033[32m", "\033[31m", "\033[33m"
RESET = "\033[0m" if sys.stdout.isatty() else ""


def pintar(ok: bool, nombre: str, detalle: str = "") -> bool:
    print(f"[{'OK' if ok else 'FALLO'}] {nombre}" + (f" -> {detalle}" if detalle else ""))
    return ok


def leer_key(args) -> str:
    if args.key:
        return args.key
    if os.environ.get("SOCKET_OPS_KEY"):
        return os.environ["SOCKET_OPS_KEY"]
    for c in (".ops_key", "../.ops_key", "/srv/infra/socket-core/.ops_key"):
        if os.path.isfile(c):
            with open(c, encoding="utf-8") as f:
                v = f.read().strip()
                if v:
                    return v
    return ""


def get_json(url: str, key: str = "", timeout=10):
    req = urllib.request.Request(url, headers={"X-Api-Key": key} if key else {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode())
        except Exception:
            return e.code, {}
    except Exception as e:
        return 0, {"_error": str(e)}


def sh(cmd: str):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=15)
    except Exception as e:
        return None


def main() -> int:
    ap = argparse.ArgumentParser(description="Estado del hub socket-core")
    ap.add_argument("--core", default="https://ws.seventwo.tech")
    ap.add_argument("--key", default="")
    ap.add_argument("--solo-core", action="store_true", help="solo chequeos del core (sin docker/nginx/fail2ban)")
    args = ap.parse_args()

    key = leer_key(args)
    alertas = []

    print(f"== socket-core @ {args.core} ==")

    # 1. Contenedor + salud local
    if not args.solo_core:
        r = sh("docker inspect -f '{{.State.Status}}|{{.State.StartedAt}}' socket-core 2>/dev/null")
        if r and r.returncode == 0 and r.stdout.strip().startswith("running"):
            pintar(True, "contenedor socket-core", r.stdout.strip().split("|")[0])
        else:
            pintar(False, "contenedor socket-core", "no está running")
            alertas.append("contenedor caído")
        s, b = get_json("http://127.0.0.1:3005/health")
        if s == 200:
            pintar(True, "health local :3005")
        else:
            pintar(False, "health local :3005", str(b.get("_error", b)))
            alertas.append("health local falla")

    # 2. Salud pública + stats
    s, b = get_json(f"{args.core}/health")
    if s == 200:
        pintar(True, "health público")
    else:
        pintar(False, "health público", str(b.get("_error", b)))
        alertas.append("health público falla")

    if not key:
        print("[AVISO] Sin key ops: define SOCKET_OPS_KEY, --key o archivo .ops_key para ver stats y canales.")
        print("  Crear una: python3 scripts/alta_cliente.py  (nombre sugerido: ops-monitoreo, prefijo *)")
    else:
        s, b = get_json(f"{args.core}/stats", key)
        if s == 200:
            d = b["data"]
            pintar(True, "stats", f"{d['channels']} canales, {d['total_clients']} conectados")
        else:
            pintar(False, "stats", b.get("error", f"HTTP {s}"))
            alertas.append("stats con error")

        # 3. Canales + conectados
        s, b = get_json(f"{args.core}/channels", key)
        if s == 200:
            chans = b["data"]
            print(f"\n-- canales ({len(chans)}) --")
            for c in sorted(chans, key=lambda x: x["name"]):
                s2, b2 = get_json(f"{args.core}/channels/{c['id']}/clients", key)
                nombres = ",".join(x["id"] for x in b2.get("data", [])) if s2 == 200 else "?"
                print(f"  {c['name']:40} conectados={c['clients_count']:<4} [{nombres}]")
        else:
            pintar(False, "lista de canales", b.get("error", f"HTTP {s}"))
            alertas.append("canales con error")

    # 4. Borde: nginx + cert + fail2ban
    if not args.solo_core:
        print("")
        r = sh("systemctl is-active nginx 2>/dev/null || service nginx status 2>/dev/null | head -1")
        if r and ("active" in (r.stdout or "") or r.returncode == 0):
            pintar(True, "nginx activo")
        else:
            pintar(False, "nginx", (r.stdout or r.stderr or "").strip()[:80])
            alertas.append("nginx no activo")

        host = args.core.split("://", 1)[-1].split("/", 1)[0].split(":")[0]
        if args.core.startswith("https"):
            # Lo que el mundo ve: handshake TLS con SNI (sin necesitar permisos root).
            try:
                ctx = ssl.create_default_context()
                with ctx.wrap_socket(
                    __import__("socket").create_connection((host, 443), timeout=10),
                    server_hostname=host,
                ) as s:
                    cert = s.getpeercert()
                exp = datetime.datetime.strptime(cert["notAfter"], "%b %d %H:%M:%S %Y %Z").replace(
                    tzinfo=datetime.timezone.utc
                )
                dias = (exp - datetime.datetime.now(datetime.timezone.utc)).days
                if dias > 15:
                    pintar(True, f"cert {host}", f"vence en {dias} días")
                else:
                    pintar(False, f"cert {host}", f"vence en {dias} días")
                    alertas.append("cert por vencer")
            except Exception as e:
                pintar(False, f"cert {host}", str(e)[:100])
                alertas.append("cert sin verificar")
        else:
            pintar(True, "cert (core sin TLS local, omitido)")

        r = sh("sudo -n fail2ban-client status socketcore-auth 2>&1 | grep -E 'Currently (failed|banned)'")
        if r and r.returncode == 0 and r.stdout.strip():
            pintar(True, "fail2ban socketcore-auth", r.stdout.strip().replace("\n", " ").replace("\t", " "))
        else:
            pintar(False, "fail2ban socketcore-auth", "sin datos (¿sudo sin password?)")
            alertas.append("fail2ban sin verificar")

    print("")
    if alertas:
        print(f"{ROJO}ALERTAS ({len(alertas)}): {', '.join(alertas)}{RESET}")
        return 1
    print(f"{VERDE}TODO BIEN{RESET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
