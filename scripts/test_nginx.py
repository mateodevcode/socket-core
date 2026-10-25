#!/usr/bin/env python3
"""Pruebas reales de la configuración de nginx/ en contenedores.

- nginx -t con certificados autofirmados de prueba.
- upstream falso que devuelve las cabeceras recibidas.
- /admin/x => 404, ruta desconocida => 404, /ws conserva Upgrade y XFF real,
  y se supera limit_conn => 429.

Requiere Docker y openssl. Se ejecuta en CI (job nginx).
"""

import concurrent.futures
import os
import re
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NET = "sc-nginx-net"
UP = "sc-nginx-upstream"
NG = "sc-nginx-under-test"
PORT_HTTP = 18080
PORT_HTTPS = 18443

UPSTREAM_PY = r"""
from http.server import BaseHTTPRequestHandler, HTTPServer
import json, time
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if "slow" in self.path:
            time.sleep(2)
        body = json.dumps({"path": self.path, "headers": dict(self.headers)}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
HTTPServer(("0.0.0.0", 9000), H).serve_forever()
"""


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", **kw)


def must(cmd, **kw):
    r = run(cmd, **kw)
    if r.returncode != 0:
        raise SystemExit(f"[FALLO] {' '.join(map(str, cmd))}\n{r.stdout}\n{r.stderr}")
    return r


def curl(url, headers=None):
    cmd = ["curl", "-sk", "--max-time", "10", "-o", "-", "-w", "\n%{http_code}", url]
    for h in headers or []:
        cmd[1:1] = ["-H", h]
    r = run(cmd)
    body, _, code = r.stdout.rpartition("\n")
    return int(code or 0), body


def main():
    if run(["docker", "version"]).returncode != 0:
        print("[FALLO] Docker no disponible")
        return 1
    tmp = Path(tempfile.mkdtemp(prefix="sc-nginx-"))
    certs = tmp / "certs"
    live = certs / "live" / "ws.seventwo.tech"
    live.mkdir(parents=True)
    try:
        # Certificado autofirmado + dhparam rápido (solo pruebas).
        must(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
              "-keyout", str(live / "privkey.pem"), "-out", str(live / "fullchain.pem"),
              "-days", "2", "-subj", "/CN=ws.seventwo.tech",
              "-addext", "subjectAltName=DNS:ws.seventwo.tech"])
        must(["openssl", "dhparam", "-dsaparam", "-out", str(certs / "ssl-dhparams.pem"), "2048"])
        (certs / "options-ssl-nginx.conf").write_text("ssl_protocols TLSv1.2 TLSv1.3;\n")

        (tmp / "upstream.py").write_text(UPSTREAM_PY, encoding="utf-8")
        (tmp / "ws-http.conf").write_text((ROOT / "nginx/ws.seventwo.tech.http.conf").read_text(), encoding="utf-8")
        https_validate = (ROOT / "nginx/ws.seventwo.tech.https.conf").read_text()
        (tmp / "ws-https-validate.conf").write_text(https_validate, encoding="utf-8")
        https_runtime = https_validate.replace("http://127.0.0.1:3005", "http://upstream:9000")
        (tmp / "ws-https.conf").write_text(https_runtime, encoding="utf-8")

        def mounts(https_source):
            return [
                "-v", f"{certs}:/etc/letsencrypt:ro",
                "-v", f"{ROOT / 'nginx/conf.d/socketcore-limits.conf'}:/etc/nginx/conf.d/socketcore-limits.conf:ro",
                "-v", f"{tmp / 'ws-http.conf'}:/etc/nginx/conf.d/ws-http.conf:ro",
                "-v", f"{https_source}:/etc/nginx/conf.d/ws-https.conf:ro",
            ]

        # 1) nginx -t con la configuración real (certs de prueba).
        r = run(["docker", "run", "--rm", *mounts(tmp / "ws-https-validate.conf"), "nginx:1.27-alpine", "nginx", "-t"])
        if r.returncode != 0:
            print("[FALLO] nginx -t\n", r.stdout, r.stderr)
            return 1
        print("OK nginx -t")

        run(["docker", "rm", "-f", UP, NG])
        run(["docker", "network", "rm", NET])
        must(["docker", "network", "create", NET])
        must(["docker", "run", "-d", "--name", UP, "--network", NET,
              "--network-alias", "upstream",
              "-v", f"{tmp / 'upstream.py'}:/upstream.py:ro",
              "python:3.12-slim", "python", "/upstream.py"])
        must(["docker", "run", "-d", "--name", NG, "--network", NET,
              "-p", f"127.0.0.1:{PORT_HTTP}:80", "-p", f"127.0.0.1:{PORT_HTTPS}:443",
              *mounts(tmp / "ws-https.conf"), "nginx:1.27-alpine"])
        url = f"https://127.0.0.1:{PORT_HTTPS}"
        host = "Host: ws.seventwo.tech"
        # espera
        ok = False
        for _ in range(30):
            code, _ = curl(url + "/health", [host])
            if code == 200:
                ok = True
                break
            time.sleep(1)
        if not ok:
            print("[FALLO] nginx no arrancó\n", run(["docker", "logs", NG]).stdout[-3000:])
            return 1

        fallos = []

        code, _ = curl(url + "/admin/x", [host])
        if code != 404:
            fallos.append(f"/admin/x => {code} (esperado 404)")

        code, _ = curl(url + "/otra-ruta", [host])
        if code != 404:
            fallos.append(f"/otra-ruta => {code} (esperado 404)")

        code, body = curl(url + "/ws", [host, "Upgrade: websocket", "Connection: Upgrade",
                                        "X-Forwarded-For: 1.2.3.4"])
        if code != 200:
            fallos.append(f"/ws => {code} (esperado 200 del upstream falso)")
        else:
            if "Upgrade" not in body and "upgrade" not in body:
                fallos.append("/ws no llegó con la cabecera Upgrade")
            if "1.2.3.4" in body:
                fallos.append("X-Forwarded-For falsificado llegó al upstream (no se sobrescribió)")
            if "X-Forwarded-For" not in body:
                fallos.append("no llegó X-Forwarded-For al upstream")
        if not fallos:
            print("OK rutas y cabeceras")

        # limit_conn: 40 peticiones lentas concurrentes; limite 30 => algún 429.
        ctx = ssl._create_unverified_context()

        def slow_req(_):
            req = urllib.request.Request(url + "/ws?slow=1", headers={"Host": "ws.seventwo.tech"})
            try:
                with urllib.request.urlopen(req, timeout=15, context=ctx) as r:
                    return r.status
            except urllib.error.HTTPError as e:
                return e.code
            except Exception:
                return 0

        with concurrent.futures.ThreadPoolExecutor(max_workers=40) as ex:
            codes = list(ex.map(slow_req, range(40)))
        n429 = sum(1 for c in codes if c == 429)
        if n429 == 0:
            fallos.append(f"limit_conn no devolvió 429 (códigos={sorted(set(codes))})")
        else:
            print(f"OK limit_conn: {n429} x429")

        if fallos:
            print("[FALLO]")
            for f in fallos:
                print(" -", f)
            return 1
        print("[OK] nginx")
        return 0
    finally:
        run(["docker", "rm", "-f", UP, NG])
        run(["docker", "network", "rm", NET])
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
