#!/usr/bin/env python3
"""Backup/restauración consistente de SQLite usando la API de backup.

Uso:
    python3 scripts/sqlite_backup.py backup  --db DATA.db --out BACKUP.db
    python3 scripts/sqlite_backup.py restore --backup BACKUP.db --db DATA.db [--force] [--create]

`backup` comprueba `PRAGMA integrity_check` del resultado; si no es "ok" falla.
No usa `tar` en caliente (evita copias inconsistentes con WAL).
"""

import argparse
import os
import sqlite3
import sys
import tempfile


def _connect_ro(path: str) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def _integrity_check(path: str) -> None:
    try:
        con = _connect_ro(path)
        try:
            fila = con.execute("PRAGMA integrity_check").fetchone()
        finally:
            con.close()
    except sqlite3.DatabaseError as e:
        raise RuntimeError(f"integrity_check falló: {e}") from e
    if not fila or fila[0] != "ok":
        raise RuntimeError(f"integrity_check falló: {fila[0] if fila else 'sin resultado'}")


def _check_not_in_use(db_path: str) -> None:
    con = sqlite3.connect(db_path, timeout=0)
    try:
        con.execute("BEGIN EXCLUSIVE")
        con.execute("ROLLBACK")
    except sqlite3.OperationalError as e:
        raise RuntimeError(f"la DB está en uso: {e}") from e
    finally:
        con.close()


def _checkpoint(path: str) -> None:
    con = sqlite3.connect(path)
    try:
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        con.execute("PRAGMA journal_mode=DELETE")
    finally:
        con.close()


def _copy_current(db_path: str, destino: str) -> None:
    origen = _connect_ro(db_path)
    con = sqlite3.connect(destino)
    try:
        origen.backup(con)
    finally:
        origen.close()
        con.close()


def backup(db_path: str, out_path: str) -> None:
    origen = _connect_ro(db_path)
    destino = sqlite3.connect(out_path)
    try:
        origen.backup(destino)
    finally:
        origen.close()
        destino.close()
    _integrity_check(out_path)
    if os.name == "posix":
        os.chmod(out_path, 0o600)


def restore(
    backup_path: str,
    db_path: str,
    force: bool = False,
    create: bool = False,
) -> None:
    if not os.path.isfile(backup_path):
        raise RuntimeError(f"el backup no existe: {backup_path}")

    _integrity_check(backup_path)

    existe = os.path.exists(db_path)
    if not existe and not create:
        raise RuntimeError(f"la DB destino no existe: {db_path} (usa --create)")
    if existe and not force:
        _check_not_in_use(db_path)

    parent = os.path.dirname(os.path.abspath(db_path))
    if not os.path.isdir(parent):
        raise RuntimeError(f"el directorio destino no existe: {parent}")

    fd, tmp = tempfile.mkstemp(
        prefix=f".{os.path.basename(db_path)}.restore-",
        suffix=".db",
        dir=parent,
    )
    os.close(fd)
    sidecars: list[tuple[str, str]] = []
    reemplazada = False
    try:
        origen = _connect_ro(backup_path)
        destino = sqlite3.connect(tmp)
        try:
            origen.backup(destino)
        finally:
            origen.close()
            destino.close()

        _checkpoint(tmp)
        _integrity_check(tmp)

        if existe:
            _copy_current(db_path, db_path + ".antes-restore")

        for sufijo in ("-wal", "-shm"):
            origen_sidecar = db_path + sufijo
            if os.path.exists(origen_sidecar):
                tmp_sidecar = f"{tmp}.sidecar{sufijo}"
                os.rename(origen_sidecar, tmp_sidecar)
                sidecars.append((origen_sidecar, tmp_sidecar))

        for sufijo in ("-wal", "-shm"):
            try:
                os.remove(tmp + sufijo)
            except OSError:
                pass

        os.replace(tmp, db_path)
        tmp = ""
        reemplazada = True

        for _, tmp_sidecar in sidecars:
            try:
                os.remove(tmp_sidecar)
            except OSError:
                pass
        sidecars = []
    except Exception:
        if not reemplazada:
            for origen_sidecar, tmp_sidecar in sidecars:
                if os.path.exists(tmp_sidecar):
                    os.rename(tmp_sidecar, origen_sidecar)
        raise
    finally:
        if tmp:
            for sufijo in ("", "-wal", "-shm"):
                try:
                    os.remove(tmp + sufijo)
                except OSError:
                    pass

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
    r.add_argument("--force", action="store_true")
    r.add_argument("--create", action="store_true")

    args = ap.parse_args()
    try:
        if args.cmd == "backup":
            backup(args.db, args.out)
            print(f"backup OK: {args.out}")
        else:
            restore(args.backup, args.db, force=args.force, create=args.create)
            print(f"restore OK: {args.db}")
        return 0
    except Exception as e:
        print(f"[ERROR] {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
