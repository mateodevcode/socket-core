#!/usr/bin/env python3
"""Tests de scripts/smoke.py con subprocess.run simulado (sin red ni Docker)."""
import importlib.util
import json
import sys
import unittest
from pathlib import Path
from unittest import mock

AQUI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("smoke", AQUI / "smoke.py")
smoke = importlib.util.module_from_spec(_spec)
sys.modules["smoke"] = smoke
_spec.loader.exec_module(smoke)

MASTER = "master-secreto-de-prueba-32-chars-min"
API_KEY = "sc_live_secreta_de_prueba"


class FakeRun:
    def __init__(self, fail_emit=False):
        self.calls = []
        self.fail_emit = fail_emit

    def __call__(self, argv, input=None, **kw):
        self.calls.append((argv, input or ""))
        cfg = input or ""
        if "DELETE" in cfg and "/admin/keys/" in cfg:
            return mock.Mock(returncode=0, stdout='{"success":true}\n200', stderr="")
        if "POST" in cfg and "/admin/keys" in cfg:
            body = {"success": True, "data": {"id": "kid-1", "api_key": API_KEY}}
            return mock.Mock(returncode=0, stdout=json.dumps(body) + "\n200", stderr="")
        if "/events" in cfg:
            code = 500 if self.fail_emit else 200
            body = {"success": not self.fail_emit, "error": "boom"} if self.fail_emit else {
                "success": True, "data": {"success": True}
            }
            return mock.Mock(returncode=0, stdout=json.dumps(body) + f"\n{code}", stderr="")
        if "POST" in cfg and "/channels" in cfg:
            body = {"success": True, "data": {"name": "smoke:probe"}}
            return mock.Mock(returncode=0, stdout=json.dumps(body) + "\n201", stderr="")
        if "GET" in cfg and "/channels/" in cfg:
            body = {"success": True, "data": {"name": "smoke-"}, "_probe": True}
            # El nombre real lo extrae el smoke del prefijo; devolvemos el del cfg.
            for line in cfg.splitlines():
                if line.startswith("url = "):
                    nombre = line.split("/channels/", 1)[-1].rstrip('"')
                    body["data"]["name"] = nombre
            return mock.Mock(returncode=0, stdout=json.dumps(body) + "\n200", stderr="")
        return mock.Mock(returncode=0, stdout="{}\n200", stderr="")


class TestSmoke(unittest.TestCase):
    def test_flujo_ok_y_secretos_solo_por_stdin(self):
        fake = FakeRun()
        with mock.patch.object(smoke.subprocess, "run", fake):
            rc = smoke.run("https://hub.example", MASTER)
        self.assertEqual(rc, 0)
        # La API key y la MASTER nunca están en argv (solo en el stdin de curl -K -).
        for argv, stdin in fake.calls:
            unido = " ".join(argv)
            self.assertNotIn(MASTER, unido)
            self.assertNotIn(API_KEY, unido)
            self.assertEqual(argv[-2:], ["-K", "-"])
            self.assertIn("header = ", stdin)
        # Hay create key, canal, evento, GET y DELETE final.
        cfg_all = "\n".join(c for _, c in fake.calls)
        self.assertIn("/admin/keys", cfg_all)
        self.assertIn("/channels", cfg_all)
        self.assertIn("/events", cfg_all)
        self.assertIn("DELETE", cfg_all)

    def test_borra_key_en_finally_si_falla_la_emision(self):
        fake = FakeRun(fail_emit=True)
        with mock.patch.object(smoke.subprocess, "run", fake):
            with self.assertRaises(smoke.SmokeError):
                smoke.run("https://hub.example", MASTER)
        self.assertTrue(any("DELETE" in c and "/admin/keys/kid-1" in c for _, c in fake.calls))

    def test_sin_master_no_intenta_nada(self):
        with mock.patch.object(smoke.subprocess, "run", FakeRun()):
            with self.assertRaises(smoke.SmokeError):
                smoke.run("https://hub.example", "")


if __name__ == "__main__":
    unittest.main()
