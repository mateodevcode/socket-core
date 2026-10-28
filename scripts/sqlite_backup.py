#!/usr/bin/env python3
"""Backup/restauración consistente de SQLite usando la API de backup.

Uso:
    python3 scripts/sqlite_backup.py backup  --db DATA.db --out BACKUP.db
    python3 scripts/sqlite_backup.py restore --backup BACKUP.db --db DATA.db

`backup` comprueba `PRAGMA integrity_check` del resultado; si no es "ok" falla.
No usa `tar` en caliente (evita copias inconsistentes con WAL).
"""

import argparse
import os
import sqlite3
import sys


def backup(db_path: str, out_path: str) -> None:
    origen = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    destino = sqlite3.connect(out_path)
    try:
        origen.backup(destino)
    finally:
        origen.close()
        destino.close()
    # Verificación de integridad del backup.
    con = sqlite3.connect(f"file:{out_path}?mode=ro", uri=True)
    try:
        resultado = con.execute("PRAGMA integrity_check").fetchone()[0]
    finally:
        con.close()
    if resultado != "ok":
        raise RuntimeError(f"integrity_check del backup falló: {resultado}")
    if os.name == "posix":
        os.chmod(out_path, 0o600)


def restore(backup_path: str, db_path: str) -> None:
    con = sqlite3.connect(f"file:{backup_path}?mode=ro", uri=True)
    destino = sqlite3.connect(db_path)
    try:
        con.backup(destino)
    finally:
        con.close()
        destino.close()
    if os.name == "posix":
        os.chmod(db_path, 0o600)


def main() -> int:
    ap = argparse.ArgumentParser(description="Backup/restore consistente de SQLite")
    sub = ap.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("backup")
    b.add_argument("--db", required=True)
    b.add_argument("--out", required=True)

    r = sub.add_parser("restore")
    r.add_argument("--backup", required=True)
    r.add_argument("--db", required=True)

    args = ap.parse_args()
    try:
        if args.cmd == "backup":
            backup(args.db, args.out)
            print(f"backup OK: {args.out}")
        else:
            restore(args.backup, args.db)
            print(f"restore OK: {args.db}")
        return 0
    except Exception as e:
        print(f"[ERROR] {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
