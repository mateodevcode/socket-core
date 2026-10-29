#!/usr/bin/env python3
"""Tests de estado.py: sin dominio por defecto y secretos fuera de argv."""
import importlib.util
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

AQUI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("estado", AQUI / "estado.py")
estado = importlib.util.module_from_spec(_spec)
sys.modules["estado"] = estado
_spec.loader.exec_module(estado)


class TestCore(unittest.TestCase):
    def test_sin_dominio_por_defecto(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(estado.default_core(), "")

    def test_lee_socket_public_url(self):
        with mock.patch.dict(os.environ, {"SOCKET_PUBLIC_URL": "https://hub.example"}, clear=True):
            self.assertEqual(estado.default_core(), "https://hub.example")

    def test_fail2ban_file_list_no_vacia_ok(self):
        salida = """Status for the jail: socketcore-auth
|- Filter
|  |- Currently failed: 0
|  |- Total failed:     3
|  `- File list:        /var/log/nginx/ws.seventwo.tech.access.log
`- Actions
   |- Currently banned: 0
   |- Total banned:     0
   `- Banned IP list:
"""
        ok, detalle = estado.evaluar_fail2ban(salida)
        self.assertTrue(ok, detalle)
        self.assertIn("File list", detalle)

    def test_fail2ban_file_list_vacia_falla(self):
        salida = """Status for the jail: socketcore-auth
|- Filter
|  |- Currently failed: 0
|  |- Total failed:     3
|  `- File list:
`- Actions
   |- Currently banned: 0
   |- Total banned:     0
   `- Banned IP list:
"""
        ok, detalle = estado.evaluar_fail2ban(salida)
        self.assertFalse(ok, detalle)
        self.assertIn("File list vacía", detalle)



if __name__ == "__main__":
    unittest.main()
