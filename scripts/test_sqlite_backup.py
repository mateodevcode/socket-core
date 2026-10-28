#!/usr/bin/env python3
"""Tests de scripts/sqlite_backup.py sobre DBs sintéticas."""
import importlib.util
import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

AQUI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("sqlite_backup", AQUI / "sqlite_backup.py")
sb = importlib.util.module_from_spec(_spec)
sys.modules["sqlite_backup"] = sb
_spec.loader.exec_module(sb)


def crear_db(path, valores):
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
    con.executemany("INSERT INTO t (v) VALUES (?)", [(v,) for v in valores])
    con.commit()
    con.close()


def valores(path):
    con = sqlite3.connect(path)
    try:
        return [r[0] for r in con.execute("SELECT v FROM t ORDER BY id")]
    finally:
        con.close()


def crear_db_wal_con_sidecars(path, valores_db):
    path = Path(path)
    con = sqlite3.connect(path)
    con.execute("PRAGMA journal_mode=WAL")
    con.execute("PRAGMA wal_autocheckpoint=0")
    con.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
    con.executemany("INSERT INTO t (v) VALUES (?)", [(v,) for v in valores_db])
    con.commit()
    snap = path.parent / (path.name + ".snap")
    snap.mkdir()
    for p in path.parent.iterdir():
        if p.is_file() and p.name in (path.name, path.name + "-wal", path.name + "-shm"):
            shutil.copy2(p, snap / p.name)
    con.close()
    for p in snap.iterdir():
        shutil.copy2(p, path.parent / p.name)
    shutil.rmtree(snap)
    return path


class TestBackup(unittest.TestCase):
    def test_roundtrip_e_integridad(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "origen.db")
            bk = str(Path(d) / "backup.db")
            crear_db(db, ["a", "b"])
            sb.backup(db, bk)
            con = sqlite3.connect(bk)
            self.assertEqual(con.execute("PRAGMA integrity_check").fetchone()[0], "ok")
            con.close()
            self.assertEqual(valores(bk), ["a", "b"])

    def test_restauracion(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "origen.db")
            bk = str(Path(d) / "backup.db")
            crear_db(db, ["a", "b"])
            sb.backup(db, bk)
            # Modificamos la DB después del backup.
            con = sqlite3.connect(db)
            con.execute("DELETE FROM t")
            con.commit()
            con.close()
            self.assertEqual(valores(db), [])
            sb.restore(bk, db)
            self.assertEqual(valores(db), ["a", "b"])

    def test_integridad_fallida_no_pasa(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "rota.db")
            Path(db).write_bytes(b"no es sqlite")
            out = str(Path(d) / "out.db")
            with self.assertRaises(Exception):
                sb.backup(db, out)

    def test_restore_con_wal_presente_y_borra_sidecars(self):
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "origen.db"
            crear_db_wal_con_sidecars(db, ["a", "b"])
            self.assertTrue(Path(str(db) + "-wal").is_file())
            bk = str(Path(d) / "backup.db")
            sb.backup(str(db), bk)

            destino = Path(d) / "destino.db"
            crear_db_wal_con_sidecars(destino, ["viejo"])
            self.assertTrue(Path(str(destino) + "-wal").is_file())

            sb.restore(bk, str(destino))

            self.assertEqual(valores(destino), ["a", "b"])
            self.assertFalse(Path(str(destino) + "-wal").exists())
            self.assertFalse(Path(str(destino) + "-shm").exists())
            antes = Path(str(destino) + ".antes-restore")
            self.assertTrue(antes.is_file())
            self.assertEqual(valores(antes), ["viejo"])

    def test_backup_corrupto_no_toca_db(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "actual.db")
            crear_db(db, ["actual"])
            bk = str(Path(d) / "roto.db")
            Path(bk).write_bytes(b"no es sqlite")
            with self.assertRaisesRegex(RuntimeError, "integridad|integrity|corrupt"):
                sb.restore(bk, db)
            self.assertEqual(valores(db), ["actual"])
            self.assertFalse(Path(db + ".antes-restore").exists())

    def test_interrupcion_a_mitad_no_pierde_db(self):
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "actual.db"
            crear_db_wal_con_sidecars(db, ["actual"])
            self.assertTrue(Path(str(db) + "-wal").is_file())
            bk = str(Path(d) / "backup.db")
            sb.backup(str(db), bk)
            with mock.patch.object(sb.os, "replace", side_effect=OSError("interrupción")):
                with self.assertRaises(OSError):
                    sb.restore(bk, str(db))
            self.assertTrue(Path(str(db) + "-wal").is_file())
            self.assertEqual(valores(db), ["actual"])
            self.assertEqual(list(Path(d).glob(".actual.db.restore-*")), [])

    def test_db_en_uso_falla_sin_force(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "actual.db")
            crear_db(db, ["actual"])
            bk = str(Path(d) / "backup.db")
            sb.backup(db, bk)
            con = sqlite3.connect(db)
            con.execute("BEGIN EXCLUSIVE")
            try:
                with self.assertRaisesRegex(RuntimeError, "en uso"):
                    sb.restore(bk, db)
            finally:
                con.rollback()
                con.close()
            self.assertEqual(valores(db), ["actual"])

    def test_force_ignora_la_comprobacion_de_uso(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "actual.db")
            crear_db(db, ["actual"])
            bk = str(Path(d) / "backup.db")
            sb.backup(db, bk)
            with mock.patch.object(sb, "_check_not_in_use", side_effect=RuntimeError("en uso")):
                sb.restore(bk, db, force=True)
            self.assertEqual(valores(db), ["actual"])

    def test_create_requerido_para_destino_nuevo(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "origen.db")
            crear_db(db, ["a"])
            bk = str(Path(d) / "backup.db")
            sb.backup(db, bk)
            nuevo = str(Path(d) / "nuevo.db")
            with self.assertRaisesRegex(RuntimeError, "no existe"):
                sb.restore(bk, nuevo)
            self.assertFalse(Path(nuevo).exists())
            sb.restore(bk, nuevo, create=True)
            self.assertEqual(valores(nuevo), ["a"])



if __name__ == "__main__":
    unittest.main()
