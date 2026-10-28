#!/usr/bin/env python3
"""Tests de la lógica pura de preflight.py sobre datos/DB sintéticos."""
import datetime
import importlib.util
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

AQUI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("preflight", AQUI / "preflight.py")
pf = importlib.util.module_from_spec(_spec)
sys.modules["preflight"] = pf
_spec.loader.exec_module(pf)


def key(name, prefix, scopes, exp=None, revoked=0, kid=None):
    return {
        "id": kid or name,
        "name": name,
        "channel_prefixes": [prefix],
        "scopes": scopes,
        "expires_at": exp,
        "revoked": revoked,
    }


class TestAnalizar(unittest.TestCase):
    def test_scopes_invalidos_y_duplicados(self):
        keys = [key("bad", "b:*", ["*", "publish", "publish"])]
        rep = pf.analizar(keys, [])
        texto = " ".join(rep["problemas"])
        self.assertIn("scopes inválidos", texto)
        self.assertIn("duplicados", texto)

    def test_prefijos_solapados_incluye_revocadas(self):
        keys = [
            key("a", "x:*", ["publish"]),
            key("b", "x:sub:*", ["publish"], revoked=1),
        ]
        rep = pf.analizar(keys, [])
        self.assertTrue(any("prefijos solapados" in p for p in rep["problemas"]))
        self.assertIn(("a", "b"), pf.prefijos_solapados(keys))

    def test_expiradas_y_muy_lejanas(self):
        ahora = datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc).timestamp()
        keys = [
            key("caducada", "c:*", ["publish"], exp=int(ahora - 10)),
            key("lejana", "l:*", ["publish"], exp=int(ahora + 4000 * 86400)),
        ]
        rep = pf.analizar(keys, [], ahora=ahora)
        self.assertTrue(any("expirada" in p for p in rep["problemas"]))
        self.assertTrue(any("expira muy lejos" in a for a in rep["avisos"]))

    def test_huerfanos_y_cascada_con_db_sintetica(self):
        keys = [key("a", "a:*", ["publish"], kid="ka"), key("b", "b:*", ["publish"], kid="kb")]
        canales = [
            {"id": "c1", "name": "a:chat", "project_id": "ka"},
            {"id": "c2", "name": "a:otro", "project_id": "ka"},
            {"id": "c3", "name": "b:chat", "project_id": "kb"},
            {"id": "c4", "name": "suelto:chat", "project_id": ""},
            {"id": "c5", "name": "fantasma:chat", "project_id": "no-existe"},
        ]
        rep = pf.analizar(keys, canales)
        self.assertEqual(rep["cascada_por_key"]["a"], 2)
        self.assertEqual(rep["cascada_por_key"]["b"], 1)
        self.assertEqual(len(rep["huerfanos"]), 2)
        self.assertTrue(any("huérfanos" in p for p in rep["problemas"]))

    def test_leer_canales_desde_sqlite(self):
        with tempfile.TemporaryDirectory() as d:
            db = str(Path(d) / "sintetica.db")
            con = sqlite3.connect(db)
            con.execute(
                "CREATE TABLE channels (id TEXT PRIMARY KEY, name TEXT, project_id TEXT NOT NULL DEFAULT '')"
            )
            con.execute("INSERT INTO channels VALUES ('1','a:chat','ka')")
            con.execute("INSERT INTO channels VALUES ('2','orphan:chat','')")
            con.commit()
            con.close()
            canales = pf.leer_canales(db)
            self.assertEqual(len(canales), 2)
            self.assertEqual(canales[1]["project_id"], "")

    def test_sin_problemas_exit_ok(self):
        keys = [key("ok", "ok:*", ["publish", "subscribe"])]
        rep = pf.analizar(keys, [{"id": "c", "name": "ok:chat", "project_id": "ok"}])
        self.assertEqual(rep["problemas"], [])


if __name__ == "__main__":
    unittest.main()


class TestDbOnly(unittest.TestCase):
    def test_lee_keys_de_esquema_real(self):
        import sqlite3
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "hub.db"
            con = sqlite3.connect(db)
            con.execute(
                "CREATE TABLE api_keys (id TEXT, name TEXT, channel_prefix TEXT, "
                "scopes TEXT, expires_at INTEGER, revoked INTEGER)"
            )
            con.execute(
                "INSERT INTO api_keys VALUES ('k1','demo','demo:*','publish,subscribe',NULL,0)"
            )
            con.commit()
            con.close()
            keys = pf.leer_keys_db(str(db))
            self.assertEqual(len(keys), 1)
            self.assertEqual(keys[0]["channel_prefixes"], ["demo:*"])
            self.assertEqual(keys[0]["scopes"], ["publish", "subscribe"])


class TestTrustedProxies(unittest.TestCase):
    def test_vacio_avisa(self):
        p = pf.trusted_proxies_problemas("", ["172.18.0.1"])
        self.assertTrue(any("vacío" in x for x in p))

    def test_no_incluye_gateway(self):
        p = pf.trusted_proxies_problemas("10.0.0.5", ["172.18.0.1"])
        self.assertTrue(any("no incluye" in x for x in p))

    def test_incluye_gateway_ok(self):
        p = pf.trusted_proxies_problemas("172.18.0.1", ["172.18.0.1"])
        self.assertEqual(p, [])

    def test_subred_muy_ancha(self):
        p = pf.trusted_proxies_problemas("172.16.0.0/12", ["172.18.0.1"])
        self.assertTrue(any("ancha" in x for x in p))
        p2 = pf.trusted_proxies_problemas("10.0.0.0/8", ["172.18.0.1"])
        self.assertTrue(any("ancha" in x for x in p2))

    def test_cidr_estrecha_ok(self):
        p = pf.trusted_proxies_problemas("172.18.0.0/24", ["172.18.0.1"])
        self.assertEqual(p, [])
