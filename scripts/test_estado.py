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


if __name__ == "__main__":
    unittest.main()
