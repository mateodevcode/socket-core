#!/usr/bin/env python3
"""Ejecuta los bloques ```bash-doc-exec de docs/COMO-FUNCIONA.md contra un hub
temporal real, para que la documentación no se desincronice del comportamiento.

Uso (desde la raíz del repo):
    python3 scripts/test_docs_curl.py

Requiere: bash y curl en PATH. Compila el binario si falta.
"""

import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "COMO-FUNCIONA.md"
MASTER = "doc-test-master-key-32-chars-min!!"
JWT_SECRET = "doc-test-jwt-secret-32-chars-min!!!!"


def find_bash() -> str:
    candidatos = [
        os.environ.get("BASH"),
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ]
    for c in candidatos:
        if c and Path(c).is_file():
            return c
    w = shutil.which("bash")
    if w and "system32" not in w.lower():
        return w
    return w or "bash"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_health(url: str, timeout: float = 15.0) -> bool:
    fin = time.time() + timeout
    while time.time() < fin:
        try:
            with urllib.request.urlopen(url, timeout=1) as r:
                if r.status == 200:
                    return True
        except Exception:
            pass
        time.sleep(0.15)
    return False


def main() -> int:
    if not DOC.is_file():
        print(f"[FALLO] no existe {DOC}")
        return 1

    texto = DOC.read_text(encoding="utf-8")
    bloques = re.findall(r"```bash-doc-exec\n(.*?)```", texto, re.S)
    if not bloques:
        print("[FALLO] no hay bloques ```bash-doc-exec en la documentación")
        return 1
    print(f"Bloques ejecutables encontrados: {len(bloques)}")

    script = "set -euo pipefail\n" + "\n".join(bloques)

    binario = ROOT / "target" / "debug" / ("socket-core.exe" if os.name == "nt" else "socket-core")
    if not binario.is_file():
        print("Compilando binario (cargo build)...")
        subprocess.run(["cargo", "build"], cwd=ROOT, check=True)

    tmp = Path(tempfile.mkdtemp(prefix="sc-doc-"))
    port, admin_port = free_port(), free_port()
    core = f"http://127.0.0.1:{port}"
    admin = f"http://127.0.0.1:{admin_port}"
    env_srv = {
        **os.environ,
        "APP_ENV": "dev",
        "JWT_SECRET": JWT_SECRET,
        "MASTER_KEY": MASTER,
        "JWT_TTL_SECS": "600",
        "PORT": str(port),
        "ADMIN_PORT": str(admin_port),
        "DATABASE_URL": f"sqlite:{tmp / 'hub.db'}?mode=rwc",
        "ALLOW_BOOTSTRAP": "false",
        "RUST_LOG": "warn",
    }
    proc = subprocess.Popen(
        [str(binario)],
        env=env_srv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    ok = False
    try:
        if not wait_health(f"{core}/health"):
            out, err = proc.communicate(timeout=5)
            print("[FALLO] el hub temporal no arrancó")
            print(err.decode(errors="replace")[-2000:])
            return 1

        env_doc = {**os.environ, "CORE": core, "ADMIN": admin, "MASTER": MASTER}
        r = subprocess.run(
            [find_bash(), "-euo", "pipefail", "-c", script],
            env=env_doc,
            capture_output=True,
            timeout=120,
        )
        salida = r.stdout.decode(errors="replace")
        errores = r.stderr.decode(errors="replace")
        print("--- salida ---")
        print(salida.strip())
        if errores.strip():
            print("--- stderr ---")
            print(errores.strip())
        if r.returncode != 0:
            print(f"[FALLO] un bloque falló (exit {r.returncode})")
            return 1
        if "aislamiento OK" not in salida or "revocación OK" not in salida:
            print("[FALLO] la secuencia no llegó al final (aislamiento/revocación)")
            return 1
        ok = True
        print(f"[OK] {len(bloques)} bloques ejecutables pasaron contra {core}")
        return 0
    finally:
        try:
            proc.kill()
            proc.communicate(timeout=5)
        except Exception:
            pass
        if ok or True:
            for _ in range(10):
                try:
                    shutil.rmtree(tmp, ignore_errors=False)
                    break
                except Exception:
                    time.sleep(0.3)


if __name__ == "__main__":
    sys.exit(main())
