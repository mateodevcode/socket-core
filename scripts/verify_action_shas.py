#!/usr/bin/env python3
"""Verifica que cada action@SHA de ci.yml coincide con la etiqueta del comentario."""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / ".github/workflows/ci.yml"
PAT = re.compile(r"uses:\s*([\w.-]+/[\w.-]+)@([0-9a-f]{40})\s*#\s*(\S+)")


def ref_sha(repo, ref):
    r = subprocess.run(
        ["git", "ls-remote", f"https://github.com/{repo}", ref],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if r.returncode != 0 or not r.stdout.strip():
        return None
    return r.stdout.split()[0]


def main():
    texto = CI.read_text(encoding="utf-8")
    pares = PAT.findall(texto)
    if not pares:
        print("[FALLO] no se encontraron actions fijadas por SHA")
        return 1
    errores = []
    for repo, sha, tag in pares:
        obtenido = None
        for ref in (f"refs/tags/{tag}", f"refs/heads/{tag}"):
            obtenido = ref_sha(repo, ref)
            if obtenido:
                break
        if obtenido is None:
            errores.append(f"{repo}: no se pudo resolver {tag}")
        elif obtenido != sha:
            errores.append(f"{repo}@{tag}: ci.yml={sha} remoto={obtenido}")
        else:
            print(f"OK {repo}@{tag} = {sha}")
    if errores:
        print("\n[FALLO]")
        for e in errores:
            print(" -", e)
        return 1
    print(f"\n[OK] {len(pares)} actions verificadas")
    return 0


if __name__ == "__main__":
    sys.exit(main())
