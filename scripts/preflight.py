#!/usr/bin/env python3
"""Preflight de socket-core: SOLO LEE y avisa. No modifica nada.

- Keys y sus metadatos: vía listener admin (MASTER_KEY del operador), directo o
  `docker exec -i socket-core curl ...`.
- Canales/huérfanos/cascada: sobre una COPIA de la SQLite (--db o `docker cp`).
  Sin copia, el análisis de canales se marca como no disponible.

Uso:
    SOCKET_MASTER_KEY=... python3 scripts/preflight.py
    python3 scripts/preflight.py --db /tmp/copia.db --admin http://127.0.0.1:3015
    # Sin admin ni MASTER_KEY: solo la copia SQLite (migración/preflight offline)
    python3 scripts/preflight.py --db /tmp/copia.db --db-only

Exit != 0 si hay problemas bloqueantes (scopes inválidos, prefijos solapados,
keys expiradas, canales huérfanos). Los avisos (expiración muy lejana, sin DB)
no bloquean.
"""

import argparse
import datetime
import ipaddress
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

SCOPES_VALIDOS = {"publish", "subscribe", "manage"}
MAX_DIAS_VIGENCIA = 3650


# ------------------------- lógica pura (testeable) -------------------------

def _bases(prefixes):
    out = []
    for p in prefixes or []:
        for tok in str(p).split(","):
            tok = tok.strip()
            if not tok:
                continue
            out.append(tok[:-2] if tok.endswith(":*") else tok)
    return out


def _overlap(a, b):
    if a == "*" or b == "*":
        return True
    if a == b:
        return True
    return b.startswith(a + ":") or a.startswith(b + ":")


def prefijos_solapados(keys):
    pares = []
    for i in range(len(keys)):
        for j in range(i + 1, len(keys)):
            ai, bj = _bases(keys[i].get("channel_prefixes")), _bases(keys[j].get("channel_prefixes"))
            if any(_overlap(x, y) for x in ai for y in bj):
                pares.append((keys[i].get("name"), keys[j].get("name")))
    return pares


def trusted_proxies_problemas(trusted, candidatas):
    """Comprueba TRUSTED_PROXIES contra las IPs candidatas (gateway/IPs de la red)."""
    entradas = [x.strip() for x in str(trusted or "").split(",") if x.strip()]
    problemas = []
    if not entradas:
        return ["TRUSTED_PROXIES vacío: se ignorará X-Forwarded-For"]
    redes = []
    for e in entradas:
        try:
            net = ipaddress.ip_network(e, strict=False)
        except ValueError:
            problemas.append(f"TRUSTED_PROXIES inválido: {e}")
            continue
        tope = 16 if net.version == 4 else 48
        if net.prefixlen <= tope:
            problemas.append(f"TRUSTED_PROXIES con subred demasiado ancha: {e}")
        redes.append(net)
    if candidatas:
        incluye = any(
            ipaddress.ip_address(str(ip)) in net for ip in candidatas for net in redes
        )
        if not incluye:
            problemas.append(
                "TRUSTED_PROXIES no incluye el gateway/IP de central_network: "
                + ", ".join(str(c) for c in candidatas)
            )
    return problemas


def analizar(keys, canales, ahora=None):
    """keys: lista de dicts del admin; canales: lista de dicts {id,name,project_id}."""
    if ahora is None:
        ahora = datetime.datetime.now(datetime.timezone.utc).timestamp()
    problemas, avisos = [], []
    por_proyecto_canales = {}
    by_id = {k.get("id"): k for k in keys}

    for k in keys:
        nombre = k.get("name", "?")
        scopes = k.get("scopes") or []
        invalidos = [s for s in scopes if s not in SCOPES_VALIDOS]
        duplicados = {s for s in scopes if scopes.count(s) > 1}
        if invalidos:
            problemas.append(f"key '{nombre}': scopes inválidos {invalidos}")
        if duplicados:
            problemas.append(f"key '{nombre}': scopes duplicados {sorted(duplicados)}")
        exp = k.get("expires_at")
        if exp is not None:
            if exp < ahora:
                problemas.append(f"key '{nombre}': expirada ({_fecha(exp)})")
            elif exp > ahora + MAX_DIAS_VIGENCIA * 86400:
                avisos.append(f"key '{nombre}': expira muy lejos ({_fecha(exp)})")
        por_proyecto_canales[nombre] = 0

    for (a, b) in prefijos_solapados(keys):
        problemas.append(f"prefijos solapados entre '{a}' y '{b}'")

    huerfanos = []
    cascada = {k.get("name", "?"): 0 for k in keys}
    for c in canales or []:
        pid = c.get("project_id") or ""
        key = by_id.get(pid)
        if not pid or key is None:
            huerfanos.append(c.get("name") or c.get("id"))
        else:
            por_proyecto_canales[key.get("name", "?")] += 1
            cascada[key.get("name", "?")] += 1

    if huerfanos:
        problemas.append(f"canales huérfanos (invisibles): {huerfanos}")

    return {
        "problemas": problemas,
        "avisos": avisos,
        "canales_por_proyecto": por_proyecto_canales,
        "cascada_por_key": cascada,
        "huerfanos": huerfanos,
        "db_disponible": canales is not None,
    }


def leer_keys_db(db_path):
    """Keys desde una copia SQLite (sin admin/MASTER_KEY), para --db-only."""
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT id, name, channel_prefix, scopes, expires_at, revoked FROM api_keys"
        ).fetchall()
    finally:
        con.close()
    keys = []
    for r in rows:
        prefixes = [x.strip() for x in str(r[2] or "").split(",") if x.strip()]
        scopes = [x.strip() for x in str(r[3] or "").split(",") if x.strip()]
        keys.append(
            {
                "id": r[0],
                "name": r[1],
                "channel_prefixes": prefixes,
                "scopes": scopes,
                "expires_at": r[4],
                "revoked": bool(r[5]),
            }
        )
    return keys


def leer_canales(db_path):
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute("SELECT id, name, project_id FROM channels").fetchall()
        return [{"id": r[0], "name": r[1], "project_id": r[2] or ""} for r in rows]
    finally:
        con.close()


def _fecha(ts):
    return datetime.datetime.fromtimestamp(ts, datetime.timezone.utc).strftime("%Y-%m-%d")


# ------------------------- acceso admin (solo lectura) -------------------------

def _get_json(url, master, timeout=10):
    req = urllib.request.Request(url, headers={"X-Master-Key": master})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def _docker_exec_get(admin, master):
    port = admin.rstrip("/").rsplit(":", 1)[-1] or "3015"
    r = subprocess.run(
        [
            "docker", "exec", "-i", "socket-core",
            "curl", "-sS", f"http://127.0.0.1:{port}/admin/keys",
            "-H", f"X-Master-Key: {master}",
        ],
        capture_output=True,
        timeout=20,
    )
    if r.returncode != 0:
        raise RuntimeError(r.stderr.decode(errors="replace").strip())
    return json.loads(r.stdout.decode())


def obtener_keys(args, master):
    url = f"{args.admin.rstrip('/')}/admin/keys"
    try:
        return _get_json(url, master), "directo"
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"admin HTTP {e.code}") from e
    except Exception:
        return _docker_exec_get(args.admin, master), "docker exec"


def copiar_db(args):
    if args.db:
        return args.db
    tmp = os.path.join(tempfile.gettempdir(), "socket-core-preflight.db")
    try:
        r = subprocess.run(
            ["docker", "cp", "socket-core:/app/data/socket-core.db", tmp],
            capture_output=True,
            timeout=30,
        )
        if r.returncode != 0:
            return None
        return tmp
    except Exception:
        return None


def main():
    ap = argparse.ArgumentParser(description="Preflight de socket-core (solo lectura)")
    ap.add_argument("--admin", default="http://127.0.0.1:3015")
    ap.add_argument("--master-key", default=os.environ.get("SOCKET_MASTER_KEY", ""))
    ap.add_argument("--db", default="", help="copia local de la SQLite (recomendado)")
    ap.add_argument(
        "--db-only",
        action="store_true",
        help="lee keys y canales de --db (sin listener admin ni MASTER_KEY)",
    )
    ap.add_argument(
        "--trusted-proxies",
        default=os.environ.get("TRUSTED_PROXIES", ""),
        help="valor de TRUSTED_PROXIES (por defecto, el del entorno)",
    )
    args = ap.parse_args()

    if args.db_only and not args.db:
        print("[ERROR] --db-only requiere --db (copia SQLite).")
        return 2
    if not args.db_only and not args.master_key:
        print("[ERROR] Falta SOCKET_MASTER_KEY (o --master-key).")
        return 2

    if args.db_only:
        try:
            keys = leer_keys_db(args.db)
        except Exception as e:
            print(f"[ERROR] No se pudieron leer keys de {args.db}: {e}")
            return 2
        via = "db-only"
    else:
        try:
            keys, via = obtener_keys(args, args.master_key)
        except Exception as e:
            print(f"[ERROR] No se pudo leer /admin/keys: {e}")
            return 2
    print(f"Keys leídas: {len(keys)} (vía {via})")

    db = copiar_db(args)
    canales = None
    if db:
        try:
            canales = leer_canales(db)
            print(f"Canales leídos de copia SQLite: {len(canales)} ({db})")
        except Exception as e:
            print(f"[AVISO] No se pudo leer la copia SQLite: {e}")
    else:
        print("[AVISO] Sin copia de SQLite: no se pueden analizar canales/huérfanos/cascada.")

    rep = analizar(keys, canales)

    # R9: TRUSTED_PROXIES vs gateway real (si Docker está disponible).
    candidatas = []
    try:
        gw = subprocess.run(
            ["docker", "network", "inspect", "central_network",
             "--format", "{{range .IPAM.Config}}{{.Gateway}} {{end}}"],
            capture_output=True, text=True, timeout=10,
        )
        red = subprocess.run(
            ["docker", "network", "inspect", "central_network",
             "--format", "{{range .Containers}}{{.IPv4Address}} {{end}}"],
            capture_output=True, text=True, timeout=10,
        )
        candidatas = [x.split("/")[0] for x in (gw.stdout + " " + red.stdout).split() if x.strip()]
    except Exception:
        pass
    tp_problemas = trusted_proxies_problemas(args.trusted_proxies, candidatas)
    rep["problemas"].extend(tp_problemas)

    print("\n-- canales por proyecto --")
    for nombre, n in sorted(rep["canales_por_proyecto"].items()):
        print(f"  {nombre:30} canales={n}")
    print("\n-- si borras cada key, canales en cascada --")
    for nombre, n in sorted(rep["cascada_por_key"].items()):
        print(f"  {nombre:30} se borrarían={n}")
    if not rep["db_disponible"]:
        print("  (no disponible: sin copia de SQLite)")

    if rep["problemas"]:
        print(f"\n[BLOQUEANTE] {len(rep['problemas'])}:")
        for p in rep["problemas"]:
            print("  -", p)
    if rep["avisos"]:
        print(f"\n[AVISO] {len(rep['avisos'])}:")
        for a in rep["avisos"]:
            print("  -", a)
    if not rep["problemas"]:
        print("\n[OK] Sin problemas bloqueantes.")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
