#!/usr/bin/env python3
"""Estado completo del hub socket-core.

Muestra: contenedor, salud, estadísticas globales por proyecto (admin), borde
Nginx (certificado), fail2ban y un veredicto final. Exit 0 = todo bien, 1 = alertas.

Las estadísticas salen de GET /admin/stats del listener ADMIN (loopback), con
MASTER_KEY. Ya NO hace falta una API key con prefijo "*".

Uso en el VPS:
    SOCKET_MASTER_KEY=... python3 scripts/estado.py
    # opcional: --admin http://127.0.0.1:3015 --master-key ...
    # si el admin no es alcanzable desde el host, se intenta vía
    #   docker exec socket-core curl -s http://127.0.0.1:3015/admin/stats

Uso parcial (solo core público, sirve desde tu PC):
    python3 scripts/estado.py --solo-core --core https://ws.seventwo.tech

Solo librería estándar.
"""

import argparse
import datetime
import ipaddress
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


def leer_master(args) -> str:
    if args.master_key:
        return args.master_key
    if os.environ.get("SOCKET_MASTER_KEY"):
        return os.environ["SOCKET_MASTER_KEY"]
    for c in (".master_key", "../.master_key", "/srv/infra/socket-core/.master_key"):
        if os.path.isfile(c):
            with open(c, encoding="utf-8") as f:
                v = f.read().strip()
                if v:
                    return v
    return ""


def get_json(url: str, headers: dict = None, timeout=10):
    req = urllib.request.Request(url, headers=headers or {})
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


def admin_stats(args, master: str):
    """GET /admin/stats directo; si no se alcanza, vía docker exec (loopback)."""
    url = f"{args.admin}/admin/stats"
    s, b = get_json(url, {"X-Master-Key": master})
    if s == 200:
        return s, b, "directo"
    # El listener admin escucha en loopback del contenedor: probar docker exec.
    port = args.admin.rstrip("/").rsplit(":", 1)[-1] or "3015"
    cmd = (
        "docker exec socket-core curl -s --max-time 5 "
        f"-H 'X-Master-Key: {master}' http://127.0.0.1:{port}/admin/stats"
    )
    r = sh(cmd)
    if r and r.returncode == 0 and (r.stdout or "").strip():
        try:
            return 200, json.loads(r.stdout), "docker exec"
        except Exception:
            pass
    return s, b, "directo"


def sh(cmd: str):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=15)
    except Exception:
        return None


def red_gateway() -> str:
    r = sh("docker network inspect central_network --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'")
    return (r.stdout or "").strip() if r and r.returncode == 0 else ""


def red_containers() -> list:
    """IPs de los contenedores conectados a central_network."""
    r = sh("docker network inspect central_network --format '{{range .Containers}}{{.IPv4Address}} {{end}}'")
    out = (r.stdout or "") if r and r.returncode == 0 else ""
    return [x.split("/")[0].strip() for x in out.split() if x.strip()]


def trusted_proxies_config() -> str:
    r = sh("docker inspect socket-core --format '{{range .Config.Env}}{{println .}}{{end}}'")
    for line in ((r.stdout or "") if r else "").splitlines():
        if line.startswith("TRUSTED_PROXIES="):
            return line.split("=", 1)[1].strip()
    return ""


def _subred_ancha(entrada: str) -> bool:
    try:
        net = ipaddress.ip_network(entrada.strip(), strict=False)
    except ValueError:
        return False
    tope = 16 if net.version == 4 else 48
    return net.prefixlen <= tope


def check_trusted_proxies() -> bool:
    """Comprueba gateway/IPs y que no se confíe una subred demasiado ancha."""
    trusted = trusted_proxies_config()
    gw = red_gateway()
    candidatas = ([gw] if gw else []) + red_containers()
    entradas = [t.strip() for t in trusted.split(",") if t.strip()]
    if not trusted:
        print(f"[AVISO] TRUSTED_PROXIES vacío: se ignora X-Forwarded-For y el rate limit "
              f"por IP verá el gateway {gw or '?'}. Si Nginx está delante, añade esa IP.")
        return False
    anchas = [e for e in entradas if _subred_ancha(e)]
    if anchas:
        pintar(False, "TRUSTED_PROXIES", f"subred demasiado ancha: {', '.join(anchas)}")
        return False
    for ip in candidatas:
        if ip and ip in entradas:
            pintar(True, "TRUSTED_PROXIES", f"contiene {ip}")
            return True
    print(f"[AVISO] TRUSTED_PROXIES={trusted!r} no incluye el gateway {gw or '?'} "
          f"ni IPs de central_network {candidatas}. Revisa: "
          f"docker network inspect central_network --format '{{{{range .IPAM.Config}}}}{{{{.Gateway}}}}{{{{end}}}}'")
    return False


def main() -> int:
    ap = argparse.ArgumentParser(description="Estado del hub socket-core")
    ap.add_argument("--core", default="https://ws.seventwo.tech")
    ap.add_argument("--admin", default="http://127.0.0.1:3015")
    ap.add_argument("--master-key", default="")
    ap.add_argument("--solo-core", action="store_true", help="solo chequeos del core (sin docker/nginx/fail2ban)")
    args = ap.parse_args()

    master = leer_master(args)
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

        if not check_trusted_proxies():
            alertas.append("TRUSTED_PROXIES sin configurar/incorrecto")

    # 2. Salud pública + stats
    s, b = get_json(f"{args.core}/health")
    if s == 200:
        pintar(True, "health público")
    else:
        pintar(False, "health público", str(b.get("_error", b)))
        alertas.append("health público falla")

    if not master:
        print("[AVISO] Sin MASTER_KEY: define SOCKET_MASTER_KEY o --master-key para /admin/stats.")
    else:
        s, b, via = admin_stats(args, master)
        if s == 200:
            proyectos = b["data"]["projects"]
            total_canales = sum(p.get("channels", 0) for p in proyectos)
            total_conn = sum(p.get("connections", 0) for p in proyectos)
            pintar(True, "admin/stats", f"{len(proyectos)} proyectos, {total_canales} canales, {total_conn} conexiones ({via})")
            print()
            print("-- por proyecto --")
            print(f"  {'proyecto':28} {'revocada':8} {'canales':8} {'conexiones':10} {'clientes':8}")
            for p in sorted(proyectos, key=lambda x: x.get("project_name", "")):
                print(
                    f"  {p.get('project_name','?'):28} {str(bool(p.get('revoked'))):8} "
                    f"{p.get('channels',0):<8} {p.get('connections',0):<10} {p.get('clients',0):<8}"
                )
        else:
            pintar(False, "admin/stats", b.get("error", f"HTTP {s}"))
            alertas.append("admin/stats con error")

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
