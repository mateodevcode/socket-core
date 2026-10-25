#!/usr/bin/env python3
"""Tests de scripts/sqlite_backup.py sobre DBs sintéticas."""
import importlib.util
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
